import { resolve } from "node:path";
import { afterEach, describe, expect, test } from "vitest";
import { AgentOs, type Permissions } from "../src/index.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const BROWSER_BASE_API_KEY = process.env.BROWSER_BASE_API_KEY ?? "";
const BROWSER_BASE_PROJECT_ID = process.env.BROWSER_BASE_PROJECT_ID ?? "";
const HAS_BROWSERBASE_CREDENTIALS = Boolean(
	BROWSER_BASE_API_KEY && BROWSER_BASE_PROJECT_ID,
);

const BROWSERBASE_PERMISSIONS: Permissions = {
	fs: "allow",
	childProcess: "allow",
	env: "allow",
	network: {
		default: "deny",
		rules: [
			{
				mode: "allow",
				patterns: ["dns://*.browserbase.com", "tcp://*.browserbase.com:*"],
			},
		],
	},
};

const GUEST_SCRIPT = String.raw`
import WebSocket from "ws";

function ensure(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

const apiKey = process.env.BROWSERBASE_API_KEY;
const projectId = process.env.BROWSERBASE_PROJECT_ID;

const createResponse = await fetch("https://api.browserbase.com/v1/sessions", {
  method: "POST",
  headers: {
    "x-bb-api-key": apiKey,
    "content-type": "application/json",
  },
  body: JSON.stringify({
    projectId,
    browserSettings: {
      viewport: { width: 1288, height: 711 },
    },
    userMetadata: {
      agent_os_browserbase_ws_test: "true",
    },
  }),
  signal: AbortSignal.timeout(30_000),
});
if (!createResponse.ok) {
  throw new Error(
    "Browserbase session create failed with " +
      createResponse.status +
      ": " +
      (await createResponse.text()),
  );
}

const created = await createResponse.json();
ensure(created.id, "missing session id");
ensure(created.connectUrl, "missing connectUrl");

const cdpReply = await new Promise((resolve, reject) => {
  const socket = new WebSocket(created.connectUrl, {
    headers: {
      "User-Agent": "agent-os-browserbase-ws-test",
    },
  });
  const timer = setTimeout(() => {
    reject(new Error("timed out waiting for Browserbase CDP reply"));
  }, 15_000);

  socket.once("open", () => {
    socket.send(JSON.stringify({ id: 1, method: "Browser.getVersion", params: {} }));
  });

  socket.once("message", (data) => {
    clearTimeout(timer);
    try {
      socket.close();
    } catch {}
    resolve(data.toString());
  });

  socket.once("error", (error) => {
    clearTimeout(timer);
    reject(error);
  });
});

const releaseResponse = await fetch(
  "https://api.browserbase.com/v1/sessions/" + created.id,
  {
    method: "POST",
    headers: {
      "x-bb-api-key": apiKey,
      "content-type": "application/json",
    },
    body: JSON.stringify({ status: "REQUEST_RELEASE" }),
    signal: AbortSignal.timeout(30_000),
  },
);
if (!releaseResponse.ok) {
  throw new Error(
    "Browserbase session release failed with " +
      releaseResponse.status +
      ": " +
      (await releaseResponse.text()),
  );
}

console.log("BROWSERBASE_CDP_REPLY:" + cdpReply);
`;

function testIf(
	condition: boolean,
	...args: Parameters<typeof test>
): void {
	if (condition) {
		// @ts-expect-error forwarded test() arguments stay runtime-compatible.
		test(...args);
		return;
	}
	const [name] = args;
	test(String(name), () => {});
}

