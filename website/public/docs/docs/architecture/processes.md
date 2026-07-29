# Processes

Internals of the kernel process model: the virtual process table, how spawns are serviced, stdio bridging, PTYs, and how WASM sh and coreutils map onto it.

<Note>These internal architecture docs are mostly generated and maintained by LLMs, then reviewed by humans. They are intentionally verbose; use your preferred LLM to ask focused questions about the architecture as needed.</Note>

This page is an internals deep-dive on the kernel's **process model**: the data
structures and syscall paths behind every guest process. For the client-facing
API (`process.exec`, `process.spawn`, `terminal.open`, lifecycle, the process tree), see
[Processes & Shell](/docs/processes). For the surrounding component and trust
model, see [Architecture](/docs/architecture).

Two invariants frame everything below:

- **No real host process is ever spawned for guest work.** Every guest process is an entry in a kernel-owned virtual process table, not an OS process. Guest JavaScript runs in V8 isolates; guest commands like `sh` and coreutils run as WebAssembly. Neither is `node` or a host binary.
- **Every process operation is a syscall into the kernel.** Spawning, waiting, signaling, reading stdout, and resizing a PTY all cross from the untrusted executor into the sidecar-owned kernel, which services them against virtualized resources.

## The virtual process table

Each VM owns one process table. It is the authority for what is "running"
inside that VM; nothing in it corresponds to a host PID.

- **Per-VM and isolated.** Two VMs have two independent tables. A PID in one VM is meaningless in another, and processes are never visible across the VM boundary.
- **Holds every guest process,** not only the ones a client started explicitly. A `spawn` from the client, a child spawned by guest `node:child_process`, and the processes behind a shell pipeline are all table entries. This is why the system-wide views (`allProcesses`, `processTree`) can show more than what the client launched.
- **Tracks lifecycle and lineage.** Each entry carries its PID, the command and arguments, parent PID (so the tree can be reconstructed), running/exited status, exit code once collected, and its attached stdio endpoints.
- **Records a driver.** An entry knows which execution backend services it (for example a V8 isolate versus a WASM runtime). This is the `driver` field surfaced on `allProcesses`. Drivers differ in *how* the code runs; they share the same table, the same kernel-owned stdio, and the same boundary.

<Note>The process table is part of the kernel the sidecar owns. The executor never mutates it directly; it can only ask the kernel to create, wait on, or signal an entry. That request-only relationship is the sidecar-to-executor boundary applied to processes.</Note>

## How a spawn is serviced

A spawn, whether it originates from a client `spawn`/`exec` call or from guest
`node:child_process`, follows one path through the kernel:

1. **The request crosses into the kernel.** A client call arrives over the wire protocol; a guest call arrives as a syscall from the executor. Either way the kernel, not the caller, performs the work.
2. **Permission check.** The kernel applies the VM's permission policy before doing anything. Process execution is denied by default and must be granted; the policy is trusted input, the guest making the request is not.
3. **Resolve the program.** The command is resolved against the VM's virtual filesystem (PATH lookup over the VFS), not the host. The resolved program decides the driver: a JavaScript entrypoint runs in a V8 isolate; a `.wasm` program (including `sh` and coreutils) runs on the WASM runtime.
4. **Allocate the table entry.** The kernel assigns a virtual PID, records the command, arguments, environment, working directory, and parent PID, and links stdio endpoints (see below).
5. **Start execution.** The driver begins running the program. For a one-shot `exec` the kernel additionally collects stdout, stderr, and the exit code and returns them as the call's result; for `spawn` it leaves the process running and streams output via events.
6. **Reap and record exit.** When the program finishes, the kernel records the exit code on the table entry and marks it exited, which is what a `wait`/`waitProcess` resolves against and what `processExit` reports.

Signals (`stopProcess` / SIGTERM, `killProcess` / SIGKILL) are the same shape: a
request into the kernel, which applies it to the virtualized process rather than
to any host process.

## Process image replacement

Guest `execve` is distinct from the client-facing one-shot `exec` API. It
replaces the current process image in place, following Linux process semantics:

- the virtual PID, parent, process group, session, working directory, pending signals, blocked-signal mask, stdio, and non-`FD_CLOEXEC` descriptors survive;
- the supplied pathname is resolved literally relative to the current working directory, without a `PATH` or basename fallback;
- `argv` and `envp` replace the old image's values exactly, including a custom or empty `argv[0]`;
- descriptors marked `FD_CLOEXEC` close at the image-commit point; and
- caught signal handlers reset to their defaults, while ignored dispositions remain ignored.

