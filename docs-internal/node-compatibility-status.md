# Node.js Compatibility Status

This document is the source of truth for AgentOS Node.js compatibility test
coverage, known failures, deferred limitations, and validation history. Update
it whenever a fixture, runtime behavior, package-manager workflow, or relevant
nightly gate changes.

## Status vocabulary

| Status | Meaning |
| --- | --- |
| `passing` | The latest isolated rerun matches host Node behavior. |
| `open` | Deterministic incompatibility or harness defect that should be fixed. |
| `environmental` | Failure was caused by missing prerequisites or runner load; the test remains required. |
| `upstream` | The published package is defective on host Node too, so runtime compatibility cannot be evaluated. |
| `blocked-native` | The package requires an unsupported native/N-API addon and has no usable JavaScript/WASM fallback. |
| `deferred` | Valid compatibility work, explicitly postponed with a recorded reason. |
| `fixed-awaiting-rerun` | A fix exists but has not completed the full validation sequence. |

`blocked-native` is not a generic escape hatch. Record the exact addon, prove
that the exercised path loads it, and check for a maintained pure JavaScript or
WASM alternative before using this status.

## Latest baseline

Baseline date: 2026-07-19  
Revision: change `ymrvvnlu` on bookmark `node-ecosystem`  
Base: `main` / `9d24346f`  
Runtime: Node.js 24.17.0, pnpm 10.13.1; pinned Yarn CLIs are fixture dependencies, while Bun and global Yarn remain unavailable on this host  
Browser runtimes: intentionally excluded

| Surface | Result | Status | Notes |
| --- | ---: | --- | --- |
| Workspace build | 54/54 tasks | `passing` | The full non-browser root build, including OpenCode and agent software packaging, passed. |
| Workspace typecheck | 76 tasks passed before shared-workspace cleanup | `environmental` | The full check was invalidated when another session removed root/package `node_modules` and `target/debug` during execution. Focused runtime-core and core typechecks pass after restoring dependencies; browser packages remain excluded. |
| Default runtime-core Vitest | 292 passed, 1 failed, 1 skipped | `environmental` | The sole complete-run failure was the WASM abstract Unix-socket test reaching its 30-second outer timeout under shared-runner load. The exact case passed in 4.02s in isolation; all 25 child-process tests and the remaining networking/runtime cases passed in the complete run. |
| Full ecosystem catalog | 87 passed, 6 skipped | `passing` | Final clean rerun completed in 773.69s across 92 fixtures plus discovery. All 86 executable fixture contracts are green, including Vitest/Mocha, Vite/Rollup, npm/pnpm/Yarn, TypeScript and developer CLIs, WebSockets, Vercel and agent SDKs, Prisma, Supabase, Browse CLI, Astro, Next.js, and explicit native-failure contracts. |
| npm workflows | 27/27 passed | `passing` | Final clean rerun completed in 395.41s. npm install/list/init/scripts/lifecycle, npx package execution, pipes, concurrently, and a clean Next production build all pass. |
| Secure WebSocket regression | 1/1 passed | `passing` | A guest `ws` client exchanges a message with a host `wss://` endpoint. Empty SNI on an IP target now uses the connection host as the rustls identity without emitting SNI. |
| Non-browser Rust workspace check | pass | `passing` | `cargo check --workspace` passes with both browser sidecars excluded. |
| Runtime accounting soak | 1/1 | `passing` | No accounting or scheduler drift. |
| Native multi-VM fault soak | 0/1 | `open` | Node import-cache materialization exceeded 30 seconds. |
| Fixed product versions | pass | `passing` | All 20 checked package/Cargo manifests remain pinned to `0.0.1`. |
| Nightly workflow YAML parse | pass | `passing` | The nightly workflow parses and includes both opt-in Node matrices. |
| Post-rebase focused validation | all focused checks | `passing` | On current `main`, real Vitest and `ws` parity, four npm workflow cases, npm-bin realpath behavior, runtime-core typechecking, targeted Cargo checking, Rust formatting, fixed versions, mirror generation, JSON, and nightly YAML all pass. |

## Ecosystem catalog

The full catalog runs every fixture through host Node and AgentOS, then compares
exit code, stdout, and stderr. A green installation alone is not compatibility.

### Passing fixtures

