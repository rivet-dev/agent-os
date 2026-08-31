# Zid × agentOS dylib integration — issue tracker

Living tracker for the Zid team's dylib-preview migration. Covers **their** reported
blockers and **our** (agentOS / secure-exec) bugs surfaced while investigating them.

**Versions under test (their pins):**
- `@rivet-dev/agentos*` / `agentos-core` / `agentos-pi` / `agentos-sidecar`: `0.0.0-integrate-dylib-into-main.815fcda`
- `rivetkit`: `0.0.0-feat-dylib-actor-plugin.c44621f`
- `@agentos-software/common`: `0.3.0-rc.2`; `@secure-exec/core`: `0.3.0`
- Their runtime: Node 22, **linux-x64-gnu prebuilts under OrbStack + Rosetta x86-emulation on macOS** (prod = Railway, native linux-x64).

> Note: the dylib stack lives on the `integrate-dylib-into-main` branch (HEAD = `815fcda`),
> **not** on `main` yet. `815fcda` is what their version is built from.

## How they integrate
They do **not** use the native `agentOs()` actor. They built their own RivetKit JS actor
holding an `AgentOs` core instance (`c.vars.agentOs`) and drive it directly (the removed
`rivetkit/agent-os` actor's replacement). ~12 custom actions, a host toolkit (in-VM Pi
extension → HTTP to host), and server-side `onSessionEvent`.

## Reproduction status (key finding)
Reproduced on **native linux-x64** with their pinned versions, escalating to **their actual code**:
1. Faithful replay of their actor sequence (create → seed writes → their three
   `createInMemoryFileSystem` JS-driver mounts → `createSession("pi")` with `cwd:"/workspace"`
   and `cwd:"/"`) — **all pass.**
2. **Their actual custom adapter, bundled with their actual `build-adapter.mjs`** (Proxy-stubbing
   8 packages + `eval(require())` + minify), swapped over the stock `agentos-pi` adapter →
   `createSession("pi")` reaches `session/new` and returns a sessionId in ~1.4s. **No chdir ENOENT.**
3. **Their adapter UNBUNDLED** (runtime `import "@agentclientprotocol/sdk"`) → also resolves and
   `createSession` succeeds. (The *stock* dylib adapter likewise imports `@agentclientprotocol/sdk`
   at runtime and resolves.)

**So Q1–Q4 do NOT reproduce on native, even with their exact adapter + flow + mounts.** Their
blockers are **environment-specific**. Two root causes:
- **Q1 (resolution)** = **node_modules hoist layout.** Core mounts the agent package's hoisted
  `node_modules` tree at `/root/node_modules`; the adapter resolves `@agentclientprotocol/sdk` by
  walking up from `/root/node_modules/@rivet-dev/agentos-pi`. In a **flat npm install** the dep is
  hoisted top-level → resolves. In their monorepo install it isn't on that chain → "not found."
  Bundling (their workaround) is correct; alternatively mount/hoist the dep onto the chain.
