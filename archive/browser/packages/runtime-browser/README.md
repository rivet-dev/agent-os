# AgentOS language execution Browser

Browser driver primitives for agentos.

- Package: `@rivet-dev/agentos-runtime-browser`
- Exports: `createBrowserDriver`, `createBrowserRuntimeDriverFactory`, `createOpfsFileSystem`, `BrowserWorkerAdapter`

The browser WASI command host does not implement the native runtime's
`host_net` socket transport. Commands importing `host_net` (including the
registry SSH, Git, and curl artifacts) may be projected but are rejected when
that process image is actually spawned or executed, with
`ERR_AGENTOS_BROWSER_WASM_NETWORK_UNSUPPORTED`; run those commands in the
native runtime. Unused native-only commands do not prevent a browser VM from
starting.
