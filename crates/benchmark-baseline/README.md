# agentos-benchmark-baseline

Native floor binary for the differential benchmark matrix.

Build the host lane:

```sh
cargo build --release -p agentos-benchmark-baseline
```

Build the WASI lane used by the VM WASM executor:

```sh
cargo build --release --target wasm32-wasip1 -p agentos-benchmark-baseline
```

The wasm artifact is written to `target/wasm32-wasip1/release/agentos-benchmark-baseline.wasm`.
