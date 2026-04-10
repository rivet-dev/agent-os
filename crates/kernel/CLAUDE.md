# Kernel

The kernel provides a POSIX-like userspace environment. The goal is that a program written for Linux should run inside the VM without modification, subject to the execution runtimes available (Node.js, WASM, Python).

## Linux Compatibility

- **Correct errno values.** Every kernel operation that fails must return the correct POSIX errno (`ENOENT`, `EACCES`, `EEXIST`, `EISDIR`, `ENOTDIR`, `EXDEV`, `EBADF`, `EPERM`, `ENOSYS`, etc.). Agents check errno values to decide control flow -- wrong errnos cause cascading failures.
- **Standard `/proc` layout.** `/proc/self/`, `/proc/[pid]/`, `/proc/[pid]/fd/`, `/proc/[pid]/environ`, `/proc/[pid]/cwd`, `/proc/[pid]/cmdline` should contain the expected content.
- **Synthetic procfs paths use guest-visible permission subjects.** Permission checks for procfs access should authorize the guest-visible proc path directly rather than resolving through the backing VFS realpath.
- **Standard `/dev` devices.** `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, `/dev/fd/*`, `/dev/pts/*` must exist and behave correctly. `/dev/urandom` must return cryptographically random bytes, not deterministic values.
- **Stream-device byte counts belong on length-aware read paths.** For unbounded devices such as `/dev/zero` and `/dev/urandom`, exact Linux-style byte-count assertions should target `pread` / `fd_read` in `device_layer.rs` and kernel FD tests; `read_file()` has no byte-count parameter.
- **Correct signal semantics.** `SIGCHLD` on child exit. `SIGPIPE` on write to broken pipe. `SIGWINCH` on terminal resize. Signal delivery must respect process groups and sessions.
- **Virtual processes should stay on the normal FD table.** For tool-backed child processes, create a regular kernel process entry, wire stdio through fd `0`/`1`/`2` with pipes or PTYs, and use the owner-checked kernel helpers to read stdin, write stdout/stderr, and mark exit instead of introducing side buffers.
- **Kernel-owned networking should start in `socket_table.rs`.** Track per-process socket/listener/connection lifecycle there and let `cleanup_process_resources()` reclaim those records on process exit instead of adding ad hoc side counters elsewhere.
- **Standard filesystem paths.** `/tmp` must be writable. `/etc/hostname`, `/etc/resolv.conf`, `/etc/passwd`, `/etc/group` should contain valid content. `/usr/bin/env` should exist for shebangs. Shell (`/bin/sh`, `/bin/bash`) must be available.
- **Direct script exec should resolve registered stubs before reparsing files.** When the kernel executes a path under `/bin/` or `/usr/bin/` that corresponds to a registered command driver, dispatch that driver directly before falling back to shebang parsing.
- **Environment variable conventions.** `HOME`, `USER`, `PATH`, `SHELL`, `TERM`, `HOSTNAME`, `PWD`, `LANG` must be set to reasonable values. `PATH` must include standard directories where commands are found.
- **Document deviations in the friction log** at `.agent/notes/vm-friction.md`.

## Virtual Filesystem Design Reference

- The VFS chunking and metadata architecture is modeled after **JuiceFS** (https://juicefs.com/docs/community/architecture/). Reference JuiceFS docs when designing chunk/block storage, metadata engine separation, or read/write data paths.
- Key JuiceFS concepts that apply: three-tier data model (Chunk/Slice/Block), pluggable metadata engines (SQLite, Redis, PostgreSQL), fixed-size block storage in object stores (S3), and metadata-data separation.
- For detailed design analysis: https://juicefs.com/en/blog/engineering/design-metadata-data-storage

### Agent-OS filesystem packages

- The old `fs-sqlite` and `fs-postgres` packages were deleted. They are replaced by the Agent OS `SqliteMetadataStore` and the `ChunkedVFS` composition layer.
- File system drivers live in `registry/file-system/`. Prefer their declarative mount helpers when available; the legacy custom-`VirtualFileSystem` path is only for arbitrary caller-supplied filesystems and compatibility fallbacks.
- The Rivet actor integration currently uses `ChunkedVFS(InMemoryMetadataStore + InMemoryBlockStore)` as legacy temporary infrastructure. This must move to durable metadata and block storage.

## Filesystem Conventions

- **OS-level content uses mounts, not post-boot writes.** If agentOS needs custom directories in the VM (e.g., `/etc/agentos/`), mount a pre-populated filesystem at boot -- don't create the kernel and then write files into it afterward.
- **Filesystem semantics must be durable.** Any state that changes filesystem behavior -- including overlay deletes, whiteouts, tombstones, copy-up state, directory entries, inode metadata, or file contents -- must be represented in durable filesystem or metadata storage. No in-memory side tables or transient hacks.
- **Overlay metadata must stay out-of-band from the merged tree.** Store whiteouts or opaque-directory markers under a reserved hidden metadata root and filter that root out of user-visible results.
- **Overlay mutating ops need raw-layer checks plus upper-layer moves.** Once copy-up marks directories opaque, merged `read_dir()` no longer tells you whether lower layers still hold children, so `rmdir`-style emptiness checks must inspect raw upper and lower entries directly. For identity-preserving ops like `rename`, stage the source into the writable upper first and then call the upper filesystem's native `rename`.
- **Overlay filesystem behavior must match Linux OverlayFS as closely as possible, including mount-boundary semantics.** Treat the kernel OverlayFS docs as normative. OverlayFS overlays directory trees, not the mount table. Mounted filesystems remain separate mount boundaries, and cross-mount operations must keep normal mount semantics (`EXDEV`, separate identity, separate read-only rules).
- **User-facing filesystem APIs should distinguish mounts from layers.** Mounts are separate mounted filesystems presented to the kernel VFS. Layers are overlay-building blocks. Do not collapse those into one generic concept.
- **Middle layers in a Docker-like stack should be frozen layers, not extra writable uppers.** Linux OverlayFS supports one writable upper per overlay mount. Additional stacked layers should be immutable snapshot/materialized lower layers.
- **readdir returns `.` and `..` entries** -- always filter them when iterating children to avoid infinite recursion.
- **`VirtualStat` additions must be propagated end-to-end.** When stat grows new fields, update kernel-backed storage stats, synthetic `/proc` and `/dev` stats, sidecar mount/plugin conversions, sidecar protocol serialization, and the TypeScript `VirtualStat` / `GuestFilesystemStat` adapters together.
- **Never interfere with the user's filesystem or code.** Don't write config files, instruction files, or metadata into the user's working directory. Use dedicated OS paths or CLI flags instead.
- **Agent prompt injection must be non-destructive.** Preserve existing user-provided instructions, append rather than replace, and always provide `skipOsInstructions` opt-out.
