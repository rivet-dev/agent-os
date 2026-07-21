import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create({ permissions: { network: "allow" } });

try {
	// Filesystem: pathlib and os use the VM's persistent filesystem.
	const files = await runtime.python.execute(
		`
from pathlib import Path

Path("/workspace/data.txt").write_text("hello")
print(Path("/workspace/data.txt").read_text())
		`,
		{ output: { capture: "all" } },
	);
	console.log(files.stdout); // "hello\n"

	// Process trees: subprocess spawns real guest processes.
	const processes = await runtime.python.execute(
		`
import subprocess

subprocess.run(["ls", "-la", "/workspace"], check=True)
		`,
		{ output: { capture: "all" } },
	);
	console.log(processes.stdout); // Directory listing for /workspace

	// Networking: sockets and HTTP clients go through the VM network policy.
	const status = await runtime.python.execute(
		`
import urllib.request

print(urllib.request.urlopen("https://example.com").status)
		`,
		{ output: { capture: "all" } },
	);
	console.log(status.stdout); // "200\n"
} finally {
	await runtime.dispose();
}
