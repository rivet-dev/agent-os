// Run a Python program inside an AgentOS VM.

import { AgentOs } from "@rivet-dev/agentos";

const vm = await AgentOs.create();

await vm.filesystem.writeFile(
	"/tmp/demo.py",
	`import json
import pathlib
import subprocess
import urllib.request

workspace = pathlib.Path("/workspace")
workspace.mkdir(parents=True, exist_ok=True)
(workspace / "answer.json").write_text(json.dumps({"answer": 42}))

child = subprocess.run(
    ["sh", "-c", "printf 'hello from a child process'"],
    check=True,
    capture_output=True,
    text=True,
)

print((workspace / "answer.json").read_text())
print(child.stdout)
`,
);

const result = await vm.process.exec("python /tmp/demo.py");
console.log(result.stdout);
console.log("Exit code:", result.exitCode);

await vm.dispose();
