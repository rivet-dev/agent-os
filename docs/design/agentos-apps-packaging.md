# agentOS Apps Packaging

Status: implemented and validated on 2026-07-24.

The production builder is `@agentos-software/apps-builder`. Turbo builds its
generated `dist/package.aospkg` before `@rivet-dev/agentos-apps`; the artifact
is gitignored but included in the published builder npm package. The current
builder package is 14.2 MB uncompressed and 3.6 MB inside its npm tarball.
The focused shell package is 2.9 MB uncompressed and 1.0 MiB inside its npm
tarball; the build VM no longer mounts the 67.3 MB coreutils package.

Validation reduced the real RivetKit 2.3.9 fixture from 40,967,331 bytes to
4,889,457 bytes. End-to-end tests cover a package-free app, RivetKit HTTP,
DirectActor state, two-replica autoscaling and routing, and bounded load. The
builder tests also inspect the isolated release rather than resolving
dependencies from the repository.

This document records the agreed packaging model for agentOS Apps. The central
rule is:

> Tenant dependencies exist only in a disposable build VM. Execution replicas
> receive a minimal, immutable App Bundle and never install or build anything.

The public `deployApp()` API does not expose the bundler, artifact format, or
build-VM configuration.

## Public API

Packaging remains an implementation detail behind the existing API:

```ts
await deployApp({
	appId: "hello",
	source: new URL("../fixtures/app", import.meta.url),
});
```

Generated applications continue to use the in-memory file form:

```ts
await deployApp({
	appId: "hello",
	files,
});
```

The common API must not add `bundler`, `runtime`, `artifact`, `minify`, or
builder-package options. agentOS Apps owns the build conventions and their sane
defaults.

## Packaging Flow

```text
deployApp({ appId, source/files })
              |
              v
source stored in the stable app actor's SQLite
              |
              v
temporary AgentOS build VM
  - mounts platform-owned apps-builder.aospkg
  - mounts the platform POSIX shell for package build scripts
  - writes tenant source into /workspace
  - runs npm ci/install for tenant dependencies
  - runs npm run build when defined
  - generates the AgentOS HTTP runner
  - bundles runner + server code + JavaScript dependencies
  - emits imported WASM/binary modules separately
  - collects static assets
              |
              v
minimal /release directory
  main.mjs
  modules/*
  public/*
  manifest.json
              |
              v
pack /release as an immutable .aospkg
              |
              v
store checksummed .aospkg chunks in app actor SQLite
              |
              v
replica rehydrates and mounts .aospkg at /app
              |
              v
node /app/main.mjs
```

This is equivalent to a multi-stage Docker build: the AgentOS build VM is the
builder stage, and the release `.aospkg` is the minimal final image.

## Platform Build Package

The build tool must be platform-owned and automatically available. Tenants must
not install it through their own `package.json`.

Add a software package with a name such as:

```text
@agentos-software/apps-builder
```

`@rivet-dev/agentos-apps` depends on that package in the same way it currently
depends on `@agentos-software/tar`. Package scripts use the focused
`@agentos-software/sh` package instead of mounting the full coreutils command
set. Each package exports a `SoftwarePackageRef`, and only the agentOS Apps
build VM includes them in `software`:

```ts
const buildVmOptions = {
	defaultSoftware: false,
	software: [sh, tar, appsBuilder],
	// Existing bounded permissions and limits.
};
```

The packed software payload contains the pinned build program and everything it
needs:

```text
apps-builder.aospkg
  build-app.mjs
  esbuild-wasm JavaScript support
  esbuild.wasm
  package metadata
```

The exact projected package path should come from package resolution rather than
being duplicated as an arbitrary versioned string. The VM invokes the JavaScript
entrypoint with its existing Node runtime.

Important lifecycle properties:

- The host installs the Apps builder transitively with agentOS Apps.
- Every build VM mounts the same immutable, read-only software package.
- No deployment downloads the platform bundler into the tenant workspace.
- Serving replicas do not mount the builder package.
- The release `.aospkg` never contains the builder.
- The builder version and configuration participate in the release hash.
- Updating the builder invalidates build-cache keys.
- Do not add the builder to the global AgentOS base layer; ordinary VMs and
  serving replicas do not need its relatively large WASM compiler.