| Fixture | Coverage |
| --- | --- |
| `astro-pass` | Real Astro build using the catalog's JavaScript/WASM-compatible toolchain path |
| `async-exit-code-pass` | Exit status retained after asynchronous work drains |
| `axios-pass` | HTTP client and response handling |
| `bcryptjs-pass` | Pure-JavaScript password hashing |
| `browserbase-browse-cli-pass` | Installed Browserbase Browse CLI execution and command/help discovery without launching a browser runtime |
| `chalk-pass` | CommonJS package and terminal formatting |
| `child-process-ipc-pass` | `fork()` startup buffering, IPC liveness, reply, disconnect, and termination |
| `cli-toolkit-pass` | Commander, Yargs, bounded Execa sync execution, Ora, Glob, and Fast Glob behavior |
| `crypto-random-pass` | Node crypto random APIs |
| `dotenv-pass` | Environment-file parsing |
| `drizzle-pass` | Drizzle ORM package loading/query construction |
| `esm-import-pass` | Basic ESM loading |
| `express-pass` | HTTP server lifecycle |
| `fastify-pass` | HTTP server lifecycle |
| `fs-metadata-rename-pass` | Metadata and rename behavior |
| `hono-node-server-pass` | Hono Node server adapter |
| `ioredis-pass` | Redis client package loading and offline behavior |
| `jsonwebtoken-pass` | JWT signing and verification |
| `lodash-es-pass` | ESM-only utility package |
| `mocha-pass` | Real Mocha CLI discovery, hooks, synchronous tests, and asynchronous tests |
| `module-access-pass` | Projected module access and transitive loading |
| `mysql2-pass` | MySQL client package loading and protocol helpers |
| `net-create-server-pass` | Node TCP listener behavior |
| `node-fetch-pass` | Fetch client behavior |
| `npm-layout-pass` | npm dependency layout |
| `nextjs-pass` | Real Next.js production build and emitted manifest/page artifacts |
| `optional-deps-pass` | Optional dependency resolution |
| `pg-pass` | Pure-JavaScript PostgreSQL client path (`pg`, not `pg-native`) |
| `pino-pass` | Structured logging |
| `pnpm-layout-pass` | pnpm symlink/store layout |
| `pnpm-cli-pass` | pnpm 10.13.1 CLI startup and exact version output |
| `prisma-pass` | Prisma 7 generated client, Decimal/SQL utilities, and pure-JavaScript PostgreSQL driver adapter path |
| `require-main-pass` | CommonJS entrypoint `require.main` and `module.id === "."` semantics |
| `rollup-wasm-cli-pass` | Real Rollup CLI bundling and tree shaking through the official WASM build |
| `semver-pass` | Version parsing and ranges |
| `ssh2-pass` | SSH protocol package loading |
| `ssh2-sftp-client-pass` | SFTP client package loading |
| `supabase-pass` | Supabase query construction and an authenticated REST exchange against a local server |
| `uuid-pass` | UUID generation |
| `vite-pass` | Core Vite/Rollup JavaScript production bundle |
| `vitest-pass` | Real Vitest CLI with Vite 7 and WASM Rollup: two files, hooks, async tests, and inline snapshots |
| `ws-pass` | `ws` client/server behavior plus Node's global WHATWG `WebSocket`, including text and binary echo |
| `yaml-pass` | YAML parsing and serialization |
| `yarn-classic-cli-pass` | Yarn Classic 1.22.22 CLI startup and exact version output |
| `yarn-modern-cli-pass` | Yarn Modern 4.17.1 CLI startup and exact version output |
| `zod-pass` | Schema parsing and validation |

The grouped agent-use fixtures add meaningful behavioral coverage for these
packages:

