import type { ActorHandle } from "rivetkit/client";
import { createAgentOS } from "../actor.js";

const definition = createAgentOS();
declare const actions: ActorHandle<typeof definition>;

void actions.process.exec("printf nested");
void actions.process.execFile("printf", ["nested"]);
void actions.process.spawn("node", ["server.js"]);
void actions.createContext("analysis");
void actions.contexts.list();
void actions.javascript.execute("console.log('nested')");
void actions.javascript.spawn("setInterval(() => {}, 1000)");
void actions.typescript.check("const answer: number = 42");
void actions.javascript.npm.runScript("test");
void actions.python.execute("print('nested')");
void actions.python.spawn("while True: pass");
void actions.terminal.open();
void actions.filesystem.readFile("/workspace/example.txt");
void actions.network.httpRequest({ port: 3000, path: "/" });
void actions.software.list();
void actions.agents.list();
void actions.sessions.list();
void actions.cron.list();

// Established flat actions remain as compatibility aliases.
void actions.exec("printf legacy");
void actions.openShell();
void actions.readFile("/workspace/example.txt");
void actions.process.exec("printf nested").then((result) => result.stdout);
void actions.exec("printf legacy").then((result) => result.stdout);
// @ts-expect-error legacy exec preserves KernelExecResult, not execution lifecycle metadata.
void actions.exec("printf legacy").then((result) => result.executionId);

// New language-execution routes are nested rather than duplicated as flat
// actor actions.
// @ts-expect-error executeJavaScript was introduced in this change.
void actions.executeJavaScript("console.log('flat')");
// @ts-expect-error Context lifecycle is nested.
void actions.getExecution("execution-id");