## Build Tool

Use the mainstream esbuild build model rather than creating a new compiler or
framework build system:

- Native isolated Linux builders may use ordinary native `esbuild`.
- The current AgentOS build VM should use `esbuild-wasm`, because the normal
  `esbuild` npm package launches a platform-specific native executable.
- Both implementations must produce the same App Bundle contract.
- The choice between native and WASM esbuild remains internal and can change
  without changing `deployApp()` or the execution replicas.

The direct `esbuild-wasm` API is validated in a real AgentOS build VM. The
builder uses esbuild's in-process browser service with `worker: false`; its
Node entrypoint launches a child-process service that is unnecessary inside
the VM and previously made failed builds stall.

The platform-owned build program should roughly:

1. Accept a generated runner entrypoint, workspace root, release output
   directory, and bounded build settings.
2. Bundle for the AgentOS Node runtime as ESM.
3. Set `NODE_ENV` to `production` for dead-code elimination.
4. Enable tree shaking and minification.
5. Emit an external source map for diagnostics, but keep it outside the runtime
   package.
6. Emit recognized WASM and binary imports as separate files.
7. Return a metafile describing every generated input and output.
8. Hash the output files and write the App Bundle manifest.

The generated AgentOS runner, rather than the tenant entrypoint alone, is the
bundle entrypoint. This ensures the HTTP adapter and the tenant's imports share
one module graph and one RivetKit module identity.

## App Bundle

The logical runtime output is:

```text
/release
  main.mjs
  modules/
    <name>-<content-hash>.wasm
    <name>-<content-hash>.bin
  public/
    index.html
    assets/*
  manifest.json
```

Not every release needs every directory. A server-only release may contain only
`main.mjs`, while a static site may contain a small generated runner and
`public/`.

The internal manifest should be versioned and simple:

```ts
interface AppBundleManifest {
	version: 1;
	mainModule: string;
	modules: Array<{
		path: string;
		type: "esm" | "wasm" | "text" | "data";
		size: number;
		hash: string;
	}>;
	assets: Array<{
		path: string;
		size: number;
		hash: string;
	}>;
}
```

This manifest is internal. Users do not construct or upload it directly in the
initial API.

The release package must not contain:

```text
node_modules/
src/
package-lock.json
tsconfig.json
platform build tools
unused package files
```

SQLite continues to retain the submitted source separately for release history
and rebuilding. The execution artifact contains only runtime outputs.

## Module And Asset Discovery

Do not recursively scan `node_modules` for files with interesting extensions.
Packages often ship browser, debug, test, and architecture-specific payloads
that are not used at runtime.

Use three bounded rules instead.

### Imported modules

The bundler follows statically analyzable imports:

```ts
import dependency from "dependency";
import schema from "./schema.json";
import query from "./query.sql";
import wasmPath from "./engine.wasm";
```

JavaScript, TypeScript, and JSON join the bundle. Recognized WASM and binary
imports become hashed files under `modules/`. Small text-like module types may
be inlined.

Literal dynamic imports are supported. Computed imports and opaque filesystem
paths are not generally discoverable by any bundler.

### Static assets

Use conventional static output directories rather than guessing arbitrary
files:

- a package-free root static site;
- `dist/` when a frontend build produces `dist/index.html`;
- `public/` for explicitly public application assets.

Static paths, sizes, and hashes are recorded in the manifest. Initially, include
the required bytes in the release `.aospkg`. The manifest permits a future
content-addressed asset store without changing the user API.

Do not implement a multi-step asset upload session or JWT protocol for the
proof of concept. The stable app actor already receives and durably owns the
complete file tree.

### Non-analyzable runtime files

Computed imports and arbitrary `fs.readFile(runtimeValue)` cannot be packaged
reliably without an explicit convention. The initial behavior should fail with
a bounded, typed build error that identifies the unresolved dependency.

An advanced module-rule escape hatch may be added when a concrete application
requires it. It is not part of the initial common API.