| Fixture | Packages exercised |
| --- | --- |
| `agent-frameworks-pass` | `e2b`, `@e2b/code-interpreter`, `@mastra/core`, `effect`, `@slack/web-api`, `@slack/bolt` |
| `agent-sdks-pass` | `openai`, `@anthropic-ai/sdk`, `@modelcontextprotocol/sdk`, `langchain`, `googleapis`, `octokit` |
| `archives-agent-pass` | `jszip`, `archiver`, `fflate` |
| `crypto-agent-pass` | `jose`, `openpgp`, `libsodium-wrappers`, `tweetnacl` |
| `databases-agent-pass` | `postgres`, `mongodb`, `redis`, `@libsql/client` remote/web path, `@aws-sdk/client-s3` |
| `developer-clis-pass` | Prettier, ESLint, webpack/webpack-cli, Babel, Terser, rimraf, cross-env, `json`, manypkg, and Madge with real formatting, fixing, bundling, transforming, minifying, removal, child-env, JSON editing, monorepo checking, and graph-generation behavior |
| `documents-agent-pass` | `pdf-lib`, `mammoth`, `exceljs` |
| `http-clients-agent-pass` | `undici`, `got`, `ky`, `superagent`, Socket.IO polling and forced WebSocket transports |
| `images-agent-pass` | `jimp`, `pngjs`, `jpeg-js` |
| `parsing-agent-pass` | `cheerio`, `linkedom`, `marked`, `markdown-it`, `csv-parse`, `fast-xml-parser`, `protobufjs`, `ajv` |
| `typescript-cli-pass` | TypeScript 6 compiler CLI producing JavaScript from a real project |
| `vercel-ai-pass` | AI SDK core, OpenAI/Anthropic/Gateway providers, React/ReactDOM server rendering, and Workflow |
| `vercel-platform-pass` | Vercel Blob, Edge Config, Express/Fastify/Hono adapters, current and legacy Flags, Functions, NFT, OG, OIDC, OpenTelemetry, Sandbox, and a representative SDK operation module |

### Deferred, open, and native-service probes

| Fixture | Status | Latest failure | Next action |
| --- | --- | --- | --- |
| `node-test-runner-blocked` | `open` | AgentOS does not currently expose the core `node:test` module. | Implement the Node test-runner builtin; this is a runtime compatibility gap, not a native-addon limitation. |
| `nextjs-turbopack-blocked` | `blocked-native` | Next 16 Turbopack attempts the platform SWC `.node` addons and then requires `node:worker_threads`; the staged SWC WASM fallback is insufficient for Turbopack. | Retain the exact expected-failure contract. The non-Turbopack Next production build remains passing. |
| `jest-native-resolver-blocked` | `blocked-native` | Jest 30.4.2 delegates module resolution to `unrs-resolver` 1.12.2, whose supported Node path loads a platform N-API `.node` binding. Host Node runs the real Jest test; AgentOS fails the explicit native-resolver contract. | Retain the expected-failure contract until Jest or `unrs-resolver` offers a usable non-native Node path. |
| `vite-react-esbuild-blocked` | `deferred` | Vite's React build requires the esbuild native service executable and does not settle in the JavaScript-only VM. | Retain as an explicit expected-failure contract; core Vite/Rollup coverage remains passing in `vite-pass`. |
| `vitest-default-rolldown-native-blocked` | `blocked-native` | Vitest's current default dependency graph uses Vite 8/Rolldown and loads `rolldown-binding.linux-x64-gnu.node`. | Keep the native failure contract. Supported Vitest uses Vite 7 plus `@rollup/wasm-node` in `vitest-pass`. |
| `rollup-native-blocked` | `blocked-native` | Rollup's default Linux package loads its platform native binding. | Keep the native contract and use the passing official WASM Rollup path where native code is unavailable. |
| `tsx-esbuild-native-blocked` | `open` | The public `tsx/esm/api` path first reaches unsupported `module.register()` loader hooks; esbuild's native service is a downstream limitation. | Implement public Node loader-hook behavior before reclassifying the remaining esbuild dependency. |
| `tsup-esbuild-native-blocked` | `blocked-native` | The real tsup CLI reaches native Rollup/esbuild tooling. | Retain the expected-failure contract while the individual TypeScript compiler and WASM Rollup paths remain passing. |
| `swc-native-blocked` | `blocked-native` | `@swc/core` loads a platform `.node` binding. | Retain the expected-failure contract; Next's separately staged SWC WASM production build passes. |
| `biome-native-blocked` | `blocked-native` | Biome's CLI is a platform-native executable. | Retain the expected-failure contract. |
| `turbo-native-blocked` | `blocked-native` | Turbo's CLI is a platform-native executable. | Retain the expected-failure contract. |
| `vercel-sdk-root-large-graph-blocked` | `deferred` | The `@vercel/sdk` root barrel expands a 43 MiB, 4,117-file generated ESM graph and exceeds the bounded 120-second import window. | Improve very-large-graph loading. A representative `@vercel/sdk/sdk/user.js` operation module already passes. |