const CLI_PAGES_SCRIPT = String.raw`
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

const BROWSE_PATH = "/root/node_modules/@browserbasehq/browse-cli/dist/index.js";
const CONFIG_DIR = "/tmp/browserbase-cli-debug";

function listFiles(dir, prefix = "") {
  if (!existsSync(dir)) {
    return [];
  }
  const entries = readdirSync(dir).sort();
  const files = [];
  for (const entry of entries) {
    const fullPath = path.join(dir, entry);
    const relativePath = prefix ? path.join(prefix, entry) : entry;
    const stats = statSync(fullPath);
    if (stats.isDirectory()) {
      files.push(...listFiles(fullPath, relativePath));
      continue;
    }
    files.push(relativePath);
  }
  return files;
}

function tailFile(filePath, maxLines = 40) {
  if (!existsSync(filePath)) {
    return "";
  }
  return readFileSync(filePath, "utf8")
    .trim()
    .split("\n")
    .slice(-maxLines)
    .join("\n");
}

function dumpSessionState(session) {
  const prefixes = [
    "sock",
    "pid",
    "ws",
    "mode",
    "mode-override",
    "connect",
    "context",
    "local-config",
    "local-info",
    "lock",
  ];
  return prefixes
    .map((suffix) => {
      const filePath = "/tmp/browse-" + session + "." + suffix;
      if (!existsSync(filePath)) {
        return filePath + ": <missing>";
      }
      const content = tailFile(filePath, 20);
      return filePath + ":\n" + content;
    })
    .join("\n");
}

function runNodeScript(scriptPath, args, label) {
  const result = spawnSync(process.execPath, [scriptPath, ...args], {
    encoding: "utf8",
    env: {
      ...process.env,
      BROWSERBASE_CONFIG_DIR: CONFIG_DIR,
      BROWSERBASE_CDP_CONNECT_MAX_MS: "5000",
      BROWSERBASE_SESSION_CREATE_MAX_MS: "10000",
      STAGEHAND_FIRST_TOP_LEVEL_PAGE_TIMEOUT_MS: "2000",
    },
    timeout: 60_000,
  });
  if (result.error) {
    throw new Error(label + " failed: " + result.error.message);
  }
  if (result.status !== 0) {
    const latestDir = path.join(CONFIG_DIR, "sessions", "latest");
    const logPath = path.join(latestDir, "session_events.log");
    const jsonlPath = path.join(latestDir, "session_events.jsonl");
    const sessionState = dumpSessionState(process.env.BROWSE_SESSION || "default");
    const tracePath = "/tmp/browse-trace.log";
    throw new Error(
      label +
        " exited with " +
        result.status +
        "\nstdout:\n" +
        (result.stdout || "") +
        "\nstderr:\n" +
        (result.stderr || "") +
        "\nconfig files:\n" +
        listFiles(CONFIG_DIR).join("\n") +
        "\nsession_events.log tail:\n" +
        tailFile(logPath) +
        "\nsession_events.jsonl tail:\n" +
        tailFile(jsonlPath) +
        "\nbrowse trace tail:\n" +
        tailFile(tracePath) +
        "\nsession state:\n" +
        sessionState,
    );
  }
  return result.stdout.trim();
}

if (!existsSync(BROWSE_PATH)) {
  throw new Error("missing browse cli path");
}

const pages = runNodeScript(BROWSE_PATH, ["pages", "--json"], "browse pages");
console.log("BROWSERBASE_PAGES:" + pages);
`;

const DIRECT_STAGEHAND_INIT_SCRIPT = String.raw`
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname } from "node:path";

const require = createRequire(import.meta.url);
const browsePath = "/root/node_modules/@browserbasehq/browse-cli/dist/index.js";

if (!existsSync(browsePath)) {
  throw new Error("missing browse cli path");
}

const browseDir = dirname(dirname(browsePath));
const stagehandPath = require.resolve("@browserbasehq/stagehand", {
  paths: [browseDir],
});
const { V3 } = require(stagehandPath);

const steps = [];
function note(step) {
  steps.push(step);
  console.log("STAGEHAND_STEP:" + step);
}

note("construct");
const stagehand = new V3({
  env: "BROWSERBASE",
  verbose: 0,
  disablePino: true,
  disableAPI: true,
  browserbaseSessionCreateParams: {
    userMetadata: {
      agent_os_browserbase_direct_stagehand_test: "true",
    },
  },
});

try {
  note("before-init");
  await stagehand.init();
  note("after-init");
  const pages = stagehand.context.pages().map((page, index) => ({
    index,
    url: page.url(),
    targetId: page.targetId(),
    mainFrameId: page.mainFrameId(),
  }));
  console.log(
    "BROWSERBASE_DIRECT_STAGEHAND:" +
      JSON.stringify({
        steps,
        pageCount: pages.length,
        pages,
      }),
  );
} finally {
  note("before-close");
  await stagehand.close().catch(() => {});
  note("after-close");
}
`;

