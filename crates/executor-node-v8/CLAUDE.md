# Node.js V8 executor

This crate is the Node.js-specific execution surface over
`agentos-executor-v8-runtime`.

- Keep Node policy and public Node execution types here.
- Reusable isolate/session mechanics belong in `agentos-executor-v8-runtime`.
- Filesystem, network, process, signal, and TTY semantics belong in the kernel
  and are reached through `agentos-executor-contract`.
- Never add a host-Node fallback for guest code.