### Catalog skips

| Fixture | Status | Reason |
| --- | --- | --- |
| `bun-layout-pass` | `environmental` | Bun is unavailable on this validation host. |
| `yarn-berry-layout-pass` | `environmental` | Yarn is unavailable on this validation host. |
| `yarn-classic-layout-pass` | `environmental` | Yarn is unavailable on this validation host. |
| `pg-native-blocked` | `environmental` | The host addon links a newer glibc/libpq ABI, so host-success/guest-failure parity cannot be established on this runner. |
| `vercel-mcp-adapter-upstream-blocked` | `upstream` | Published `@vercel/mcp-adapter@0.3.2` contains metadata but no resolvable root implementation or entrypoint, including on host Node. |
| `vercel-sdk-root-large-graph-blocked` | `deferred` | The bounded root-barrel import is intentionally skipped; representative SDK operation-module coverage passes. |

### Recently repaired blockers

| Surface | Status | Evidence |
| --- | --- | --- |
| `workspace-layout-pass` | `passing` | Full host/AgentOS parity passes with scoped workspace-symlink target mounts. |
| `jsdom-pass` | `passing` | Full parity passes with synchronous `require(esm)` plus Node-compatible `buffer.isAscii`/`isUtf8`. |
| Socket.IO WebSocket upgrades | `passing` | Standalone `ws`, Socket.IO polling, and forced `websocket` ping/ack pass in the full catalog. |
| Node global WebSocket | `passing` | A WHATWG/EventTarget adapter over the proven sidecar-backed `ws` transport passes a real text-echo parity test; the underlying `ws` fixture also covers binary echo. |
| Secure WebSockets over TLS | `passing` | The dedicated host/guest `wss://` exchange passes; IP endpoints preserve empty-SNI behavior while using the host as rustls's handshake identity. |
| OpenPGP default password S2K | `passing` | The unmodified upstream default passes through bounded incremental host hash sessions in the full catalog. |
| Large synchronous file reads | `passing` | Next's 41.8 MiB SWC WASM is transferred in bounded raw pathname ranges instead of one amplified 55.7 MiB response. |
| Next.js production builds | `passing` | The fixture pre-stages Next's official SWC WASM fallback, runs a clean production build, and verifies build/page/API manifests and compiled output. |
| CommonJS main-module semantics | `passing` | Direct CommonJS entrypoints set `require.main`, `process.mainModule`, a null parent, and `module.id === "."`. |
| Async `process.exitCode` | `passing` | Exit status is re-read after active handles and pending module evaluation drain. |
| Child-process IPC | `passing` | Pre-listener messages are bounded and buffered, IPC keeps the process alive only with listeners, kill wakes the event pump, and disconnect/exit complete. |
| CommonJS/ESM default interop | `passing` | `require(esm)` now supplies Node's `__esModule` marker when a default export exists, fixing E2B's bundled Chalk interop. |
| `util.promisify(execFile)` | `passing` | Node's global custom-promisifier symbol is honored and `execFile` resolves `{ stdout, stderr }`, allowing Browse CLI execution. |
| Package export specificity | `passing` | Overlapping wildcard exports prefer the more specific pattern, so generated SDK paths no longer resolve with duplicated `.js` suffixes. |
| Crypto named exports | `passing` | The ESM export inventory matches the implemented crypto surface, including cipher, DH, curve, prime, and RSA helpers required by Vercel Flags. |
| CLI stdio EventEmitter surface | `passing` | stdout/stderr now expose prepend/remove/listener APIs required by Yarn Classic. |
| Nested CommonJS dynamic imports | `passing` | Dynamic imports made while loading a nested CommonJS module resolve against that module instead of the synthetic entry resource; webpack-cli 6 now bundles successfully. |
| Relative synchronous CLI file I/O | `passing` | `readFileSync` and `writeFileSync` resolve relative paths against the child process cwd, allowing Terser and similar CLIs to operate on ordinary relative filenames. |
| Builtin ESM export inventory | `passing` | `fs.realpath`, `fs.rmdir`/`rmdirSync`, `node:constants` file constants, and `util.styleText` are available to current CLI dependency graphs. |

