# Bash

Run shell commands and arbitrary argv operations through the agentOS execution lifecycle.

Run bash inside the VM. Shell commands are the simplest agentOS execution
surface, and share the same execution lifecycle as JavaScript, TypeScript,
Python, and package workflows.

- **`process.exec()`**: Runs a bash command line. Use for pipes, redirects, globs.
- **`process.execFile()`**: Injection-safe; args are never parsed by a shell.
- **In-VM only**: Commands run inside the VM, never in the host shell.

For multi-step work, consider letting the agent write [JavaScript](/docs/javascript)
or [Python](/docs/python) instead — one round trip and real data structures
rather than a chain of shell calls.

## Run commands

## Background and interactive work

`spawn` starts a long-lived process and returns a `pid`. From there you get
stdin, a PTY, output replay, signals, and waiting.

## Files and software

Shell commands see the persistent [filesystem](/docs/filesystem) shared by
agents, JavaScript, and Python. Common POSIX commands ship by default; more is
projected through the [software registry](/docs/software).

## Bindings

[Bindings](/docs/bindings) appear as commands, so pipelines use trusted host
capabilities without putting credentials inside the VM.

## Permissions, limits, and timeouts

Every command inherits the VM [permission policy](/docs/permissions) and
[resource limits](/docs/resource-limits).