const BROWSERBASE_SDK_SCRIPT = String.raw`
import { existsSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const sdkPath = "/root/node_modules/.pnpm/@browserbasehq+sdk@2.10.0/node_modules/@browserbasehq/sdk/index.js";

if (!existsSync(sdkPath)) {
  throw new Error("missing browserbase sdk path");
}

const Browserbase = require(sdkPath).default;
const bb = new Browserbase({ apiKey: process.env.BROWSERBASE_API_KEY });

const steps = [];
function note(step) {
  steps.push(step);
  console.log("BROWSERBASE_SDK_STEP:" + step);
}

note("before-create");
const created = await bb.sessions.create({
  projectId: process.env.BROWSERBASE_PROJECT_ID,
  browserSettings: {
    viewport: { width: 1288, height: 711 },
  },
  userMetadata: {
    agent_os_browserbase_sdk_test: "true",
  },
});
note("after-create");

if (!created?.id || !created?.connectUrl) {
  throw new Error("browserbase sdk create returned unexpected payload");
}

let debugUrl = null;
try {
  note("before-debug");
  const debug = await bb.sessions.debug(created.id);
  note("after-debug");
  debugUrl = debug?.debuggerUrl ?? null;
} catch (error) {
  note("debug-error");
  debugUrl = String(error);
}

note("before-release");
await bb.sessions.update(created.id, { status: "REQUEST_RELEASE" });
note("after-release");

console.log(
  "BROWSERBASE_SDK_RESULT:" +
    JSON.stringify({
      steps,
      sessionId: created.id,
      connectUrl: created.connectUrl,
      debugUrl,
    }),
);
`;

const HTTPS_BROWSERBASE_SCRIPT = String.raw`
import https from "node:https";

function requestJson(method, path, body, agent) {
  return new Promise((resolve, reject) => {
    const payload = body ? JSON.stringify(body) : null;
    const req = https.request(
      {
        protocol: "https:",
        hostname: "api.browserbase.com",
        path,
        method,
        agent,
        headers: {
          "x-bb-api-key": process.env.BROWSERBASE_API_KEY,
          "content-type": "application/json",
          ...(payload ? { "content-length": Buffer.byteLength(payload) } : {}),
        },
      },
      (res) => {
        let responseBody = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          responseBody += chunk;
        });
        res.on("end", () => {
          resolve({
            statusCode: res.statusCode ?? 0,
            body: responseBody,
          });
        });
      },
    );

    req.setTimeout(15_000, () => {
      req.destroy(new Error("https request timeout"));
    });
    req.on("error", reject);
    if (payload) {
      req.write(payload);
    }
    req.end();
  });
}

const agent = new https.Agent({ keepAlive: true });
console.log("HTTPS_BROWSERBASE_STEP:before-create");
const createResponse = await requestJson(
  "POST",
  "/v1/sessions",
  {
    projectId: process.env.BROWSERBASE_PROJECT_ID,
    browserSettings: {
      viewport: { width: 1288, height: 711 },
    },
    userMetadata: {
      agent_os_https_browserbase_test: "true",
    },
  },
  agent,
);
console.log("HTTPS_BROWSERBASE_STEP:after-create");
if (createResponse.statusCode < 200 || createResponse.statusCode >= 300) {
  throw new Error(
    "create failed with " + createResponse.statusCode + ": " + createResponse.body,
  );
}

const created = JSON.parse(createResponse.body);
if (!created?.id) {
  throw new Error("missing created session id");
}

console.log("HTTPS_BROWSERBASE_STEP:before-debug");
const debugResponse = await requestJson(
  "GET",
  "/v1/sessions/" + created.id + "/debug",
  null,
  agent,
);
console.log("HTTPS_BROWSERBASE_STEP:after-debug");

console.log("HTTPS_BROWSERBASE_STEP:before-release");
const releaseResponse = await requestJson(
  "POST",
  "/v1/sessions/" + created.id,
  { status: "REQUEST_RELEASE" },
  agent,
);
console.log("HTTPS_BROWSERBASE_STEP:after-release");

console.log(
  "HTTPS_BROWSERBASE_RESULT:" +
    JSON.stringify({
      createStatus: createResponse.statusCode,
      debugStatus: debugResponse.statusCode,
      releaseStatus: releaseResponse.statusCode,
      sessionId: created.id,
    }),
);
`;

