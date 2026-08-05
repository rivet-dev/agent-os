pub mod generated;
#[cfg(not(target_arch = "wasm32"))]
pub mod pack;
pub mod versioned;

use std::ops::Range;

use crate::posix::vfs::{VfsError, VfsResult};

pub const AOSPKG_MAGIC: [u8; 4] = [0x89, b'A', b'O', b'S'];
pub const AOSPKG_FORMAT_VERSION: u16 = 1;
pub const AOSPKG_HEADER_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AospkgHeader {
    pub manifest_len: u32,
    pub index_len: u32,
    pub manifest: Range<usize>,
    pub index: Range<usize>,
    pub mount: Range<usize>,
}

/// Parse the 16-byte `.aospkg` header and return checked chunk ranges.
///
/// The container is startup-critical and deliberately simple:
/// header + chunk1 manifest + chunk2 mount index + chunk3 uncompressed mount.tar.
/// Projection decodes only chunk1; `TarFileSystem::open` decodes chunk2 and
/// serves file reads from chunk3 offsets. Do not reintroduce tar header scans,
/// JSON manifest parsing, or whole-archive reads on these paths.
pub fn parse_aospkg_header(bytes: &[u8]) -> VfsResult<AospkgHeader> {
    parse_aospkg_header_from_prefix(bytes, bytes.len())
}

pub fn parse_aospkg_header_from_prefix(header: &[u8], file_len: usize) -> VfsResult<AospkgHeader> {
    if header.len() < AOSPKG_HEADER_LEN {
        return Err(VfsError::new(
            "EINVAL",
            format!(
                ".aospkg header truncated: {} bytes < {AOSPKG_HEADER_LEN} bytes",
                header.len()
            ),
        ));
    }

    if file_len < AOSPKG_HEADER_LEN {
        return Err(VfsError::new(
            "EINVAL",
            format!(".aospkg file truncated: {file_len} bytes < {AOSPKG_HEADER_LEN} bytes"),
        ));
    }

    if header[0..4] != AOSPKG_MAGIC {
        return Err(VfsError::new("EINVAL", "invalid .aospkg magic"));
    }
    let format_version = u16::from_le_bytes([header[4], header[5]]);
    if format_version != AOSPKG_FORMAT_VERSION {
        return Err(VfsError::new(
            "EINVAL",
            format!("unsupported .aospkg format version: {format_version}"),
        ));
    }
    let flags = u16::from_le_bytes([header[6], header[7]]);
    if flags != 0 {
        return Err(VfsError::new(
            "EINVAL",
            format!("unsupported .aospkg flags: {flags}"),
        ));
    }

    let manifest_len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let index_len = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    let manifest_len_usize = usize::try_from(manifest_len).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(".aospkg manifest length overflows usize: {manifest_len} bytes"),
        )
    })?;
    let index_len_usize = usize::try_from(index_len).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(".aospkg index length overflows usize: {index_len} bytes"),
        )
    })?;
    let index_start = AOSPKG_HEADER_LEN
        .checked_add(manifest_len_usize)
        .ok_or_else(|| VfsError::new("EOVERFLOW", ".aospkg manifest range overflows usize"))?;
    let mount_start = index_start
        .checked_add(index_len_usize)
        .ok_or_else(|| VfsError::new("EOVERFLOW", ".aospkg index range overflows usize"))?;
    if mount_start > file_len {
        return Err(VfsError::new(
            "EINVAL",
            format!(
                ".aospkg chunks exceed file size: header {AOSPKG_HEADER_LEN} bytes + manifest {manifest_len} bytes + index {index_len} bytes > {file_len} bytes"
            ),
        ));
    }

    Ok(AospkgHeader {
        manifest_len,
        index_len,
        manifest: AOSPKG_HEADER_LEN..index_start,
        index: index_start..mount_start,
        mount: mount_start..file_len,
    })
}

