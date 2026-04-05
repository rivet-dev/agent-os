# TODO

- Add OCI import/export support for overlay filesystem layers and snapshots after phase 1. The phase-1 API should only guarantee the bundled base filesystem artifact and the internal snapshot export/import format.
- Run the full registry native-kernel suite after rebuilding the WASM command artifacts. `registry/tests/smoke.test.ts` skipped in this pass because the local `registry/native/target/wasm32-wasip1/release/commands` binaries were not present, so the new `createKernel` sidecar-backed path is only verified by package-level builds plus targeted core tests right now.
- Expand verification for the new sidecar-backed kernel compatibility surface around `socketTable`/`processTable` observability and browser runtime end-to-end specs. The source builds are green and targeted tests passed, but the deeper integration suites were not exercised in this pass.