const CDP_TARGET_SCRIPT = String.raw`
import WebSocket from "ws";

function ensure(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

const apiKey = process.env.BROWSERBASE_API_KEY;
const projectId = process.env.BROWSERBASE_PROJECT_ID;

const createResponse = await fetch("https://api.browserbase.com/v1/sessions", {
  method: "POST",
  headers: {
    "x-bb-api-key": apiKey,
    "content-type": "application/json",
  },
  body: JSON.stringify({
    projectId,
    browserSettings: {
      viewport: { width: 1288, height: 711 },
    },
    userMetadata: {
      agent_os_browserbase_target_test: "true",
    },
  }),
  signal: AbortSignal.timeout(30_000),
});
if (!createResponse.ok) {
  throw new Error(
    "Browserbase session create failed with " +
      createResponse.status +
      ": " +
      (await createResponse.text()),
  );
}
const created = await createResponse.json();
ensure(created.id, "missing session id");
ensure(created.connectUrl, "missing connectUrl");

const frameTreeSummary = await new Promise((resolve, reject) => {
  const socket = new WebSocket(created.connectUrl, {
    headers: {
      "User-Agent": "agent-os-browserbase-target-test",
    },
  });
  let nextId = 1;
  const inflight = new Map();
  const timer = setTimeout(() => {
    reject(new Error("timed out waiting for Browserbase target attach flow"));
  }, 20_000);

  function send(method, params = {}, sessionId) {
    const id = nextId++;
    return new Promise((resolveSend, rejectSend) => {
      inflight.set(id, { resolveSend, rejectSend });
      socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }

  socket.on("message", async (data) => {
    try {
      const message = JSON.parse(data.toString());
      if ("id" in message) {
        const entry = inflight.get(message.id);
        if (!entry) {
          return;
        }
        inflight.delete(message.id);
        if (message.error) {
          entry.rejectSend(
            new Error(message.error.code + " " + message.error.message),
          );
          return;
        }
        entry.resolveSend(message.result);
      }
    } catch (error) {
      clearTimeout(timer);
      reject(error);
    }
  });

  socket.once("open", async () => {
    try {
      await send("Target.setAutoAttach", {
        autoAttach: true,
        waitForDebuggerOnStart: true,
        flatten: true,
      });
      const targetList = await send("Target.getTargets");
      let pageTarget = targetList.targetInfos.find((target) => target.type === "page");
      if (!pageTarget) {
        const createdTarget = await send("Target.createTarget", { url: "about:blank" });
        pageTarget = { targetId: createdTarget.targetId, type: "page" };
      }
      const attached = await send("Target.attachToTarget", {
        targetId: pageTarget.targetId,
        flatten: true,
      });
      const sessionId = attached.sessionId;
      ensure(sessionId, "missing attached sessionId");
      await send("Page.enable", {}, sessionId);
      await send("Runtime.runIfWaitingForDebugger", {}, sessionId);
      const frameTree = await send("Page.getFrameTree", {}, sessionId);
      clearTimeout(timer);
      try {
        socket.close();
      } catch {}
      resolve({
        sessionId,
        frameId: frameTree.frameTree?.frame?.id,
        url: frameTree.frameTree?.frame?.url,
      });
    } catch (error) {
      clearTimeout(timer);
      reject(error);
    }
  });

  socket.once("error", (error) => {
    clearTimeout(timer);
    reject(error);
  });
});

const releaseResponse = await fetch(
  "https://api.browserbase.com/v1/sessions/" + created.id,
  {
    method: "POST",
    headers: {
      "x-bb-api-key": apiKey,
      "content-type": "application/json",
    },
    body: JSON.stringify({ status: "REQUEST_RELEASE" }),
    signal: AbortSignal.timeout(30_000),
  },
);
if (!releaseResponse.ok) {
  throw new Error(
    "Browserbase session release failed with " +
      releaseResponse.status +
      ": " +
      (await releaseResponse.text()),
  );
}

console.log("BROWSERBASE_FRAME_TREE:" + JSON.stringify(frameTreeSummary));
`;

