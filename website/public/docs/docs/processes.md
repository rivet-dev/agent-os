# Processes & Shell

Execute commands, spawn long-running processes, and open interactive shells in agentOS VMs.

Run commands with one-shot `exec`, spawn long-running processes with streaming stdout/stderr and stdin, manage their lifecycle (stop, kill, wait, inspect), open interactive PTY-backed shells, and inspect the process tree across all VM runtimes.

## One-shot execution

Use `exec` to run a command and wait for completion. Returns stdout, stderr, and exit code.

## Spawn a long-running process

Use `spawn` for processes that run in the background. Call `connect()` and subscribe to native `processOutput` and `processExit` events, filtering their `pid` in application code.

## Write to stdin

Send input to a running process.

## Process lifecycle

## Interactive shells

Open an interactive shell with PTY support. Subscribe to native `shellData`, `shellStderr`, and `shellExit` events, filtering their `shellId` in application code.