pub fn encode_aospkg_header(manifest_len: usize, index_len: usize) -> VfsResult<[u8; 16]> {
    let manifest_len = u32::try_from(manifest_len).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(".aospkg manifest chunk too large: {manifest_len} bytes > u32::MAX bytes"),
        )
    })?;
    let index_len = u32::try_from(index_len).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(".aospkg index chunk too large: {index_len} bytes > u32::MAX bytes"),
        )
    })?;
    let mut header = [0u8; AOSPKG_HEADER_LEN];
    header[0..4].copy_from_slice(&AOSPKG_MAGIC);
    header[4..6].copy_from_slice(&AOSPKG_FORMAT_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&0u16.to_le_bytes());
    header[8..12].copy_from_slice(&manifest_len.to_le_bytes());
    header[12..16].copy_from_slice(&index_len.to_le_bytes());
    Ok(header)
}

/// Read and decode only the chunk1 `PackageManifest` from a `.aospkg` file:
/// 16-byte header, seek, decode. This is the startup-critical projection read —
/// it must never parse tar headers, decode chunk2, or touch chunk3. Shared by
/// every host-side consumer (sidecar projection, actor plugin) so container
/// framing has exactly one implementation.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_manifest_chunk_from_file(
    path: &std::path::Path,
) -> VfsResult<generated::v1::PackageManifest> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)
        .map_err(|e| VfsError::new("EIO", format!("open {}: {e}", path.display())))?;
    let file_len = file
        .metadata()
        .map_err(|e| VfsError::new("EIO", format!("stat {}: {e}", path.display())))?
        .len();
    let file_len = usize::try_from(file_len).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(
                "{} is too large to address on this platform",
                path.display()
            ),
        )
    })?;
    let mut header = [0u8; AOSPKG_HEADER_LEN];
    file.read_exact(&mut header)
        .map_err(|e| VfsError::new("EIO", format!("read {} header: {e}", path.display())))?;
    let parsed = parse_aospkg_header_from_prefix(&header, file_len)?;
    let mut manifest = vec![0u8; parsed.manifest.len()];
    file.seek(SeekFrom::Start(parsed.manifest.start as u64))
        .map_err(|e| VfsError::new("EIO", format!("seek {} manifest: {e}", path.display())))?;
    file.read_exact(&mut manifest)
        .map_err(|e| VfsError::new("EIO", format!("read {} manifest: {e}", path.display())))?;
    versioned::decode_package_manifest(&manifest).map_err(|error| {
        VfsError::new(
            "EINVAL",
            format!("decode package manifest in {}: {error}", path.display()),
        )
    })
}

pub fn validate_mount_range(
    header: &AospkgHeader,
    offset: u64,
    size: u64,
) -> VfsResult<Range<usize>> {
    let mount_base = u64::try_from(header.mount.start).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(
                ".aospkg mount base overflows u64: {} bytes",
                header.mount.start
            ),
        )
    })?;
    let absolute_start = mount_base
        .checked_add(offset)
        .ok_or_else(|| VfsError::new("EOVERFLOW", ".aospkg member start overflows u64"))?;
    let absolute_end = absolute_start
        .checked_add(size)
        .ok_or_else(|| VfsError::new("EOVERFLOW", ".aospkg member end overflows u64"))?;
    let file_len = u64::try_from(header.mount.end).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(
                ".aospkg file length overflows u64: {} bytes",
                header.mount.end
            ),
        )
    })?;
    if absolute_end > file_len {
        return Err(VfsError::new(
            "EIO",
            format!(
                ".aospkg member range exceeds file size: mountBase {mount_base} bytes + offset {offset} bytes + size {size} bytes > {file_len} bytes"
            ),
        ));
    }
    let start = usize::try_from(absolute_start).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(".aospkg member start overflows usize: {absolute_start} bytes"),
        )
    })?;
    let end = usize::try_from(absolute_end).map_err(|_| {
        VfsError::new(
            "EOVERFLOW",
            format!(".aospkg member end overflows usize: {absolute_end} bytes"),
        )
    })?;
    Ok(start..end)
}