const CDP_BOOTSTRAP_SCRIPT = String.raw`
import WebSocket from "ws";

function ensure(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

const apiKey = process.env.BROWSERBASE_API_KEY;
const projectId = process.env.BROWSERBASE_PROJECT_ID;

const createResponse = await fetch("https://api.browserbase.com/v1/sessions", {
  method: "POST",
  headers: {
    "x-bb-api-key": apiKey,
    "content-type": "application/json",
  },
  body: JSON.stringify({
    projectId,
    browserSettings: {
      viewport: { width: 1288, height: 711 },
    },
    userMetadata: {
      agent_os_browserbase_bootstrap_test: "true",
    },
  }),
  signal: AbortSignal.timeout(30_000),
});
if (!createResponse.ok) {
  throw new Error(
    "Browserbase session create failed with " +
      createResponse.status +
      ": " +
      (await createResponse.text()),
  );
}
const created = await createResponse.json();
ensure(created.id, "missing session id");
ensure(created.connectUrl, "missing connectUrl");

const bootstrapSummary = await new Promise((resolve, reject) => {
  const socket = new WebSocket(created.connectUrl, {
    headers: {
      "User-Agent": "agent-os-browserbase-bootstrap-test",
    },
  });
  let nextId = 1;
  const inflight = new Map();
  const steps = [];
  const timer = setTimeout(() => {
    reject(new Error("timed out waiting for Browserbase bootstrap flow: " + JSON.stringify(steps)));
  }, 20_000);

  function note(step) {
    steps.push(step);
    console.log("BOOTSTRAP_STEP:" + step);
  }

  function send(method, params = {}, sessionId) {
    const id = nextId++;
    return new Promise((resolveSend, rejectSend) => {
      inflight.set(id, { resolveSend, rejectSend, method });
      socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }

  socket.on("message", (data) => {
    try {
      const message = JSON.parse(data.toString());
      if ("id" in message) {
        const entry = inflight.get(message.id);
        if (!entry) {
          return;
        }
        inflight.delete(message.id);
        if (message.error) {
          entry.rejectSend(
            new Error(entry.method + ": " + message.error.code + " " + message.error.message),
          );
          return;
        }
        entry.resolveSend(message.result);
      }
    } catch (error) {
      clearTimeout(timer);
      reject(error);
    }
  });

  socket.once("open", async () => {
    try {
      note("root-setAutoAttach");
      await send("Target.setAutoAttach", {
        autoAttach: true,
        waitForDebuggerOnStart: true,
        flatten: true,
      });
      note("root-setDiscoverTargets");
      await send("Target.setDiscoverTargets", { discover: true });
      note("root-getTargets");
      const targetList = await send("Target.getTargets");
      let pageTarget = targetList.targetInfos.find((target) => target.type === "page");
      if (!pageTarget) {
        note("root-createTarget");
        const createdTarget = await send("Target.createTarget", { url: "about:blank" });
        pageTarget = { targetId: createdTarget.targetId, type: "page" };
      }
      note("root-attachToTarget");
      const attached = await send("Target.attachToTarget", {
        targetId: pageTarget.targetId,
        flatten: true,
      });
      const sessionId = attached.sessionId;
      ensure(sessionId, "missing attached sessionId");
      note("session-Page.enable");
      await send("Page.enable", {}, sessionId);
      note("session-Runtime.enable");
      await send("Runtime.enable", {}, sessionId);
      note("session-Target.setAutoAttach");
      await send("Target.setAutoAttach", {
        autoAttach: true,
        waitForDebuggerOnStart: true,
        flatten: true,
      }, sessionId);
      note("session-Page.addScriptToEvaluateOnNewDocument");
      await send("Page.addScriptToEvaluateOnNewDocument", {
        source: "window.__agentOsBootstrapTest = true;",
        runImmediately: true,
      }, sessionId);
      note("session-Runtime.runIfWaitingForDebugger");
      await send("Runtime.runIfWaitingForDebugger", {}, sessionId);
      note("session-Page.getFrameTree");
      const frameTree = await send("Page.getFrameTree", {}, sessionId);
      note("root-Browser.setDownloadBehavior");
      await send("Browser.setDownloadBehavior", {
        behavior: "allow",
        downloadPath: "downloads",
        eventsEnabled: true,
      });
      note("api-debug");
      const debugResponse = await fetch(
        "https://api.browserbase.com/v1/sessions/" + created.id + "/debug",
        {
          headers: {
            "x-bb-api-key": apiKey,
          },
          signal: AbortSignal.timeout(15_000),
        },
      );
      const debugText = await debugResponse.text();
      clearTimeout(timer);
      try {
        socket.close();
      } catch {}
      resolve({
        steps,
        sessionId,
        frameId: frameTree.frameTree?.frame?.id,
        url: frameTree.frameTree?.frame?.url,
        debugStatus: debugResponse.status,
        debugText,
      });
    } catch (error) {
      clearTimeout(timer);
      reject(error);
    }
  });

  socket.once("error", (error) => {
    clearTimeout(timer);
    reject(error);
  });
});

const releaseResponse = await fetch(
  "https://api.browserbase.com/v1/sessions/" + created.id,
  {
    method: "POST",
    headers: {
      "x-bb-api-key": apiKey,
      "content-type": "application/json",
    },
    body: JSON.stringify({ status: "REQUEST_RELEASE" }),
    signal: AbortSignal.timeout(30_000),
  },
);
if (!releaseResponse.ok) {
  throw new Error(
    "Browserbase session release failed with " +
      releaseResponse.status +
      ": " +
      (await releaseResponse.text()),
  );
}

console.log("BROWSERBASE_BOOTSTRAP:" + JSON.stringify(bootstrapSummary));
`;