### Native/N-API negative coverage

These fixtures are green only when host Node loads the native path and AgentOS
fails with the expected unsupported-addon signature: `sharp`, `canvas`,
`@napi-rs/canvas`, `better-sqlite3`, `sqlite3`, `bcrypt`, `argon2`, Jest 30's
`unrs-resolver`, the
`pdfjs-dist` Node canvas path, the `@libsql/client` local-file path, native
Rollup/Rolldown, SWC, Biome, Turbo, and tsup's native build path.
`pg-native` is separately skipped because this runner's host addon links a
newer glibc/libpq ABI, so a valid host-success baseline cannot be established.
This does **not** apply to the popular pure-JavaScript `pg` or `postgres`
packages: both pass in the catalog.

## npm workflow status

### Package-manager CLI coverage

| Manager | Status | Automated evidence |
| --- | --- | --- |
| npm | `passing` | The 27-case npm workflow suite covers `npm --version`, `npx --version`, init, online install/list, scripts, lifecycle hooks, pipes, and package execution. |
| pnpm 10.13.1 | `passing` | `pnpm-cli-pass` executes the installed CLI in AgentOS and compares exact version output with host Node. pnpm 11 currently requires Node >=22.13 while the guest advertises 22.0.0. |
| Yarn Classic 1.22.22 | `passing` | `yarn-classic-cli-pass` executes the installed CLI with exact host parity. |
| Yarn Modern 4.17.1 | `passing` | `yarn-modern-cli-pass` executes the current bundled CLI distribution with exact host parity. |

| Issue | Status | Affected coverage | Evidence / next action |
| --- | --- | --- | --- |
| npm/npx shell launcher | `passing` | npm and npx commands | Shell shims now resolve to the real npm CLI package root instead of projecting the shim as the package. |
| npm extraction on mapped files | `passing` | `npm install`, `npx -y` | Mapped host fds now support `futimes`, and write backpressure classification no longer sends mapped fds to the kernel table. |
| npm lifecycle and scripts | `passing` | pre/postinstall, `npm run`, env propagation, shell operators | All automated cases pass. |
| npm online workflows | `passing` | install/list/execute `left-pad`, npx `semver` and `cowsay` | All automated cases pass with registry access. |
| npm `.bin` main-module symlinks | `passing` | npx `semver`, npx `cowsay`, package CLIs | Child Node processes dereference the main-module symlink before module evaluation, so `__dirname` and relative package loads match Node. A focused regression loads `../package.json` from a symlinked package bin. |
| Default writable cwd | `passing` | `npm init -y` with the in-memory compatibility filesystem | `/workspace` is now owned by uid/gid 1000 in both compatibility bootstraps, matching the product base filesystem and the default agent user. |
| Next.js focused build | `passing` | dedicated npm workflow | A clean production build uses the official SWC WASM fallback and verifies `.next/build-manifest.json`, page/API manifests, and compiled artifacts. |
| Cold online workflow deadlines | `passing` | npx download/extract, install+list, unreachable-registry error | Outer test allowances now cover their bounded sequential phases; the full 27-case matrix passes. |

## Rust/native runtime failures

