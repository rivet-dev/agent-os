import pi from "@agentos-software/pi";
import { AgentOs, nodeModulesMount } from "@rivet-dev/agentos-core";

// Common embedded AgentOs.create() configuration. The agentOS() actor accepts
// the same options and layers persistence, sleep/wake, and preview URLs on top.
const vm = await AgentOs.create({
	// Durable SQLite storage for the root filesystem and the session catalog.
	// Omit it for an in-memory VM that keeps nothing after dispose().
	database: { type: "sqlite_file", path: ".agentos/agentos.sqlite" },
	// Filesystems to mount at boot. Use nodeModulesMount() to expose a host
	// node_modules tree at /root/node_modules.
	mounts: [nodeModulesMount("/path/to/project/node_modules")],
	// Kernel permission policy (see /agentos/docs/permissions) and runtime caps
	// (see /agentos/docs/resource-limits) take the same values as the actor.
	// `bindings` does too (see /agentos/docs/bindings).
	permissions: { network: "allow" },
	limits: { jsRuntime: { v8HeapLimitMb: 128 } },
	// Software packages to install in the VM (see /agentos/docs/software).
	software: [pi],
	// Also install the default software bundle (sh + coreutils). Defaults to true;
	// set false for a bare VM with only the software you list.
	defaultSoftware: true,
	// Ports exempt from SSRF checks (for testing against host-side mock servers)
	loopbackExemptPorts: [3000],
	// Sidecar placement defaults to the shared `default` pool.
	sidecar: { kind: "shared" },
});

await vm.dispose();