For a WASM-to-WASM replacement, the sidecar commits the kernel state first and
the executor then swaps the module in place. This keeps runner-local pipe,
socket, and duplicated-descriptor state attached to the same process instead of
silently recreating it. `waitpid` reports an exit code and a terminating signal
as separate values, so a normal `exit(137)` is not confused with `SIGKILL`.

## Stdio bridging

Standard streams are kernel-owned objects, not host file descriptors. Each
process entry has stdin, stdout, and stderr endpoints that the kernel wires up
when the entry is created.

- **Capture vs. stream.** For `exec`, the kernel buffers stdout and stderr and hands them back when the process exits. For `spawn`, output is delivered incrementally as `processOutput` events tagged with the PID and the stream (`stdout`/`stderr`), and `processExit` signals completion.
- **Writable stdin.** `writeProcessStdin` pushes bytes into the process's stdin endpoint; `closeProcessStdin` closes the write side so programs that read to EOF (like `cat`) can finish. None of this touches a real pipe on the host.
- **Pipes between processes.** Shell pipelines (`a | b`) connect one process's stdout endpoint to the next process's stdin endpoint through kernel-owned pipes. The pipe is a virtual object in the kernel, so a pipeline behaves like Linux without any host IPC.

Because these endpoints are kernel objects, the same bridging works identically
whether the process is a V8 isolate or a WASM program; the driver writes to and
reads from kernel stdio, not from anything host-provided.

## PTYs and interactive shells

An interactive shell needs a terminal, not just piped stdio: line editing, job
control signals, and window size all depend on a PTY. The kernel provides
virtual PTY devices for this.

- **A shell is a process plus a PTY.** `terminal.open` allocates a kernel PTY and starts a shell process attached to it, returning a `shellId`. The PTY is a virtualized terminal device, never a host `/dev/pts` entry.
- **Bidirectional terminal I/O.** `terminal.write` feeds keystrokes into the PTY master side; everything the shell and its children emit comes back as `shellData` events. This carries terminal control sequences, so full-screen TUIs behave correctly.
- **Resize is a terminal operation.** `terminal.resize` updates the PTY's window size (columns and rows), which the kernel propagates to the foreground process the way a real terminal resize would, so programs relying on `TIOCGWINSZ`-style sizing redraw correctly.
- **Teardown.** `terminal.close` tears down the PTY and the attached shell process. An open shell keeps the VM active, the same way an open PTY keeps a session alive on a real system.

## WASM sh and coreutils on the process model

The shell and the standard commands behind process execution are not special
host helpers; they are ordinary guest processes that happen to be WebAssembly.
For the full WASM execution model see [WASM VM](/docs/architecture/posix-syscalls); here is how it
maps onto the process table specifically.

- **They are normal table entries.** Running `sh`, `ls`, `cat`, etc. allocates virtual PIDs and table entries exactly like any other process, with the WASM driver recorded on each. A pipeline of coreutils is several entries linked by kernel pipes.
- **POSIX process semantics are virtualized, not borrowed from the host.** Plain WASI has no process model (no `fork`/`exec`/`wait`). agentOS supplies those semantics through kernel-backed host imports, so a WASM program that spawns and waits on a child drives the *same* kernel process table that JS guests use. A coreutil spawning a subcommand is one table entry creating another.
- **Same stdio, same PTY.** WASM processes read and write the kernel stdio endpoints described above, and a shell built from WASM `sh` attaches to a kernel PTY just like any interactive shell. The driver differs; the kernel-owned plumbing does not.

This is why the process model is uniform: whether an entry is a V8 isolate or a
WASM binary, it lives in the same per-VM table, goes through the same
permission-checked spawn path, and uses the same kernel-owned stdio and PTYs.

## See also

- [Processes & Shell](/docs/processes): the client API for running and managing processes.
- [WASM VM](/docs/architecture/posix-syscalls): how WebAssembly guests get POSIX process, user, and network semantics.
- [Architecture](/docs/architecture): components, the trust boundary, and the request lifecycle.
- [Permissions](/docs/permissions): the policy the kernel checks on every spawn.