| Area / target | Status | Latest failure | Next action |
| --- | --- | --- | --- |
| Native root mount (`agentos-client`) | `open` | Relative guest write to `/workspace/relative.txt` is denied. | Repair writable native-root path mapping and permissions. |
| WASM execution | `open` | 64 KiB stack limit cannot be enforced by V8; recursion watchdog expires. | Make the configured limit enforceable or reject it at configuration time with a supported minimum. |
| WASM `path_open` flags | `open` | Directory/nofollow lookup flags are dropped before kernel conversion. | Preserve WASI lookup flags through the host import. |
| Projected WASM modules | `open` | `/identity.wasm`, `/signal-state.wasm`, `/entry.wasm`, and service test modules resolve as ENOENT. | Fix execution-source projection/permission path resolution. |
| Python/VFS sharing | `open` | `/workspace/new.txt` is absent after Python writes it. | Repair Python shadow-to-kernel write synchronization. |
| JS bridge reads | `open` | Missing stat payload becomes `EIO`; bounded reads expect `EINVAL`. | Return typed stat/read errors and payloads from the bridge. |
| Limits inventory | `open` | 19 constants are unclassified. | Add typed configuration or documented invariant/deferred inventory entries. |
| Binding registry bound | `open` | Capacity rejection uses untyped `invalid_state`. | Return a structured resource-limit error naming the limit and override path. |
| Native stdio termination | `open` | Closing a required response/control ingress stream does not fail the sidecar. | Enforce the three-lane terminal contract. |
| Node server close | `open` | Listener teardown enters close drain gate in the wrong order. | Complete listener teardown asynchronously before drain completion. |
| ObjectS3 tests | `deferred` | Three active tests expect unsupported xattrs, special inodes, and atime behavior. | Either implement the dormant backend or consistently ignore it with the existing dormant-ObjectS3 reason. |
| xfstests correctness | `environmental` | 29 tests fail because `XFSTESTS_ROOT` is absent. | Provision the pinned xfstests tree in the expensive gate or skip with a typed prerequisite reason. |
| Multi-VM fault soak | `open` | Node import-cache materialization exceeds its 30-second bound. | Determine whether this is contention-only; retain the bound and fix cache/materialization progress if reproducible in isolation. |
| Node test runner | `open` | `node:test` is not registered as a supported builtin. | Implement the core test-runner module; do not classify it as a native blocker. |
| ESM loader registration | `open` | `module.register()` is unavailable, blocking tsx's public ESM API before its esbuild path is reached. | Implement bounded loader registration compatible with Node's public API. |

## Build and harness issues

| Issue | Status | Next action |
| --- | --- | --- |
| Incomplete generated `toolchain/vendor` accepted as valid | `open` | Validate required files/checksum or revendor automatically instead of checking directory existence. |
| OpenCode inherits nonexistent Corepack `node-gyp` | `open` | Provision a repository-owned `node-gyp` path for nested Bun lifecycle scripts. |
| Fixed-version verifier scans generated ecosystem caches | `passing` | Generated `.cache` trees are excluded and covered by a regression test. |
| Runtime integration watchdogs under host contention | `environmental` | Rerun in isolation; improve diagnostics and prevent unhandled teardown rejections after timeout. |
| Temporary test tools removed from `/tmp/agentos-*` | `environmental` | Keep long-lived test runtimes in a persistent cache path and avoid broad cleanup collisions. |
| Node 22+ synchronous `require(esm)` interop | `passing` | Synchronous ESM graphs load as namespace objects; graphs with top-level await fail with `ERR_REQUIRE_ASYNC_MODULE`. jsdom 29 passes focused parity. |
| Default OpenPGP password S2K exceeds bridge request accounting | `passing` | Incremental hash sessions keep every update bounded and the catalog now exercises the unmodified OpenPGP default. |
| Loaded integration harness deadlines | `passing` | Pipe, shim, dispose, and WASI-spawn files retain their runtime assertions while outer test windows cover setup and idempotent teardown. Full runs can still report environmental starvation when unrelated builds saturate the shared host; every affected file passes in isolation. |
| Current wrapper/watcher CLIs | `deferred` | `dotenv-cli` 11 loses its loaded env when handing off to the child, `nodemon` 3 does not complete the crash-supervision contract, and zx 8 observes the snapshot-default `require("process").argv`. Track these as process-module/child-supervision gaps before promoting them to passing fixtures. |
| ts-node 10 | `deferred` | File execution eagerly imports the restricted interactive `node:repl` module and Node-private `Module` hooks. The public TypeScript 6 compiler CLI passes; do not weaken the REPL security contract just to admit ts-node. |
| Babel 8 CLI | `deferred` | The current ESM/Commander CLI does not retain its first positional argument in the guest execution path. Babel 7.29.7 has full transform parity and remains the admitted behavioral fixture. |
| Execa async execution | `open` | The new catalog proves bounded `execaSync`; the asynchronous Execa path did not complete reliably during probe development. | Add an isolated async completion/backpressure fixture and fix the owning child lifecycle path before claiming full Execa support. |

## Candidate expansion catalog

Candidates must exercise meaningful behavior rather than only `require()` the
package. Research and implementation status will be maintained here.

