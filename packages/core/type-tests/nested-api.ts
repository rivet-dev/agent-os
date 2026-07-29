import type { AgentOs } from "../src/agent-os.js";
import type {
	CodeExecutionResult,
	ProcessDescriptor,
} from "../src/language-execution.js";

declare const vm: AgentOs;

async function checkNestedApi(): Promise<void> {
	const attached: CodeExecutionResult = await vm.process.exec("true");
	const spawned: ProcessDescriptor = await vm.process.spawn("true", []);
	void attached;
	void spawned;

	await vm.process.execFile("true", []);
	vm.process.get(1);
	vm.process.list();
	vm.process.tree();
	await vm.process.wait(1);
	await vm.process.signal(1, "SIGTERM");
	vm.process.kill(1);
	await vm.process.writeStdin(1, "input");
	await vm.process.closeStdin(1);
	await vm.process.resizePty(1, { cols: 80, rows: 24 });
	await vm.process.readOutput(1);

	await vm.createContext("context");
	await vm.contexts.get("context");
	await vm.contexts.list();
	await vm.contexts.reset("context");
	await vm.contexts.delete("context");
	await vm.javascript.execute("");
	await vm.javascript.evaluate("1");
	await vm.javascript.executeFile("/workspace/main.js");
	await vm.javascript.spawn("");
	await vm.javascript.spawnFile("/workspace/main.js");
	await vm.typescript.execute("");
	await vm.typescript.evaluate("1");
	await vm.typescript.executeFile("/workspace/main.ts");
	await vm.typescript.spawn("");
	await vm.typescript.spawnFile("/workspace/main.ts");
	await vm.typescript.check("");
	await vm.typescript.checkProject();
	await vm.javascript.npm.install();
	await vm.javascript.npm.runScript("test");
	await vm.javascript.npm.runPackage("typescript");

	await vm.python.execute("");
	await vm.python.evaluate("1");
	await vm.python.executeFile("/workspace/main.py");
	await vm.python.executeModule("main");
	await vm.python.spawn("");
	await vm.python.spawnFile("/workspace/main.py");
	await vm.python.spawnModule("main");
	await vm.python.install();

	// @ts-expect-error Spawned processes always use a fresh realm.
	await vm.javascript.spawn("", { contextId: "context" });
	// @ts-expect-error The old execution namespace was removed.
	await vm.executions.list();
	// @ts-expect-error TypeScript is a top-level namespace.
	await vm.javascript.typescript.execute("");
	// @ts-expect-error The old retained-state option was removed.
	await vm.javascript.execute("", { executionId: "context" });
	// @ts-expect-error Contexts must be created explicitly.
	await vm.javascript.execute("", { createIfMissing: true });
	// @ts-expect-error Backgrounding uses spawn.
	await vm.javascript.execute("", { detached: true });

	const terminal = vm.terminal.open();
	await vm.terminal.write(terminal.shellId, "input");
	vm.terminal.resize(terminal.shellId, 80, 24);
	await vm.terminal.wait(terminal.shellId);
	vm.terminal.close(terminal.shellId);

	await vm.filesystem.readFile("/workspace/file");
	await vm.filesystem.writeFile("/workspace/file", "content");
	await vm.filesystem.readFiles(["/workspace/file"]);
	await vm.filesystem.writeFiles([
		{ path: "/workspace/file", content: "content" },
	]);
	await vm.filesystem.stat("/workspace/file");
	await vm.filesystem.mkdir("/workspace/dir");
	await vm.filesystem.readdir("/workspace");
	await vm.filesystem.readdirEntries("/workspace");
	await vm.filesystem.readdirRecursive("/workspace");
	await vm.filesystem.exists("/workspace/file");
	await vm.filesystem.move("/workspace/from", "/workspace/to");
	await vm.filesystem.remove("/workspace/file");
	await vm.filesystem.export({ maxBytes: 1024 });
	await vm.filesystem.unmount("/workspace/mount");
	await vm.filesystem.listMounts();

	await vm.software.list();
	await vm.agents.list();
	await vm.sessions.get();
	await vm.sessions.list();
	await vm.sessions.delete();
	await vm.sessions.unload();
	await vm.sessions.cancelPrompt();
	await vm.sessions.readHistory();
	await vm.sessions.getConfig();
	await vm.sessions.getCapabilities();
	await vm.sessions.getAgentInfo();
	vm.cron.list();

	// New execution APIs were renamed, not retained as flat aliases.
	// @ts-expect-error Use javascript.execute().
	vm.executeJavaScript("");
	// @ts-expect-error Use python.execute().
	vm.executePython("");
	// @ts-expect-error Use contexts.list().
	vm.listExecutions();
}

void checkNestedApi;