## RivetKit

RivetKit receives a built-in packaging adapter because it is a first-class
agentOS Apps use case.

The target is:

```text
main.mjs                         bundled app + RivetKit JavaScript
modules/rivetkit-<hash>.wasm     RivetKit runtime
```

Do not retain the RivetKit npm package tree, NAPI bindings, Engine CLI, or
AgentOS host integrations in the release.

Prefer an upstream RivetKit surface that makes its WASM import statically
analyzable or accepts preloaded WASM bindings. Until that is available, the
Apps builder may explicitly resolve and emit the one known RivetKit WASM module.
This is a narrow platform adapter, not a generic `node_modules` scan.

The generated runner initializes the emitted WASM bytes before importing or
starting the guest registry. `RIVETKIT_RUNTIME=wasm` and serverless runtime mode
remain enforced by the replica.

## Storage And Replica Lifecycle

The stable app actor's SQLite database remains the durable source of truth:

```text
submitted source BLOBs
release metadata
immutable checksummed .aospkg chunks
```

A replica:

1. Downloads the immutable artifact chunks.
2. Validates total bytes and SHA-256.
3. Writes a replica-scoped temporary `.aospkg`.
4. Mounts it read-only at `/app`.
5. Starts `node /app/main.mjs`.
6. Keeps the temporary package while lazy mount readers may exist.
7. Removes it after the VM is disposed.

Replicas never run npm, a framework build, or the platform bundler.

## Release Identity

The release hash must cover:

- normalized submitted source;
- selected tenant entrypoint and static root;
- tenant build configuration already used by the platform;
- generated runner semantics;
- App Bundle manifest version;
- Apps builder package version;
- bundler version and material options;
- RivetKit packaging-adapter version.

Changing packaging semantics must not reuse an artifact built under older
semantics.

## Security And Limits

Tenant source, dependencies, build scripts, and bundler inputs remain untrusted.
They execute inside the bounded build VM, not in the trusted app actor process.

Keep or add explicit limits for:

- source files and bytes;
- dependency count;
- build duration;
- process count and open file descriptors;
- V8 heap;
- build filesystem bytes;
- bundler input and output bytes;
- emitted module and asset counts;
- individual emitted file size;
- total App Bundle size;
- captured diagnostics and source-map size.

Threshold warnings and typed errors must identify the configured limit and how
to raise it. Build failures must leave the previous active release unchanged.

## Acceptance Criteria

The packaging change is complete when tests prove:

1. A plain JavaScript HTTP app bundles and serves without runtime
   `node_modules`.
2. A TypeScript app runs its build, bundles the output, and reports bounded
   compiler diagnostics on failure.
3. A real RivetKit app serves HTTP and DirectActor calls using its emitted WASM
   module.
4. RivetKit actor state survives replica replacement because state remains in
   Rivet, not the release filesystem.
5. A static website serves HTML, JavaScript, CSS, and binary assets.
6. An imported WASM fixture is emitted as a separate hashed runtime module.
7. The release archive contains only the manifest, bundle outputs, and required
   assets.
8. The release archive contains no tenant `node_modules`, source tree, lockfile,
   or Apps builder.
9. A cold replica rehydrates the minimal artifact from SQLite and becomes
   healthy without npm or network access.
10. Repeating a deployment with the same source and builder version reuses the
    same release identity.
11. Changing the builder or manifest version invalidates the release identity.
12. Unsupported computed imports or opaque runtime files fail with a clear
    typed error.
13. Artifact size is recorded in tests so the RivetKit fixture cannot silently
    regress back to shipping its production dependency tree.

## References

- [Cloudflare Wrangler bundling](https://developers.cloudflare.com/workers/wrangler/bundling/)
- [Cloudflare multipart Worker upload metadata](https://developers.cloudflare.com/workers/configuration/multipart-upload-metadata/)
- [Cloudflare Workers for Platforms static assets](https://developers.cloudflare.com/cloudflare-for-platforms/workers-for-platforms/configuration/static-assets/)
- [Cloudflare Workers versions and deployments](https://developers.cloudflare.com/workers/versions-and-deployments/)