describe("Browserbase websocket smoke test", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		if (vm) {
			await vm.dispose();
			vm = null;
		}
	});

	const browserbaseTest = (...args: Parameters<typeof test>) =>
		testIf(HAS_BROWSERBASE_CREDENTIALS, ...args);

	browserbaseTest(
		"opens a Browserbase CDP websocket and completes one command",
		async () => {
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile("/tmp/browserbase-ws-test.mjs", GUEST_SCRIPT);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn("node", ["/tmp/browserbase-ws-test.mjs"], {
				env: {
					BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
					BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
				},
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);

			const replyLine = stdout
				.split("\n")
				.find((line) => line.startsWith("BROWSERBASE_CDP_REPLY:"));
			expect(replyLine, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBeTruthy();

			const reply = JSON.parse(
				replyLine!.slice("BROWSERBASE_CDP_REPLY:".length),
			) as {
				id?: number;
				result?: { product?: string };
			};
			expect(reply.id).toBe(1);
			expect(typeof reply.result?.product).toBe("string");
		},
		45_000,
	);

	browserbaseTest(
		"initializes the browse cli daemon and lists pages in remote mode",
		async () => {
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile("/tmp/browserbase-cli-pages-test.mjs", CLI_PAGES_SCRIPT);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn("node", ["/tmp/browserbase-cli-pages-test.mjs"], {
				env: {
					BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
					BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
					BROWSE_SESSION: `browserbase-pages-${Date.now()}`,
					BROWSERBASE_CONFIG_DIR: "/tmp/browserbase-cli-debug",
					BROWSERBASE_FLOW_LOGS: "1",
					BROWSERBASE_CDP_CONNECT_MAX_MS: "5000",
					BROWSERBASE_SESSION_CREATE_MAX_MS: "10000",
					STAGEHAND_FIRST_TOP_LEVEL_PAGE_TIMEOUT_MS: "2000",
				},
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);
			expect(stdout).toContain("BROWSERBASE_PAGES:");
		},
		90_000,
	);

	browserbaseTest(
		"attaches to a Browserbase page target and reads its frame tree over raw CDP",
		async () => {
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile("/tmp/browserbase-target-test.mjs", CDP_TARGET_SCRIPT);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn("node", ["/tmp/browserbase-target-test.mjs"], {
				env: {
					BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
					BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
				},
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);
			expect(stdout).toContain("BROWSERBASE_FRAME_TREE:");
		},
		60_000,
	);

	browserbaseTest(
		"initializes Stagehand directly in Browserbase mode",
		async () => {
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile(
				"/tmp/browserbase-direct-stagehand-test.mjs",
				DIRECT_STAGEHAND_INIT_SCRIPT,
			);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn(
				"node",
				["/tmp/browserbase-direct-stagehand-test.mjs"],
				{
					env: {
						BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
						BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
					},
					onStdout: (data: Uint8Array) => {
						stdout += new TextDecoder().decode(data);
					},
					onStderr: (data: Uint8Array) => {
						stderr += new TextDecoder().decode(data);
					},
				},
			);

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);
			expect(stdout).toContain("BROWSERBASE_DIRECT_STAGEHAND:");
		},
		90_000,
	);

	browserbaseTest(
		"creates and debugs a Browserbase session through the Browserbase SDK",
		async () => {
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile("/tmp/browserbase-sdk-test.mjs", BROWSERBASE_SDK_SCRIPT);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn("node", ["/tmp/browserbase-sdk-test.mjs"], {
				env: {
					BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
					BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
				},
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);
			expect(stdout).toContain("BROWSERBASE_SDK_RESULT:");
		},
		90_000,
	);

	browserbaseTest(
		"creates and debugs a Browserbase session through node:https",
		async () => {
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile(
				"/tmp/browserbase-https-test.mjs",
				HTTPS_BROWSERBASE_SCRIPT,
			);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn("node", ["/tmp/browserbase-https-test.mjs"], {
				env: {
					BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
					BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
				},
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);
			expect(stdout).toContain("HTTPS_BROWSERBASE_RESULT:");
		},
		90_000,
	);

	browserbaseTest(
		"completes the top-level target pre-resume bootstrap sequence over raw CDP",
		async () => {
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile("/tmp/browserbase-bootstrap-test.mjs", CDP_BOOTSTRAP_SCRIPT);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn("node", ["/tmp/browserbase-bootstrap-test.mjs"], {
				env: {
					BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
					BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
				},
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);
			expect(stdout).toContain("BROWSERBASE_BOOTSTRAP:");
		},
		60_000,
	);
});
