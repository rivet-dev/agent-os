//! Engine-neutral registry for the AgentOS-owned WebAssembly host ABI.
//!
//! The generated metadata describes import identity, core signatures,
//! permission availability, semantic handler/codec routing, and the execution
//! constraints shared by the V8 compatibility and native WASM adapters. It
//! contains no engine types and grants no authority by itself.

mod generated;

pub use generated::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn generated_registry_has_the_locked_inventory_shape() {
        assert_eq!(ABI_BINDINGS.len(), 169);
        assert_eq!(CORE_SIGNATURES.len(), 29);
        assert_eq!(ALIAS_BINDINGS.len(), 40);

        let keys = ABI_BINDINGS
            .iter()
            .map(|binding| (binding.module, binding.name))
            .collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), ABI_BINDINGS.len());
        assert!(ABI_BINDINGS.iter().all(|entry| {
            binding(entry.id) == entry && core_signature(entry.signature).id == entry.signature
        }));
    }

    #[test]
    fn generated_registry_has_the_locked_permission_counts() {
        let count = |tier| {
            ABI_BINDINGS
                .iter()
                .filter(|binding| binding.permission_tiers.contains(tier))
                .count()
        };
        assert_eq!(count(PermissionTier::Isolated), 112);
        assert_eq!(count(PermissionTier::ReadOnly), 121);
        assert_eq!(count(PermissionTier::ReadWrite), 121);
        assert_eq!(count(PermissionTier::Full), 169);

        let count_aliases = |tier| {
            ALIAS_BINDINGS
                .iter()
                .filter(|binding| binding.permission_tiers.contains(tier))
                .count()
        };
        assert_eq!(count_aliases(PermissionTier::Isolated), 40);
        assert_eq!(count_aliases(PermissionTier::ReadOnly), 40);
        assert_eq!(count_aliases(PermissionTier::ReadWrite), 40);
        assert_eq!(count_aliases(PermissionTier::Full), 40);
    }
}
