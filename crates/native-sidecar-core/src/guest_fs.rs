use crate::SidecarCoreError;
use agentos_kernel::kernel::KernelVm;
use agentos_kernel::vfs::{VirtualFileSystem, VirtualStat};
use agentos_sidecar_protocol::protocol::{
    GuestDirEntry, GuestFilesystemCallRequest, GuestFilesystemOperation,
    GuestFilesystemResultResponse, GuestFilesystemStat, RootFilesystemEntryEncoding,
};
use base64::Engine;

pub fn handle_guest_filesystem_call<F>(
    kernel: &mut KernelVm<F>,
    payload: GuestFilesystemCallRequest,
) -> Result<GuestFilesystemResultResponse, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let response = match payload.operation {
        GuestFilesystemOperation::ReadFile => {
            let bytes = kernel.read_file(&payload.path).map_err(kernel_error)?;
            let (content, encoding) = encode_guest_filesystem_content(bytes);
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: Some(content),
                encoding: Some(encoding),
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::Pread => {
            let offset = payload
                .offset
                .ok_or_else(|| SidecarCoreError::new("guest filesystem pread requires offset"))?;
            let len = payload
                .len
                .ok_or_else(|| SidecarCoreError::new("guest filesystem pread requires len"))?;
            let length = usize::try_from(len).map_err(|_| {
                SidecarCoreError::new("guest filesystem pread len must fit within usize")
            })?;
            let bytes = kernel
                .pread_file(&payload.path, offset, length)
                .map_err(kernel_error)?;
            let (content, encoding) = encode_guest_filesystem_content(bytes);
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: Some(content),
                encoding: Some(encoding),
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::WriteFile => {
            let bytes = decode_guest_filesystem_content(
                &payload.path,
                payload.content.as_deref(),
                payload.encoding,
            )?;
            kernel
                .write_file(&payload.path, bytes)
                .map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::Pwrite => {
            let offset = payload
                .offset
                .ok_or_else(|| SidecarCoreError::new("guest filesystem pwrite requires offset"))?;
            let bytes = decode_guest_filesystem_content(
                &payload.path,
                payload.content.as_deref(),
                payload.encoding,
            )?;
            kernel
                .pwrite_file(&payload.path, offset, bytes)
                .map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::CreateDir => {
            kernel.create_dir(&payload.path).map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::Mkdir => {
            kernel
                .mkdir(&payload.path, payload.recursive)
                .map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::Exists => GuestFilesystemResultResponse {
            operation: payload.operation,
            path: payload.path.clone(),
            content: None,
            encoding: None,
            entries: None,
            stat: None,
            exists: Some(kernel.exists(&payload.path).map_err(kernel_error)?),
            target: None,
        },
        GuestFilesystemOperation::Stat => GuestFilesystemResultResponse {
            operation: payload.operation,
            path: payload.path.clone(),
            content: None,
            encoding: None,
            entries: None,
            stat: Some(guest_filesystem_stat(
                kernel.stat(&payload.path).map_err(kernel_error)?,
            )),
            exists: None,
            target: None,
        },
        GuestFilesystemOperation::Lstat => GuestFilesystemResultResponse {
            operation: payload.operation,
            path: payload.path.clone(),
            content: None,
            encoding: None,
            entries: None,
            stat: Some(guest_filesystem_stat(
                kernel.lstat(&payload.path).map_err(kernel_error)?,
            )),
            exists: None,
            target: None,
        },
        GuestFilesystemOperation::ReadDir => GuestFilesystemResultResponse {
            operation: payload.operation,
            path: payload.path.clone(),
            content: None,
            encoding: None,
            entries: Some(
                kernel
                    .read_dir_with_types(&payload.path)
                    .map_err(kernel_error)?
                    .into_iter()
                    .map(|entry| GuestDirEntry {
                        path: if payload.path == "/" {
                            format!("/{}", entry.name)
                        } else {
                            format!("{}/{}", payload.path, entry.name)
                        },
                        name: entry.name,
                        is_directory: entry.is_directory,
                        is_symbolic_link: entry.is_symbolic_link,
                        size: 0,
                    })
                    .collect(),
            ),
            stat: None,
            exists: None,
            target: None,
        },
        GuestFilesystemOperation::ReadDirRecursive => {
            let max_depth = payload
                .max_depth
                .map(|depth| {
                    usize::try_from(depth).map_err(|_| {
                        SidecarCoreError::new(
                            "guest filesystem read_dir_recursive max_depth must fit within usize",
                        )
                    })
                })
                .transpose()?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: Some(
                    kernel
                        .read_dir_recursive(&payload.path, max_depth)
                        .map_err(kernel_error)?
                        .into_iter()
                        .map(|entry| GuestDirEntry {
                            name: entry
                                .path
                                .rsplit('/')
                                .next()
                                .unwrap_or(entry.path.as_str())
                                .to_owned(),
                            path: entry.path,
                            is_directory: entry.is_directory,
                            is_symbolic_link: entry.is_symbolic_link,
                            size: entry.size,
                        })
                        .collect(),
                ),
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::RemoveFile => {
            kernel.remove_file(&payload.path).map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::RemoveDir => {
            kernel.remove_dir(&payload.path).map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::Remove => {
            kernel
                .remove_path(&payload.path, payload.recursive)
                .map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::Copy => {
            let destination = payload.destination_path.ok_or_else(|| {
                SidecarCoreError::new("guest filesystem copy requires a destination_path")
            })?;
            kernel
                .copy_path(&payload.path, &destination, payload.recursive)
                .map_err(kernel_error)?;
            targeted_guest_filesystem_response(payload.operation, payload.path, destination)
        }
        GuestFilesystemOperation::Move => {
            let destination = payload.destination_path.ok_or_else(|| {
                SidecarCoreError::new("guest filesystem move requires a destination_path")
            })?;
            kernel
                .move_path(&payload.path, &destination)
                .map_err(kernel_error)?;
            targeted_guest_filesystem_response(payload.operation, payload.path, destination)
        }
        GuestFilesystemOperation::Rename => {
            let destination = payload.destination_path.ok_or_else(|| {
                SidecarCoreError::new("guest filesystem rename requires a destination_path")
            })?;
            kernel
                .rename(&payload.path, &destination)
                .map_err(kernel_error)?;
            targeted_guest_filesystem_response(payload.operation, payload.path, destination)
        }
        GuestFilesystemOperation::Realpath => targeted_guest_filesystem_response(
            payload.operation,
            payload.path.clone(),
            kernel.realpath(&payload.path).map_err(kernel_error)?,
        ),
        GuestFilesystemOperation::Symlink => {
            let target = payload.target.ok_or_else(|| {
                SidecarCoreError::new("guest filesystem symlink requires a target")
            })?;
            kernel
                .symlink(&target, &payload.path)
                .map_err(kernel_error)?;
            targeted_guest_filesystem_response(payload.operation, payload.path, target)
        }
        GuestFilesystemOperation::ReadLink => targeted_guest_filesystem_response(
            payload.operation,
            payload.path.clone(),
            kernel.read_link(&payload.path).map_err(kernel_error)?,
        ),
        GuestFilesystemOperation::Link => {
            let destination = payload.destination_path.ok_or_else(|| {
                SidecarCoreError::new("guest filesystem link requires a destination_path")
            })?;
            kernel
                .link(&payload.path, &destination)
                .map_err(kernel_error)?;
            targeted_guest_filesystem_response(payload.operation, payload.path, destination)
        }
        GuestFilesystemOperation::Chmod => {
            let mode = payload
                .mode
                .ok_or_else(|| SidecarCoreError::new("guest filesystem chmod requires a mode"))?;
            kernel.chmod(&payload.path, mode).map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::Chown => {
            let uid = payload
                .uid
                .ok_or_else(|| SidecarCoreError::new("guest filesystem chown requires a uid"))?;
            let gid = payload
                .gid
                .ok_or_else(|| SidecarCoreError::new("guest filesystem chown requires a gid"))?;
            kernel
                .chown(&payload.path, uid, gid)
                .map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::Utimes => {
            let atime_ms = payload.atime_ms.ok_or_else(|| {
                SidecarCoreError::new("guest filesystem utimes requires atime_ms")
            })?;
            let mtime_ms = payload.mtime_ms.ok_or_else(|| {
                SidecarCoreError::new("guest filesystem utimes requires mtime_ms")
            })?;
            kernel
                .utimes(&payload.path, atime_ms, mtime_ms)
                .map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
        GuestFilesystemOperation::Truncate => {
            let len = payload
                .len
                .ok_or_else(|| SidecarCoreError::new("guest filesystem truncate requires len"))?;
            kernel.truncate(&payload.path, len).map_err(kernel_error)?;
            empty_guest_filesystem_response(payload.operation, payload.path)
        }
    };

    Ok(response)
}

pub fn empty_guest_filesystem_response(
    operation: GuestFilesystemOperation,
    path: String,
) -> GuestFilesystemResultResponse {
    GuestFilesystemResultResponse {
        operation,
        path,
        content: None,
        encoding: None,
        entries: None,
        stat: None,
        exists: None,
        target: None,
    }
}

pub fn targeted_guest_filesystem_response(
    operation: GuestFilesystemOperation,
    path: String,
    target: String,
) -> GuestFilesystemResultResponse {
    GuestFilesystemResultResponse {
        target: Some(target),
        ..empty_guest_filesystem_response(operation, path)
    }
}

pub fn encode_guest_filesystem_content(content: Vec<u8>) -> (String, RootFilesystemEntryEncoding) {
    match String::from_utf8(content) {
        Ok(text) => (text, RootFilesystemEntryEncoding::Utf8),
        Err(error) => (
            base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
            RootFilesystemEntryEncoding::Base64,
        ),
    }
}

pub fn decode_guest_filesystem_content(
    path: &str,
    content: Option<&str>,
    encoding: Option<RootFilesystemEntryEncoding>,
) -> Result<Vec<u8>, SidecarCoreError> {
    let content = content.ok_or_else(|| {
        SidecarCoreError::new(format!(
            "guest filesystem write_file for {path} requires content"
        ))
    })?;

    match encoding.unwrap_or(RootFilesystemEntryEncoding::Utf8) {
        RootFilesystemEntryEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        RootFilesystemEntryEncoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|error| {
                SidecarCoreError::new(format!(
                    "invalid base64 guest filesystem content for {path}: {error}"
                ))
            }),
    }
}

pub fn guest_filesystem_stat(stat: VirtualStat) -> GuestFilesystemStat {
    GuestFilesystemStat {
        mode: stat.mode,
        size: stat.size,
        blocks: stat.blocks,
        dev: stat.dev,
        rdev: stat.rdev,
        is_directory: stat.is_directory,
        is_symbolic_link: stat.is_symbolic_link,
        atime_ms: stat.atime_ms,
        mtime_ms: stat.mtime_ms,
        ctime_ms: stat.ctime_ms,
        birthtime_ms: stat.birthtime_ms,
        ino: stat.ino,
        nlink: stat.nlink,
        uid: stat.uid,
        gid: stat.gid,
    }
}

fn kernel_error(error: agentos_kernel::kernel::KernelError) -> SidecarCoreError {
    SidecarCoreError::typed(error.code(), error.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_kernel::kernel::{KernelVm, KernelVmConfig};
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn test_kernel() -> KernelVm<MemoryFileSystem> {
        let mut config = KernelVmConfig::new("guest-fs-test");
        config.permissions = Permissions::allow_all();
        KernelVm::new(MemoryFileSystem::new(), config)
    }

    fn request(operation: GuestFilesystemOperation, path: &str) -> GuestFilesystemCallRequest {
        GuestFilesystemCallRequest {
            operation,
            path: path.to_string(),
            destination_path: None,
            target: None,
            content: None,
            encoding: None,
            recursive: false,
            max_depth: None,
            mode: None,
            uid: None,
            gid: None,
            atime_ms: None,
            mtime_ms: None,
            len: None,
            offset: None,
        }
    }

    #[test]
    fn handles_kernel_backed_guest_filesystem_round_trip() {
        let mut kernel = test_kernel();
        let mut mkdir = request(GuestFilesystemOperation::Mkdir, "/tmp");
        mkdir.recursive = true;
        handle_guest_filesystem_call(&mut kernel, mkdir).unwrap();

        let mut write = request(GuestFilesystemOperation::WriteFile, "/tmp/blob.bin");
        write.content = Some(String::from("//4A"));
        write.encoding = Some(RootFilesystemEntryEncoding::Base64);
        handle_guest_filesystem_call(&mut kernel, write).unwrap();

        let read = handle_guest_filesystem_call(
            &mut kernel,
            request(GuestFilesystemOperation::ReadFile, "/tmp/blob.bin"),
        )
        .unwrap();
        assert_eq!(read.content.as_deref(), Some("//4A"));
        assert_eq!(read.encoding, Some(RootFilesystemEntryEncoding::Base64));

        let stat = handle_guest_filesystem_call(
            &mut kernel,
            request(GuestFilesystemOperation::Stat, "/tmp/blob.bin"),
        )
        .unwrap();
        let stat = stat.stat.expect("stat response");
        assert_eq!(stat.size, 3);
        assert!(!stat.is_directory);
    }

    #[test]
    fn reports_required_guest_filesystem_fields() {
        let mut kernel = test_kernel();
        let error = handle_guest_filesystem_call(
            &mut kernel,
            request(GuestFilesystemOperation::Pread, "/missing"),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "guest filesystem pread requires offset");

        let error = handle_guest_filesystem_call(
            &mut kernel,
            request(GuestFilesystemOperation::Pwrite, "/missing"),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "guest filesystem pwrite requires offset");
    }

    #[test]
    fn preserves_kernel_errno_separately_from_the_diagnostic() {
        let mut kernel = test_kernel();
        let error = handle_guest_filesystem_call(
            &mut kernel,
            request(GuestFilesystemOperation::ReadFile, "/missing"),
        )
        .unwrap_err();
        assert_eq!(error.code(), Some("ENOENT"));
        assert!(!error.message().starts_with("ENOENT:"));
        assert!(error.to_string().starts_with("ENOENT:"));
    }

    #[test]
    fn pwrite_overwrites_in_place_without_truncating_the_file() {
        let mut kernel = test_kernel();
        let mut write = request(GuestFilesystemOperation::WriteFile, "/seek.txt");
        write.content = Some(String::from("abcd"));
        write.encoding = Some(RootFilesystemEntryEncoding::Utf8);
        handle_guest_filesystem_call(&mut kernel, write).unwrap();

        // Overwrite "cd" with "XY" at offset 2; the rest of the file ("ab")
        // must survive. The lossy client-side read-modify-write this replaces
        // would have discarded "ab" whenever the readback failed.
        let mut pwrite = request(GuestFilesystemOperation::Pwrite, "/seek.txt");
        pwrite.content = Some(String::from("XY"));
        pwrite.encoding = Some(RootFilesystemEntryEncoding::Utf8);
        pwrite.offset = Some(2);
        handle_guest_filesystem_call(&mut kernel, pwrite).unwrap();

        let read = handle_guest_filesystem_call(
            &mut kernel,
            request(GuestFilesystemOperation::ReadFile, "/seek.txt"),
        )
        .unwrap();
        assert_eq!(read.content.as_deref(), Some("abXY"));
    }

    #[test]
    fn pwrite_past_end_grows_and_zero_fills_the_hole() {
        let mut kernel = test_kernel();
        let mut write = request(GuestFilesystemOperation::WriteFile, "/hole.bin");
        write.content = Some(String::from("ab"));
        write.encoding = Some(RootFilesystemEntryEncoding::Utf8);
        handle_guest_filesystem_call(&mut kernel, write).unwrap();

        let mut pwrite = request(GuestFilesystemOperation::Pwrite, "/hole.bin");
        pwrite.content = Some(String::from("Z"));
        pwrite.encoding = Some(RootFilesystemEntryEncoding::Utf8);
        pwrite.offset = Some(4);
        handle_guest_filesystem_call(&mut kernel, pwrite).unwrap();

        let read = handle_guest_filesystem_call(
            &mut kernel,
            request(GuestFilesystemOperation::ReadFile, "/hole.bin"),
        )
        .unwrap();
        // "ab" + zero-fill(2) + "Z" -> bytes [97,98,0,0,90].
        let decoded =
            decode_guest_filesystem_content("/hole.bin", read.content.as_deref(), read.encoding)
                .unwrap();
        assert_eq!(decoded, vec![97u8, 98, 0, 0, 90]);
    }

    #[test]
    fn read_dir_returns_typed_entries_in_one_call() {
        let mut kernel = test_kernel();
        let mut mkdir = request(GuestFilesystemOperation::Mkdir, "/d");
        mkdir.recursive = true;
        handle_guest_filesystem_call(&mut kernel, mkdir).unwrap();
        let mut subdir = request(GuestFilesystemOperation::Mkdir, "/d/sub");
        subdir.recursive = true;
        handle_guest_filesystem_call(&mut kernel, subdir).unwrap();
        let mut write = request(GuestFilesystemOperation::WriteFile, "/d/file.txt");
        write.content = Some(String::from("hi"));
        write.encoding = Some(RootFilesystemEntryEncoding::Utf8);
        handle_guest_filesystem_call(&mut kernel, write).unwrap();
        let mut link = request(GuestFilesystemOperation::Symlink, "/d/link");
        link.target = Some(String::from("file.txt"));
        handle_guest_filesystem_call(&mut kernel, link).unwrap();

        // One ReadDir carries every child's type; no per-entry lstat needed.
        let result = handle_guest_filesystem_call(
            &mut kernel,
            request(GuestFilesystemOperation::ReadDir, "/d"),
        )
        .unwrap();
        let entries = result.entries.expect("entries");
        assert_eq!(entries.len(), 3);
        let by = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        assert!(by("sub").is_directory && !by("sub").is_symbolic_link);
        assert!(!by("file.txt").is_directory && !by("file.txt").is_symbolic_link);
        assert!(by("link").is_symbolic_link);
    }
}
