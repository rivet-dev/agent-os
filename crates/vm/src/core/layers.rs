use crate::core::SidecarCoreError;
use agentos_vm_kernel::root_fs::{
    FilesystemEntry, FilesystemEntryKind, RootFileSystem,
    RootFilesystemDescriptor as KernelRootFilesystemDescriptor,
    RootFilesystemMode as KernelRootFilesystemMode, RootFilesystemSnapshot,
};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_VM_LAYERS: usize = 256;

#[derive(Debug)]
pub struct VmLayerStore {
    next_layer_id: u64,
    layers: BTreeMap<String, VmLayer>,
}

impl Default for VmLayerStore {
    fn default() -> Self {
        Self {
            next_layer_id: 1,
            layers: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
enum VmLayer {
    Writable(RootFileSystem),
    Snapshot(RootFilesystemSnapshot),
    Overlay(VmOverlayLayer),
}

#[derive(Debug, Clone)]
struct VmOverlayLayer {
    mode: KernelRootFilesystemMode,
    upper_layer_id: Option<String>,
    lower_layer_ids: Vec<String>,
}

impl VmLayerStore {
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    pub fn create_writable_layer(&mut self) -> Result<String, SidecarCoreError> {
        self.ensure_layer_capacity()?;
        let filesystem = new_writable_layer()?;
        let layer_id = self.allocate_layer_id()?;
        self.layers
            .insert(layer_id.clone(), VmLayer::Writable(filesystem));
        Ok(layer_id)
    }

    pub fn seal_layer(&mut self, layer_id: &str) -> Result<String, SidecarCoreError> {
        let snapshot = match self.layers.get_mut(layer_id) {
            Some(VmLayer::Writable(filesystem)) => filesystem
                .snapshot()
                .map_err(|error| SidecarCoreError::new(format!("seal layer: {error}")))?,
            Some(VmLayer::Snapshot(_)) | Some(VmLayer::Overlay(_)) => {
                return Err(SidecarCoreError::new(format!(
                    "layer {layer_id} is not writable"
                )));
            }
            None => return Err(SidecarCoreError::new(format!("unknown layer: {layer_id}"))),
        };
        let sealed_layer_id = self.allocate_layer_id()?;
        match self
            .layers
            .remove(layer_id)
            .expect("layer should still exist after snapshot")
        {
            VmLayer::Writable(_) => {}
            VmLayer::Snapshot(_) | VmLayer::Overlay(_) => {
                return Err(SidecarCoreError::new(format!(
                    "layer {layer_id} is not writable"
                )));
            }
        }
        self.layers
            .insert(sealed_layer_id.clone(), VmLayer::Snapshot(snapshot));
        Ok(sealed_layer_id)
    }

    pub fn import_snapshot(
        &mut self,
        snapshot: RootFilesystemSnapshot,
    ) -> Result<String, SidecarCoreError> {
        self.ensure_layer_capacity()?;
        let layer_id = self.allocate_layer_id()?;
        self.layers
            .insert(layer_id.clone(), VmLayer::Snapshot(snapshot));
        Ok(layer_id)
    }

    pub fn export_snapshot(
        &mut self,
        layer_id: &str,
    ) -> Result<RootFilesystemSnapshot, SidecarCoreError> {
        materialize_vm_layer_snapshot(self, layer_id)
    }

    pub fn create_overlay_layer(
        &mut self,
        mode: KernelRootFilesystemMode,
        upper_layer_id: Option<String>,
        lower_layer_ids: Vec<String>,
    ) -> Result<String, SidecarCoreError> {
        self.ensure_layer_capacity()?;
        for layer_id in &lower_layer_ids {
            if !self.layers.contains_key(layer_id) {
                return Err(SidecarCoreError::new(format!(
                    "unknown lower layer: {layer_id}"
                )));
            }
        }
        if let Some(layer_id) = upper_layer_id.as_ref() {
            if !self.layers.contains_key(layer_id) {
                return Err(SidecarCoreError::new(format!(
                    "unknown upper layer: {layer_id}"
                )));
            }
        }

        let layer_id = self.allocate_layer_id()?;
        self.layers.insert(
            layer_id.clone(),
            VmLayer::Overlay(VmOverlayLayer {
                mode,
                upper_layer_id,
                lower_layer_ids,
            }),
        );
        Ok(layer_id)
    }

    fn ensure_layer_capacity(&self) -> Result<(), SidecarCoreError> {
        if self.layers.len() >= MAX_VM_LAYERS {
            return Err(SidecarCoreError::new(format!(
                "VM layer limit exceeded: limit is {MAX_VM_LAYERS}"
            )));
        }
        Ok(())
    }

    fn allocate_layer_id(&mut self) -> Result<String, SidecarCoreError> {
        let layer_id = format!("layer-{}", self.next_layer_id);
        self.next_layer_id = self
            .next_layer_id
            .checked_add(1)
            .ok_or_else(|| SidecarCoreError::new("VM layer id overflow"))?;
        Ok(layer_id)
    }
}

fn new_writable_layer() -> Result<RootFileSystem, SidecarCoreError> {
    RootFileSystem::from_descriptor(KernelRootFilesystemDescriptor {
        mode: KernelRootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: Vec::new(),
        bootstrap_entries: Vec::new(),
    })
    .map_err(|error| SidecarCoreError::new(format!("create writable layer: {error}")))
}

fn materialize_vm_layer_snapshot(
    layers: &mut VmLayerStore,
    layer_id: &str,
) -> Result<RootFilesystemSnapshot, SidecarCoreError> {
    materialize_vm_layer_snapshot_inner(layers, layer_id, &mut BTreeSet::new())
}

fn materialize_vm_layer_snapshot_inner(
    layers: &mut VmLayerStore,
    layer_id: &str,
    active: &mut BTreeSet<String>,
) -> Result<RootFilesystemSnapshot, SidecarCoreError> {
    if !active.insert(layer_id.to_owned()) {
        return Err(SidecarCoreError::new(format!(
            "layer graph cycle detected at {layer_id}"
        )));
    }

    let result = if let Some(VmLayer::Snapshot(snapshot)) = layers.layers.get(layer_id) {
        Ok(snapshot.clone())
    } else if let Some(VmLayer::Overlay(overlay)) = layers.layers.get(layer_id) {
        let overlay = overlay.clone();
        let lowers = overlay
            .lower_layer_ids
            .iter()
            .map(|lower_id| materialize_vm_layer_snapshot_inner(layers, lower_id, active))
            .collect::<Result<Vec<_>, _>>()?;
        let bootstrap_entries = match overlay.upper_layer_id.as_deref() {
            Some(upper_layer_id) => dedupe_overlay_bootstrap_entries(
                &lowers,
                materialize_vm_layer_snapshot_inner(layers, upper_layer_id, active)?.entries,
            ),
            None => Vec::new(),
        };
        let mut root = RootFileSystem::from_descriptor(KernelRootFilesystemDescriptor {
            mode: overlay.mode,
            disable_default_base_layer: true,
            lowers,
            bootstrap_entries,
        })
        .map_err(|error| SidecarCoreError::new(format!("materialize overlay layer: {error}")))?;
        root.snapshot()
            .map_err(|error| SidecarCoreError::new(format!("snapshot overlay layer: {error}")))
    } else if let Some(VmLayer::Writable(filesystem)) = layers.layers.get_mut(layer_id) {
        filesystem
            .snapshot()
            .map_err(|error| SidecarCoreError::new(format!("snapshot writable layer: {error}")))
    } else {
        Err(SidecarCoreError::new(format!("unknown layer: {layer_id}")))
    };

    active.remove(layer_id);
    result
}

fn dedupe_overlay_bootstrap_entries(
    lowers: &[RootFilesystemSnapshot],
    upper_entries: Vec<FilesystemEntry>,
) -> Vec<FilesystemEntry> {
    let mut lower_paths = lowers
        .iter()
        .flat_map(|snapshot| snapshot.entries.iter().map(|entry| entry.path.clone()))
        .collect::<BTreeSet<_>>();

    upper_entries
        .into_iter()
        .filter(|entry| {
            if lower_paths.contains(&entry.path)
                && matches!(entry.kind, FilesystemEntryKind::Directory)
            {
                return false;
            }
            lower_paths.insert(entry.path.clone());
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_exports_and_overlays_snapshots() {
        let mut layers = VmLayerStore::default();
        let lower = layers
            .import_snapshot(RootFilesystemSnapshot {
                entries: vec![FilesystemEntry::file("/value.txt", b"lower".to_vec())],
            })
            .expect("import lower");
        let upper = layers
            .import_snapshot(RootFilesystemSnapshot {
                entries: vec![FilesystemEntry::file("/extra.txt", b"upper".to_vec())],
            })
            .expect("import upper");
        let overlay = layers
            .create_overlay_layer(
                KernelRootFilesystemMode::Ephemeral,
                Some(upper),
                vec![lower],
            )
            .expect("create overlay");

        let snapshot = layers.export_snapshot(&overlay).expect("export overlay");
        assert!(snapshot
            .entries
            .iter()
            .any(|entry| entry.path == "/value.txt"));
        assert!(snapshot
            .entries
            .iter()
            .any(|entry| entry.path == "/extra.txt"));
    }

    #[test]
    fn sealing_non_writable_layer_is_rejected() {
        let mut layers = VmLayerStore::default();
        let layer = layers
            .import_snapshot(RootFilesystemSnapshot {
                entries: Vec::new(),
            })
            .expect("import snapshot");

        let error = layers
            .seal_layer(&layer)
            .expect_err("snapshot layer should not seal");
        assert!(error.to_string().contains("is not writable"));
    }
}
