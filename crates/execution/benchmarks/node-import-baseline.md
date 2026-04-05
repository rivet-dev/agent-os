# Agent OS Node Import Benchmark

- Generated at unix ms: `1775118070728`
- Node binary: `node`
- Node version: `v24.13.0`
- Host: `linux` / `x86_64` / `20` logical CPUs
- Repo root: `/home/nathan/a5`
- Iterations: `5` recorded, `1` warmup
- Reproduce: `cargo run -p agent-os-execution --bin node-import-bench -- --iterations 5 --warmup-iterations 1`

| Scenario | Fixture | Cache | Mean wall (ms) | P50 | P95 | Mean import (ms) | Mean startup overhead (ms) |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| `isolate-startup` | empty entrypoint | disabled | 17.17 | 16.11 | 20.98 | n/a | n/a |
| `cold-local-import` | 24-module local ESM graph | disabled | 19.76 | 18.61 | 22.76 | 2.06 | 17.69 |
| `warm-local-import` | 24-module local ESM graph | primed | 18.84 | 19.00 | 19.52 | 1.89 | 16.95 |
| `builtin-import` | node:path + node:url + node:fs/promises | disabled | 17.89 | 17.13 | 20.14 | 0.84 | 17.05 |
| `large-package-import` | typescript | disabled | 206.93 | 207.47 | 215.58 | 189.49 | 17.44 |

## Hotspot Guidance

- Compile-cache reuse cuts the local import graph from 2.06 to 1.89 on average (8.6% faster), but the warm path still spends 16.95 outside guest module evaluation. That keeps startup prewarm work in `ARC-021D` and sidecar warm-pool/snapshot work in `ARC-022` on the critical path above the `17.17` empty-isolate floor.
- Warm local imports still spend 90.0% of wall time in process startup, wrapper evaluation, and stdio handling instead of guest import work. Optimizations that only touch module compilation will not remove that floor.
- The large real-world package import (`typescript`) is 225.7x the builtin path (189.49 versus 0.84). That makes `ARC-021C` the right next import-path optimization story: cache sidecar-scoped resolution results, package-type lookups, and module-format classification before attempting deeper structural rewrites.
- No new PRD stories were added from this run. The measured hotspots already map cleanly onto existing follow-ons: `ARC-021C` for safe resolution and metadata caches, `ARC-021D` for builtin/polyfill prewarm, and `ARC-022` for broader warm-pool and timing-mitigation execution work.

## Raw Samples

### `isolate-startup`
- Description: Minimal guest with no extra imports. Measures the current startup floor for create-context plus node process bootstrap.
- Wall samples (ms): [20.98, 17.24, 15.76, 15.74, 16.11]

### `cold-local-import`
- Description: Cold import of a repo-local ESM graph that simulates layered application modules without compile-cache reuse.
- Wall samples (ms): [18.16, 18.09, 18.61, 21.16, 22.76]
- Guest import samples (ms): [2.09, 2.03, 1.96, 2.24, 2.00]
- Startup overhead samples (ms): [16.07, 16.06, 16.65, 18.92, 20.75]

### `warm-local-import`
- Description: Warm import of the same local ESM graph after a compile-cache priming pass in an earlier isolate.
- Wall samples (ms): [19.00, 19.52, 19.01, 18.00, 18.65]
- Guest import samples (ms): [1.78, 1.91, 1.87, 2.02, 1.84]
- Startup overhead samples (ms): [17.22, 17.61, 17.14, 15.98, 16.81]

### `builtin-import`
- Description: Import of the common builtin path used by the wrappers and polyfill-adjacent bootstrap code.
- Wall samples (ms): [20.14, 17.13, 16.58, 15.79, 19.81]
- Guest import samples (ms): [0.85, 0.85, 0.86, 0.83, 0.82]
- Startup overhead samples (ms): [19.29, 16.29, 15.73, 14.97, 18.99]

### `large-package-import`
- Description: Cold import of the real-world `typescript` package from the workspace root `node_modules` tree.
- Wall samples (ms): [207.96, 203.42, 215.58, 200.22, 207.47]
- Guest import samples (ms): [190.64, 186.51, 198.01, 182.53, 189.76]
- Startup overhead samples (ms): [17.32, 16.91, 17.57, 17.69, 17.71]