- **Q2/Q3/Q4 (empty guest FS / no sh / writes invisible)** = **no native reproduction and no code
  explanation** → the remaining uncontrolled variable is **OrbStack + Rosetta x86-emulation** of
  the linux-x64 prebuilt (guest VFS/mount/exec syscalls misbehaving under emulation). Could be a
  single shared cause (the sidecar's mount/VFS layer failing under emulation, which would explain
  all three at once). **Not directly reproduced** (this host is native x64, can't run Rosetta).

**Decisive test for them:** run on **native linux-x64** (their Railway prod is native
linux-x64-glibc). If it works there but fails in OrbStack/Rosetta → emulation confirmed.

Their own repro scripts (`scripts/diag-adapter.mjs`, `scripts/smoke-agentos.mjs`) drive their
rivetkit server + swapped adapter; the VM flow + adapter they wrap is what was reproduced here.

---

## Their reported blockers

| # | Issue | Finding | Status |
|---|---|---|---|
| 1 | **Q0** — native `agentOs()` actor can't host their custom actions / host toolkit / `onSessionEvent`; is wrapping the core class supported? | Yes — core-direct is the documented pattern (all 13 quickstarts). All 4 of their native-actor claims verified TRUE (`actions:{}`, no JS callbacks, callbacks parsed-and-dropped, `toolKits` not serialized). | ✅ Answered |
| 2 | **Q1** — adapter `import @agentclientprotocol/sdk` → `_resolveModule returned non-string` | **Root cause: node_modules hoist layout.** Reproduced both stock & their adapter resolving the dep when hoisted flat; "not found" means it isn't on the `/root/node_modules` chain core mounts. Bundling (their workaround) is correct; else hoist/mount the dep. | ◐ Root-caused; error-message fix in **secure-exec PR #114** (diagnostics only) |
| 3 | **Q2** — `chdir` ENOENT for every path incl `/` | Base rootfs **is** provisioned — proven on their version with **their actual bundled adapter** + full flow; `createSession` reaches `session/new`. Not reproducible on native → **Rosetta x86-emulation** is the leading cause. | ✅ Diagnosed (not an SDK bug) |
| 4 | **Q3** — `command not found: sh` | `sh`/`bash` **do** ship (in `@agentos-software/coreutils`, inside `common`); works in repro. Their "common dropped sh" belief is wrong (only the package *description* omits "sh"). Likely same env (mount layer under emulation). | ✅ Diagnosed (not an SDK bug) |
| 5 | **Q4** — host `writeFile`/`mkdir` not visible to guest | Visible core-direct, **no mount required** (proven w/ their flow). Likely write-after-`createSession` ordering, a shadowing mount, or the same emulation env. | ✅ Diagnosed (not an SDK bug) |
| 6 | **Q5** — inherent to the VM model or core-class-specific? | **Neither** — full core-direct path (incl. `createSession("pi")`) reproduced working. | ✅ Answered |

---

## Our bugs / gaps (agentOS + secure-exec)

| # | Issue | Impact | Status | Proposed fix |
|---|---|---|---|---|
| 7 | `toolKit→sidecar` runs Zod **v4** `toJSONSchema()`; throws on Zod **v3** schemas | **Why they dropped `toolKits`** and went host-tools-over-HTTP | 🟡 Open | accept v3 (or convert), or document the constraint |
| 8 | Native actor **silently drops** `onSessionEvent`/`onPermissionRequest`/`onBeforeConnect`/`toolKits` | Silent footgun — users think these are wired | 🟡 Open | throw a clear "unsupported across the native boundary" error (or wire through) |
| 9 | core `AgentOs.create()` ignores the `defaultSoftware` option it documents | Latent; auto-include of `common` is actor-only (`actor.ts:192-200`) | 🟡 Open | honor `defaultSoftware` in core, or fix the JSDoc |
| 10 | `withAutoAgentNodeModulesMount` is **actor-only** — no public helper for core-direct users | Core-direct users with a custom adapter get no node_modules-mount helper (relates to #2) | 🟡 Open | export a public `nodeModulesMount`-style helper / do it in core |
| 11 | Native actor has **no `mountFs`** action and rejects JS-driver mounts (static, serializable Native mounts only) | **Blocks the proxy-actor pattern** from hosting their session/skills JS-driver VFS | 🟡 Open | dynamic `mountFs` (incl. JS-driver) on the native actor |
| 12 | Engine `:6420` vs httpPort `:6421` (`/metadata` 404) | DX confusion they hit | ⚪ Open | document, or client auto-detect |
| 13 | Audit their 11 carried patches for dylib obsolescence — esp. the WASI **read-blocked-as-write** permission typo (their patch 1) | Some patches may be obsolete; the WASI one is a real correctness bug if it survived the move into secure-exec | ⚪ Not audited | audit + confirm against `0.3.0` |
| 14 | Misleading `_resolveModule returned non-string` error (really "not found") | Sent them down the bundling path (#2) | ✅ **secure-exec PR #114 (open)** | reworded + main-only regression test |

**Status key:** ✅ done/answered · ◐ partial · 🟡 our bug, identified, not fixed · ⚪ not started

---

## Reproduction recipe
On native linux-x64 (NOT Rosetta), from public npm:
```
npm i @rivet-dev/agentos-core@0.0.0-integrate-dylib-into-main.815fcda \
      @rivet-dev/agentos-pi@0.0.0-integrate-dylib-into-main.815fcda \
      @agentos-software/common@0.3.0-rc.2
```
```js
import { AgentOs, createInMemoryFileSystem } from "@rivet-dev/agentos-core";
import common from "@agentos-software/common";
import pi from "@rivet-dev/agentos-pi";
const vm = await AgentOs.create({ software: [common, pi] });
console.log((await vm.exec("ls -la / && sh -c 'echo SH_OK' && pwd")).stdout); // base rootfs + sh
await vm.writeFile("/home/user/x.txt", "hi");
console.log((await vm.exec("cat /home/user/x.txt")).stdout);                  // host write visible, no mount
for (const p of ["/home/user/.pi/agent/sessions","/app/skills"]) vm.mountFs(p, createInMemoryFileSystem());
console.log((await vm.createSession("pi", { cwd: "/workspace", env: { HOME:"/home/user" } })).sessionId); // works
await vm.dispose();
```
All of the above succeed → Q1–Q4 are not SDK bugs.
