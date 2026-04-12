# V8 Runtime

- Guest WebAssembly compilation is enabled by default. Do not install a `set_allow_wasm_code_generation_callback` deny hook on fresh isolates or snapshot restores; package compatibility depends on `WebAssembly.Module` and `WebAssembly.Instance` working inside the isolate.
- WebAssembly safety still comes from V8's built-in limits. Conformance coverage should prove guest WASM works while oversized memory declarations still fail with V8 errors instead of reintroducing an embedder-level deny path.
