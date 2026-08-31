import path from "node:path";
import { fileURLToPath } from "node:url";
// docs:start virtual-filesystem
import { AgentOs, createHostDirBackend } from "@rivet-dev/agentos-core";

// Boot a fully virtualized VM. The guest filesystem lives entirely inside the
// kernel - writes never touch the host disk.
const rt = await AgentOs.create();

try {
	// Guest code runs as an ES module inside the VM. It writes and reads files
	// using the standard node `fs` module, backed by the kernel's virtual
	// filesystem.
	const {
		stdout = "",
		stderr = "",
		exitCode,
	} = await rt.javascript.execute(
		`
    import { mkdirSync, writeFileSync, readFileSync, readdirSync } from "node:fs";

    mkdirSync("/workspace", { recursive: true });
    writeFileSync("/workspace/hello.txt", "hello from the VM");

    const message = readFileSync("/workspace/hello.txt", "utf8");
    const entries = readdirSync("/workspace");

    console.log("read back from VM:", message);
    console.log("/workspace contents:", JSON.stringify(entries));
	  `,
		{ output: { capture: "all" } },
	);

	console.log("exitCode:", exitCode);
	if (stderr.trim()) console.log("guest stderr:", stderr.trim());
	console.log("guest stdout:");
	console.log(stdout.trim());
} finally {
	await rt.dispose();
}
// docs:end virtual-filesystem

// Mounting a host directory into the VM. `mounts` projects a real host
// directory into the guest filesystem, Docker-style. Files are read lazily
// through the VFS, and the guest sees only the mounted subtree.
const hostDir = path.join(
	path.dirname(fileURLToPath(import.meta.url)),
	"..",
	"host-data",
);

const mounted = await AgentOs.create({
	mounts: [
		{
			path: "/mnt/host-data",
			plugin: createHostDirBackend({ hostPath: hostDir, readOnly: true }),
			readOnly: true,
		},
	],
});

try {
	// The guest reads a file that lives on the real host, exposed read-only at
	// the mount point. Nothing else on the host is visible.
	const {
		stdout = "",
		stderr = "",
		exitCode,
	} = await mounted.javascript.execute(
		`
    import { readFileSync, readdirSync } from "node:fs";

    const entries = readdirSync("/mnt/host-data");
    const greeting = readFileSync("/mnt/host-data/greeting.txt", "utf8");

    console.log("mounted contents:", JSON.stringify(entries));
    console.log("guest read host file:", greeting.trim());
	  `,
		{ output: { capture: "all" } },
	);

	console.log("[mount] exitCode:", exitCode);
	if (stderr.trim()) console.log("[mount] guest stderr:", stderr.trim());
	console.log("[mount] guest stdout:");
	console.log(stdout.trim());
} finally {
	await mounted.dispose();
}