| Category | Candidate packages | Status |
| --- | --- | --- |
| PDF/document | `pdf-lib`, `pdfjs-dist`, `mammoth`, `exceljs` | `added` |
| Images | `jimp`, `pngjs`, `jpeg-js` | `added` |
| Archives/compression | `jszip`, `archiver`, `fflate` | `added` |
| Databases/servers | `postgres`, `mongodb`, `redis`, `@libsql/client`, `@aws-sdk/client-s3` | `added` |
| HTTP/API clients | `undici`, `got`, `ky`, `superagent`, `socket.io` | `added` |
| Crypto/auth | `jose`, `openpgp`, `libsodium-wrappers`, `tweetnacl` | `added` |
| Parsing/data formats | `cheerio`, `jsdom`, `linkedom`, `marked`, `markdown-it`, `csv-parse`, `fast-xml-parser`, `protobufjs`, `ajv` | `added` |
| Agent/LLM tooling | `openai`, `@anthropic-ai/sdk`, `@modelcontextprotocol/sdk`, `langchain`, `googleapis`, `octokit` | `added` |
| Vercel/AI platform | `ai`, `@ai-sdk/openai`, `@ai-sdk/anthropic`, `@ai-sdk/gateway`, `workflow`, React/ReactDOM, and Vercel's Node server packages | `added`; Turbopack and the SDK root barrel are explicitly deferred |
| Agent frameworks | `e2b`, `@e2b/code-interpreter`, `@mastra/core`, `effect`, `@slack/web-api`, `@slack/bolt` | `added` |
| Database platforms | `@prisma/client`, `@prisma/adapter-pg`, `@supabase/supabase-js` | `added` |
| Browser automation CLI | Browserbase `browse` CLI | `added`; CLI-only because browser runtimes are outside AgentOS scope |
| Package managers | npm, pnpm 10, Yarn Classic, Yarn Modern | `added`; npm has the deep 27-workflow suite and the others have direct installed-CLI parity fixtures |
| Test/build tooling | Vitest, Mocha, Vite, Rollup WASM, TypeScript | `added`; default Vitest/Rolldown, native Rollup, tsup, tsx loader hooks, SWC, Biome, and Turbo have explicit blocker contracts |
| CLI foundations | Commander, Yargs, Execa sync, Ora, Glob, Fast Glob | `added`; asynchronous Execa completion remains open |
| Developer CLIs | Prettier, ESLint, webpack/webpack-cli, Babel, Terser, rimraf, cross-env, `json`, manypkg, Madge, TypeScript | `added` with behavioral checks; Jest 30 is an explicit N-API blocker, while dotenv-cli, nodemon, zx, ts-node, and Babel 8 remain tracked deferred candidates |
| Native-addon negative coverage | `sharp`, `canvas`, `@napi-rs/canvas`, `better-sqlite3`, `sqlite3`, `pg-native`, `bcrypt`, `argon2`, `pdfjs-dist`'s Node canvas path, `@libsql/client`'s local file path, Rollup/Rolldown, SWC, Biome, Turbo | `added` |

The database catalog deliberately includes both `pg` (already passing) and
`postgres`: both have supported pure-JavaScript paths. `pg-native` is tracked
separately because it explicitly selects a native binding. Native-package
classification is backed by explicit host-success/guest-failure fixtures;
`pg-native` remains the sole host-ABI prerequisite skip.

Prisma coverage deliberately uses Prisma 7's JavaScript query compiler and
`@prisma/adapter-pg`; it does not claim support for Prisma's native Rust engine
paths. Supabase's REST client path passes, and WebSocket behavior is covered by
an actual global WebSocket echo. The deprecated `@vercel/flags` package and its
current `flags` replacement are both covered. Deprecated `@vercel/postgres` and
`@vercel/kv` were not promoted because `pg`, `postgres`, Redis clients, Prisma,
and Supabase already cover the maintained paths. Browser-only Vercel analytics
packages remain excluded by the repository's Node-only runtime policy.

## Rerun commands

