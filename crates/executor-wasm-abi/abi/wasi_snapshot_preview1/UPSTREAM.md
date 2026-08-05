# Pinned WASI Preview1 interface source

These WITX files are copied verbatim from the WebAssembly/WASI repository at
commit `d4d3df3072b65ce43cb01c1add72b402d69a79d1` (the source
revision embedded by the pinned `witx = 0.9.1` parser):

- `phases/snapshot/witx/typenames.witx`
- `phases/snapshot/witx/wasi_snapshot_preview1.witx`

The upstream files are licensed under Apache-2.0 with LLVM exception. They are
the source of truth for generated Preview1 core-WASM signatures and memory
layouts. AgentOS's custom `host_*` ABI remains defined by the same generated
manifest alongside the lowered Preview1 surface.