```bash
# Default TypeScript/runtime surface (browser packages remain excluded)
pnpm check-types
AGENTOS_E2E_NETWORK=1 pnpm --dir packages/runtime-core test

# Full package ecosystem and package-manager workflows
pnpm --dir packages/runtime-core test:ecosystem:full
pnpm --dir packages/runtime-core test:npm-workflows

# Complete non-browser Rust surface
cargo test --workspace \
  --exclude agentos-sidecar-browser \
  --exclude agentos-native-sidecar-browser \
  --no-fail-fast -- --test-threads=1

# Explicit ignored churn gates
cargo test -p agentos-runtime \
  multi_vm_generation_soak_has_no_accounting_or_scheduler_drift \
  --lib -- --ignored --test-threads=1
cargo test -p agentos-native-sidecar --test service \
  multi_vm_protocol_faults_reconcile_shared_runtime_soak \
  -- --ignored --test-threads=1
```

## Validation history

| Date | Revision | Summary |
| --- | --- | --- |
| 2026-07-18 | `ymrvvnlu` / `63f710d8` | Established post-main baseline; enabled the full ecosystem and npm workflow gates in nightly; recorded 9 ecosystem failures, 17 npm failures, and 13 failing Rust targets. |
| 2026-07-19 | `ymrvvnlu` | Expanded the catalog to 61 fixtures, added agent-oriented and native-negative coverage, fixed conditional/peer/transitive layouts, RivetKit, SSE, npm/npx launch and extraction, and reached 27/27 npm workflows with the full catalog green under its explicit pass/fail/skip contracts. |
| 2026-07-19 | `ymrvvnlu` / `c965ae5b` | Fixed standalone and Socket.IO WebSockets, workspace links, OpenPGP defaults, synchronous `require(esm)`, jsdom, and bounded large-file reads; retained Astro/Next/Vite as explicit deferred framework-build probes pending the full rerun. |
| 2026-07-19 | `ymrvvnlu` / `9d98be76` | Completed the full catalog at 58 passed/4 skipped and npm workflows at 27/27; confirmed WebSocket and forced Socket.IO upgrades; fixed aggregate cold-install, Next wrapper, npm online, and loaded integration harness deadlines; passed build, typecheck, non-browser Cargo check, focused Rust interop/bridge tests, fixed-version verification, and nightly YAML parsing. |
| 2026-07-19 | `ymrvvnlu` | Finalized 66 catalog cases at 62 passed/4 skipped and reran all 27 npm workflows; fixed Next production builds, CommonJS main-module semantics, async exit codes, child IPC lifecycle, and secure WebSocket IP/SNI handling; retained only explicit native-addon, esbuild-service, or unavailable-tool skips. |
| 2026-07-19 | `ymrvvnlu` | Expanded to 75 catalog cases at 69 passed/6 skipped and reran all 27 npm workflows; added Vercel AI/platform, React, Workflow, E2B, Mastra, Effect, Slack, Prisma, Supabase, Browse CLI, and Turbopack contracts; fixed global WebSocket, ESM interop, promisification, package-export specificity, URL/random APIs, crypto exports, and safe inspector loading. |
| 2026-07-19 | `ymrvvnlu` | Expanded to 80 fixtures: 74 executable contracts passed and 6 skipped (75 passed/6 skipped including discovery); reran all 27 npm workflows; added pnpm, Yarn Classic/Modern, TypeScript, 10 behavioral developer CLIs, and Jest 30 native-resolver coverage; fixed CLI stdio, nested-CJS dynamic imports, relative sync file I/O, builtin ESM exports, explicit nonzero pass contracts, and the inspector regression assertion. Workspace typecheck passed 146/146 tasks. |
| 2026-07-19 | `ymrvvnlu` | Expanded to 92 fixtures: 86 executable contracts passed and 6 skipped (87 passed/6 skipped including discovery) in 773.69s; reran all 27 npm workflows in 395.41s; added real Vitest and Mocha suites, WASM/native Rollup contracts, common CLI foundations, Node test-runner and modern native-tool blockers; fixed npm-bin symlink main resolution, writable workspace ownership, relative child filesystem behavior, and late execution teardown. |
| 2026-07-19 | `ymrvvnlu` / base `9d24346f` | Rebased onto current `main`, carried the expensive-suite rename to `*.nightly.test.ts`, and reran focused Vitest, WebSocket, npm-init/npx, npm-bin, typecheck, Cargo, formatting, manifest, mirror, and workflow gates successfully. |
