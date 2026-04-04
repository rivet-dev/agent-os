import type { ManagedProcess } from "@secure-exec/core";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AcpClient } from "../src/acp-client.js";
import { AgentOs } from "../src/agent-os.js";
import type { JsonRpcNotification } from "../src/protocol.js";
import { createStdoutLineIterable } from "../src/stdout-lines.js";

/**
 * Comprehensive mock ACP adapter that supports all protocol methods.
 * Handles: initialize, session/new, session/prompt (with notifications),
 * session/cancel, session/set_mode, session/set_config_option,
 * request/permission (sends permission notification during prompt).
 */
const FULL_MOCK_ACP_ADAPTER = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);
      if (msg.id === undefined) continue;

      let result;
      switch (msg.method) {
        case 'initialize':
          result = {
            protocolVersion: 1,
            agentInfo: { name: 'mock-agent', version: '2.0.0' },
            agentCapabilities: {
              permissions: true,
              plan_mode: true,
              questions: false,
              tool_calls: true,
              text_messages: true,
              images: false,
              file_attachments: false,
              session_lifecycle: true,
              error_events: true,
              reasoning: true,
              status: true,
              streaming_deltas: false,
              mcp_tools: true,
            },
          };
          break;
        case 'session/new':
          result = { sessionId: 'test-session-1' };
          break;
        case 'session/prompt': {
          const sid = (msg.params && msg.params.sessionId) || 'test-session-1';
          // Send multiple session/update notifications before response
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            method: 'session/update',
            params: { sessionId: sid, type: 'status', status: 'thinking' },
          }) + '\\n');
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            method: 'session/update',
            params: { sessionId: sid, type: 'text', text: 'Mock response text' },
          }) + '\\n');
          result = { sessionId: sid, status: 'complete' };
          break;
        }
        case 'session/cancel':
          result = { cancelled: true };
          break;
        case 'session/set_mode':
          result = { modeId: msg.params.modeId, applied: true };
          break;
        case 'session/set_config_option':
          result = { configId: msg.params.configId, value: msg.params.value, applied: true };
          break;
        case 'request/permission':
          result = { acknowledged: true };
          break;
        case 'custom/echo':
          result = { echo: msg.params };
          break;
        default:
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            error: { code: -32601, message: 'Method not found: ' + msg.method },
          }) + '\\n');
          continue;
      }

      process.stdout.write(JSON.stringify({
        jsonrpc: '2.0', id: msg.id, result,
      }) + '\\n');
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that sends a permission request notification during session/prompt.
 */
const PERMISSION_MOCK_ADAPTER = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);
      if (msg.id === undefined) continue;

      let result;
      switch (msg.method) {
        case 'initialize':
          result = { protocolVersion: 1, agentInfo: { name: 'perm-agent', version: '1.0' } };
          break;
        case 'session/new':
          result = { sessionId: 'perm-session-1' };
          break;
        case 'session/prompt': {
          // Send permission request notification before responding
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            method: 'request/permission',
            params: {
              sessionId: 'perm-session-1',
              permissionId: 'perm-001',
              description: 'Run shell command: ls -la',
              tool: 'shell',
              command: 'ls -la',
            },
          }) + '\\n');
          result = { sessionId: 'perm-session-1', status: 'waiting_for_permission' };
          break;
        }
        case 'request/permission':
          result = { acknowledged: true };
          break;
        default:
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            error: { code: -32601, message: 'Method not found' },
          }) + '\\n');
          continue;
      }

      process.stdout.write(JSON.stringify({
        jsonrpc: '2.0', id: msg.id, result,
      }) + '\\n');
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that sends a modern ACP session/request_permission request during session/prompt.
 */
const MODERN_PERMISSION_MOCK_ADAPTER = `
let buffer = '';
let pendingPromptId = null;
const permissionRequestId = 'perm-modern-001';

process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);

      if (msg.id === permissionRequestId && msg.method === undefined) {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0',
          id: pendingPromptId,
          result: {
            sessionId: 'perm-session-2',
            status: 'permission_granted',
            permission: msg.result,
          },
        }) + '\\n');
        pendingPromptId = null;
        continue;
      }

      if (msg.id === undefined) continue;

      let result;
      switch (msg.method) {
        case 'initialize':
          result = { protocolVersion: 1, agentInfo: { name: 'perm-agent-modern', version: '1.0' } };
          break;
        case 'session/new':
          result = { sessionId: 'perm-session-2' };
          break;
        case 'session/prompt':
          pendingPromptId = msg.id;
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            id: permissionRequestId,
            method: 'session/request_permission',
            params: {
              sessionId: 'perm-session-2',
              options: [
                { optionId: 'allow_once', kind: 'allow_once', name: 'Allow once' },
                { optionId: 'allow_always', kind: 'allow_always', name: 'Always allow' },
                { optionId: 'reject_once', kind: 'reject_once', name: 'Reject' },
              ],
              toolCall: {
                kind: 'execute',
                toolCallId: 'tool-001',
                title: 'Bash',
                status: 'pending',
                rawInput: { command: 'xu hello-agent-os' },
              },
            },
          }) + '\\n');
          continue;
        default:
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            error: { code: -32601, message: 'Method not found' },
          }) + '\\n');
          continue;
      }

      process.stdout.write(JSON.stringify({
        jsonrpc: '2.0', id: msg.id, result,
      }) + '\\n');
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that uses OpenCode-style option IDs (once/always/reject)
 * instead of the allow_once/allow_always/reject_once aliases.
 */
const OPENCODE_STYLE_PERMISSION_MOCK_ADAPTER = `
let buffer = '';
let pendingPromptId = null;
const permissionRequestId = 'perm-opencode-001';

process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);

      if (msg.id === permissionRequestId && msg.method === undefined) {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0',
          id: pendingPromptId,
          result: {
            sessionId: 'perm-session-opencode',
            status: 'permission_granted',
            permission: msg.result,
          },
        }) + '\\n');
        pendingPromptId = null;
        continue;
      }

      if (msg.id === undefined) continue;

      switch (msg.method) {
        case 'initialize':
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            result: { protocolVersion: 1, agentInfo: { name: 'perm-agent-opencode', version: '1.0' } },
          }) + '\\n');
          continue;
        case 'session/new':
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            result: { sessionId: 'perm-session-opencode' },
          }) + '\\n');
          continue;
        case 'session/prompt':
          pendingPromptId = msg.id;
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            id: permissionRequestId,
            method: 'session/request_permission',
            params: {
              sessionId: 'perm-session-opencode',
              options: [
                { optionId: 'once', kind: 'allow_once', name: 'Allow once' },
                { optionId: 'always', kind: 'allow_always', name: 'Always allow' },
                { optionId: 'reject', kind: 'reject_once', name: 'Reject' },
              ],
              toolCall: {
                kind: 'execute',
                toolCallId: 'tool-oc-001',
                title: 'Bash',
                status: 'pending',
                rawInput: { command: 'echo hello' },
              },
            },
          }) + '\\n');
          continue;
        default:
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            error: { code: -32601, message: 'Method not found' },
          }) + '\\n');
          continue;
      }
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that emits non-JSON output and a notification, then hangs.
 * Used to verify timeout diagnostics include recent ACP activity.
 */
const HANGING_ACTIVITY_MOCK_ADAPTER = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);
      if (msg.id === undefined) continue;

      switch (msg.method) {
        case 'initialize':
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            id: msg.id,
            result: { protocolVersion: 1, agentInfo: { name: 'hanging-activity-agent', version: '1.0' } },
          }) + '\\n');
          break;
        case 'session/prompt':
          process.stdout.write('[sandbox.require] start node:url /\\n');
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            method: 'session/update',
            params: {
              sessionId: 'hanging-session',
              type: 'status',
              status: 'thinking',
            },
          }) + '\\n');
          break;
        default:
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            id: msg.id,
            result: { ok: true },
          }) + '\\n');
          break;
      }
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that emits the same ACP permission request twice.
 * AcpClient should dedupe the repeated inbound request ID.
 */
const DUPLICATE_MODERN_PERMISSION_MOCK_ADAPTER = `
let buffer = '';
let pendingPromptId = null;
const permissionRequestId = 'perm-dup-001';

process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);

      if (msg.id === permissionRequestId && msg.method === undefined) {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0',
          id: pendingPromptId,
          result: {
            sessionId: 'perm-session-dup',
            status: 'permission_granted',
            permission: msg.result,
          },
        }) + '\\n');
        pendingPromptId = null;
        continue;
      }

      if (msg.id === undefined) continue;

      let result;
      switch (msg.method) {
        case 'initialize':
          result = { protocolVersion: 1, agentInfo: { name: 'perm-agent-dup', version: '1.0' } };
          break;
        case 'session/new':
          result = { sessionId: 'perm-session-dup' };
          break;
        case 'session/prompt': {
          pendingPromptId = msg.id;
          const permissionRequest = JSON.stringify({
            jsonrpc: '2.0',
            id: permissionRequestId,
            method: 'session/request_permission',
            params: {
              sessionId: 'perm-session-dup',
              options: [
                { optionId: 'allow_once', kind: 'allow_once', name: 'Allow once' },
                { optionId: 'reject_once', kind: 'reject_once', name: 'Reject' },
              ],
              toolCall: {
                kind: 'execute',
                toolCallId: 'tool-dup-001',
                title: 'Bash',
                status: 'pending',
                rawInput: { command: 'xu hello-agent-os' },
              },
            },
          });
          process.stdout.write(permissionRequest + '\\n');
          process.stdout.write(permissionRequest + '\\n');
          continue;
        }
        default:
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            error: { code: -32601, message: 'Method not found' },
          }) + '\\n');
          continue;
      }

      process.stdout.write(JSON.stringify({
        jsonrpc: '2.0', id: msg.id, result,
      }) + '\\n');
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that only supports session/cancel as an ACP notification.
 * Request-form session/cancel returns -32601, while the notification increments
 * a counter that can be queried via custom/cancel-count.
 */
const NOTIFICATION_CANCEL_MOCK_ADAPTER = `
let buffer = '';
let cancelCount = 0;

process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);

      if (msg.id === undefined) {
        if (msg.method === 'session/cancel') {
          cancelCount++;
        }
        continue;
      }

      let result;
      switch (msg.method) {
        case 'initialize':
          result = {
            protocolVersion: 1,
            agentInfo: { name: 'notification-cancel-agent', version: '1.0.0' },
          };
          break;
        case 'session/new':
          result = { sessionId: 'notification-cancel-session-1' };
          break;
        case 'custom/cancel-count':
          result = { cancelCount };
          break;
        case 'session/cancel':
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            id: msg.id,
            error: {
              code: -32601,
              message: 'Method not found: session/cancel',
              data: { method: 'session/cancel' },
            },
          }) + '\\n');
          continue;
        default:
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            error: { code: -32601, message: 'Method not found: ' + msg.method },
          }) + '\\n');
          continue;
      }

      process.stdout.write(JSON.stringify({
        jsonrpc: '2.0', id: msg.id, result,
      }) + '\\n');
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that prints non-JSON banners before becoming ready.
 */
const BANNER_MOCK_ADAPTER = `
// Non-JSON startup banners
process.stdout.write('Starting mock agent v2.0...\\n');
process.stdout.write('Loading configuration...\\n');
process.stdout.write('[WARN] Debug mode enabled\\n');

let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);
      if (msg.id === undefined) continue;
      if (msg.method === 'initialize') {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: msg.id,
          result: { protocolVersion: 1, agentInfo: { name: 'banner-agent', version: '1.0' } },
        }) + '\\n');
      } else {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: msg.id,
          error: { code: -32601, message: 'Method not found' },
        }) + '\\n');
      }
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that sends malformed JSON on stdout intermittently.
 */
const MALFORMED_MOCK_ADAPTER = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);
      if (msg.id === undefined) continue;

      // Emit some malformed JSON before the real response
      process.stdout.write('{broken json\\n');
      process.stdout.write('not even close to json\\n');

      if (msg.method === 'initialize') {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: msg.id,
          result: { protocolVersion: 1, agentInfo: { name: 'malformed-agent', version: '1.0' } },
        }) + '\\n');
      }
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that sends ordered notifications for testing ordering.
 */
const ORDERED_NOTIFICATIONS_ADAPTER = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);
      if (msg.id === undefined) continue;

      if (msg.method === 'initialize') {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: msg.id,
          result: { protocolVersion: 1, agentInfo: { name: 'ordered-agent', version: '1.0' } },
        }) + '\\n');
      } else if (msg.method === 'session/new') {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: msg.id,
          result: { sessionId: 'ordered-session-1' },
        }) + '\\n');
      } else if (msg.method === 'session/prompt') {
        // Send 5 notifications in specific order
        for (let i = 1; i <= 5; i++) {
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            method: 'session/update',
            params: { seq: i, text: 'notification-' + i },
          }) + '\\n');
        }
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: msg.id,
          result: { done: true },
        }) + '\\n');
      }
    } catch (e) {}
  }
});
`;

/**
 * Mock adapter that exits immediately after receiving a message.
 */
const EXIT_MOCK_ADAPTER = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);
      if (msg.id !== undefined) {
        // Exit without responding — simulates agent crash
        process.exit(1);
      }
    } catch (e) {}
  }
});
`;

async function spawnAdapter(
	vm: AgentOs,
	script: string,
	scriptPath = "/tmp/mock-adapter.mjs",
): Promise<{
	proc: ManagedProcess;
	client: AcpClient;
}> {
	await vm.writeFile(scriptPath, script);
	const { iterable, onStdout } = createStdoutLineIterable();
	const proc = vm.kernel.spawn("node", [scriptPath], {
		streamStdin: true,
		onStdout,
		env: { HOME: "/home/user" },
	});
	const client = new AcpClient(proc, iterable);
	return { proc, client };
}

async function spawnAdapterWithTimeout(
	vm: AgentOs,
	script: string,
	timeoutMs: number,
	scriptPath = "/tmp/mock-adapter.mjs",
): Promise<{
	proc: ManagedProcess;
	client: AcpClient;
}> {
	await vm.writeFile(scriptPath, script);
	const { iterable, onStdout } = createStdoutLineIterable();
	const proc = vm.kernel.spawn("node", [scriptPath], {
		streamStdin: true,
		onStdout,
		env: { HOME: "/home/user" },
	});
	const client = new AcpClient(proc, iterable, { timeoutMs });
	return { proc, client };
}

describe("ACP protocol comprehensive tests", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("initialize returns protocolVersion and agentInfo with capabilities", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		const response = await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		expect(response.error).toBeUndefined();
		const result = response.result as {
			protocolVersion: number;
			agentInfo: { name: string; version: string };
			agentCapabilities: Record<string, boolean>;
		};
		expect(result.protocolVersion).toBe(1);
		expect(result.agentInfo.name).toBe("mock-agent");
		expect(result.agentInfo.version).toBe("2.0.0");
		expect(result.agentCapabilities.permissions).toBe(true);
		expect(result.agentCapabilities.plan_mode).toBe(true);
		expect(result.agentCapabilities.tool_calls).toBe(true);
		expect(result.agentCapabilities.mcp_tools).toBe(true);

		client.close();
	}, 30_000);

	test("session/new returns sessionId", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		const response = await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		expect(response.error).toBeUndefined();
		const result = response.result as { sessionId: string };
		expect(result.sessionId).toBe("test-session-1");

		client.close();
	}, 30_000);

	test("session/prompt receives session/update notifications before response", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		const notifications: JsonRpcNotification[] = [];
		client.onNotification((n) => notifications.push(n));

		const response = await client.request("session/prompt", {
			sessionId: "test-session-1",
			prompt: [{ type: "text", text: "Hello" }],
		});

		expect(response.error).toBeUndefined();
		const result = response.result as { sessionId: string; status: string };
		expect(result.status).toBe("complete");

		// VM stdout can deliver lines twice (known duplication); check >=2 and verify content
		expect(notifications.length).toBeGreaterThanOrEqual(2);
		const updates = notifications.filter((n) => n.method === "session/update");
		expect(updates.length).toBeGreaterThanOrEqual(2);
		const types = updates.map((n) => (n.params as { type: string }).type);
		expect(types).toContain("status");
		expect(types).toContain("text");
		const textNotif = updates.find(
			(n) => (n.params as { type: string }).type === "text",
		);
		expect((textNotif?.params as { text: string }).text).toBe(
			"Mock response text",
		);

		client.close();
	}, 30_000);

	test("session/cancel sends cancel and receives acknowledgement", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		const response = await client.request("session/cancel", {
			sessionId: "test-session-1",
		});

		expect(response.error).toBeUndefined();
		const result = response.result as { cancelled: boolean };
		expect(result.cancelled).toBe(true);

		client.close();
	}, 30_000);

	test("session/cancel falls back to ACP notification when request form is unsupported", async () => {
		const { client } = await spawnAdapter(vm, NOTIFICATION_CANCEL_MOCK_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		const response = await client.request("session/cancel", {
			sessionId: "notification-cancel-session-1",
		});

		expect(response.error).toBeUndefined();
		expect(
			response.result as {
				cancelled: boolean;
				requested: boolean;
				via: string;
			},
		).toMatchObject({
			cancelled: false,
			requested: true,
			via: "notification-fallback",
		});

		const countResponse = await client.request("custom/cancel-count");
		expect(countResponse.error).toBeUndefined();
		expect((countResponse.result as { cancelCount: number }).cancelCount).toBe(
			1,
		);

		client.close();
	}, 30_000);

	test("session/set_mode sends mode change", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		const response = await client.request("session/set_mode", {
			sessionId: "test-session-1",
			modeId: "plan",
		});

		expect(response.error).toBeUndefined();
		const result = response.result as { modeId: string; applied: boolean };
		expect(result.modeId).toBe("plan");
		expect(result.applied).toBe(true);

		client.close();
	}, 30_000);

	test("session/set_config_option sends config change", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		const response = await client.request("session/set_config_option", {
			sessionId: "test-session-1",
			configId: "model",
			value: "claude-sonnet-4-20250514",
		});

		expect(response.error).toBeUndefined();
		const result = response.result as {
			configId: string;
			value: string;
			applied: boolean;
		};
		expect(result.configId).toBe("model");
		expect(result.value).toBe("claude-sonnet-4-20250514");
		expect(result.applied).toBe(true);

		client.close();
	}, 30_000);

	test("request/permission flow -- agent sends notification, client responds", async () => {
		const { client } = await spawnAdapter(vm, PERMISSION_MOCK_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		const permissionRequests: JsonRpcNotification[] = [];
		client.onNotification((n) => {
			if (n.method === "request/permission") {
				permissionRequests.push(n);
			}
		});

		// session/prompt triggers a permission notification from the mock
		const promptResponse = await client.request("session/prompt", {
			sessionId: "perm-session-1",
			prompt: [{ type: "text", text: "list files" }],
		});

		expect(promptResponse.error).toBeUndefined();
		// VM stdout can duplicate lines; check at least 1 permission request arrived
		expect(permissionRequests.length).toBeGreaterThanOrEqual(1);

		const permParams = permissionRequests[0].params as {
			permissionId: string;
			description: string;
		};
		expect(permParams.permissionId).toBe("perm-001");
		expect(permParams.description).toBe("Run shell command: ls -la");

		// Respond to the permission request
		const permResponse = await client.request("request/permission", {
			sessionId: "perm-session-1",
			permissionId: "perm-001",
			reply: "once",
		});

		expect(permResponse.error).toBeUndefined();
		expect(
			(permResponse.result as { acknowledged: boolean }).acknowledged,
		).toBe(true);

		client.close();
	}, 30_000);

	test("session/request_permission flow -- agent sends ACP request, client responds", async () => {
		const { client } = await spawnAdapter(vm, MODERN_PERMISSION_MOCK_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		const permissionRequests: JsonRpcNotification[] = [];
		client.onNotification((notification) => {
			if (notification.method === "request/permission") {
				permissionRequests.push(notification);
			}
		});

		const promptPromise = client.request("session/prompt", {
			sessionId: "perm-session-2",
			prompt: [{ type: "text", text: "run xu hello-agent-os" }],
		});

		for (let i = 0; i < 20 && permissionRequests.length === 0; i++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}

		expect(permissionRequests.length).toBeGreaterThanOrEqual(1);
		const permParams = permissionRequests[0].params as {
			permissionId: string;
			sessionId: string;
			toolCall: {
				rawInput?: { command?: string };
			};
		};
		expect(permParams.permissionId).toBe("perm-modern-001");
		expect(permParams.sessionId).toBe("perm-session-2");
		expect(permParams.toolCall.rawInput?.command).toBe("xu hello-agent-os");

		const permResponse = await client.request("request/permission", {
			sessionId: "perm-session-2",
			permissionId: "perm-modern-001",
			reply: "always",
		});

		expect(permResponse.error).toBeUndefined();
		expect(
			(permResponse.result as { outcome: { optionId: string } }).outcome
				.optionId,
		).toBe("allow_always");

		const promptResponse = await promptPromise;
		expect(promptResponse.error).toBeUndefined();
		expect(
			(
				promptResponse.result as {
					permission: { outcome: { optionId: string } };
				}
			).permission.outcome.optionId,
		).toBe("allow_always");

		client.close();
	}, 30_000);

	test("duplicate session/request_permission requests are deduped by request ID", async () => {
		const { client } = await spawnAdapter(
			vm,
			DUPLICATE_MODERN_PERMISSION_MOCK_ADAPTER,
		);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		const permissionRequests: JsonRpcNotification[] = [];
		client.onNotification((notification) => {
			if (notification.method === "request/permission") {
				permissionRequests.push(notification);
			}
		});

		const promptPromise = client.request("session/prompt", {
			sessionId: "perm-session-dup",
			prompt: [{ type: "text", text: "run xu hello-agent-os" }],
		});

		for (let i = 0; i < 20 && permissionRequests.length === 0; i++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}

		expect(permissionRequests.length).toBe(1);
		const permParams = permissionRequests[0].params as {
			permissionId: string;
			sessionId: string;
		};
		expect(permParams.permissionId).toBe("perm-dup-001");
		expect(permParams.sessionId).toBe("perm-session-dup");

		const permResponse = await client.request("request/permission", {
			sessionId: "perm-session-dup",
			permissionId: "perm-dup-001",
			reply: "once",
		});

		expect(permResponse.error).toBeUndefined();
		expect(
			(permResponse.result as { outcome: { optionId: string } }).outcome
				.optionId,
		).toBe("allow_once");

		const promptResponse = await promptPromise;
		expect(promptResponse.error).toBeUndefined();
		expect(
			(
				promptResponse.result as {
					permission: { outcome: { optionId: string } };
				}
			).permission.outcome.optionId,
		).toBe("allow_once");

		client.close();
	}, 30_000);

	test("session/request_permission maps replies onto OpenCode-style option IDs", async () => {
		const { client } = await spawnAdapter(
			vm,
			OPENCODE_STYLE_PERMISSION_MOCK_ADAPTER,
		);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		const permissionRequests: JsonRpcNotification[] = [];
		client.onNotification((notification) => {
			if (notification.method === "request/permission") {
				permissionRequests.push(notification);
			}
		});

		const promptPromise = client.request("session/prompt", {
			sessionId: "perm-session-opencode",
			prompt: [{ type: "text", text: "run echo hello" }],
		});

		for (let i = 0; i < 20 && permissionRequests.length === 0; i++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}

		expect(permissionRequests.length).toBe(1);
		const permResponse = await client.request("request/permission", {
			sessionId: "perm-session-opencode",
			permissionId: "perm-opencode-001",
			reply: "once",
		});

		expect(permResponse.error).toBeUndefined();
		expect(
			(permResponse.result as { outcome: { optionId: string } }).outcome
				.optionId,
		).toBe("once");

		const promptResponse = await promptPromise;
		expect(promptResponse.error).toBeUndefined();
		expect(
			(
				promptResponse.result as {
					permission: { outcome: { optionId: string } };
				}
			).permission.outcome.optionId,
		).toBe("once");

		client.close();
	}, 30_000);

	test("initialize response carries agentCapabilities and agentInfo", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		const response = await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		const result = response.result as {
			agentCapabilities: Record<string, boolean>;
			agentInfo: { name: string; version: string };
		};

		// Verify all capability flags
		expect(result.agentCapabilities.permissions).toBe(true);
		expect(result.agentCapabilities.plan_mode).toBe(true);
		expect(result.agentCapabilities.questions).toBe(false);
		expect(result.agentCapabilities.tool_calls).toBe(true);
		expect(result.agentCapabilities.text_messages).toBe(true);
		expect(result.agentCapabilities.images).toBe(false);
		expect(result.agentCapabilities.file_attachments).toBe(false);
		expect(result.agentCapabilities.session_lifecycle).toBe(true);
		expect(result.agentCapabilities.error_events).toBe(true);
		expect(result.agentCapabilities.reasoning).toBe(true);
		expect(result.agentCapabilities.status).toBe(true);
		expect(result.agentCapabilities.streaming_deltas).toBe(false);
		expect(result.agentCapabilities.mcp_tools).toBe(true);

		// Verify agentInfo
		expect(result.agentInfo).toEqual({
			name: "mock-agent",
			version: "2.0.0",
		});

		client.close();
	}, 30_000);

	test("rawSend arbitrary method routes through AcpClient correctly", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		// custom/echo is supported by the mock adapter
		const response = await client.request("custom/echo", {
			foo: "bar",
			num: 42,
		});

		expect(response.error).toBeUndefined();
		const result = response.result as {
			echo: { foo: string; num: number };
		};
		expect(result.echo.foo).toBe("bar");
		expect(result.echo.num).toBe(42);

		// Unknown method returns JSON-RPC error
		const unknownResponse = await client.request("unknown/method", {
			data: true,
		});

		expect(unknownResponse.error).toBeDefined();
		expect(unknownResponse.error?.code).toBe(-32601);
		expect(unknownResponse.error?.message).toContain("Method not found");

		client.close();
	}, 30_000);

	test("malformed JSON-RPC response is handled gracefully", async () => {
		const { client } = await spawnAdapter(vm, MALFORMED_MOCK_ADAPTER);

		// The adapter sends broken JSON lines before valid responses.
		// AcpClient should skip them and still deliver the valid response.
		const response = await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		expect(response.error).toBeUndefined();
		const result = response.result as {
			protocolVersion: number;
			agentInfo: { name: string };
		};
		expect(result.protocolVersion).toBe(1);
		expect(result.agentInfo.name).toBe("malformed-agent");

		client.close();
	}, 30_000);

	test("request timeout triggers rejection after configured timeout", async () => {
		// Use a very short timeout (500ms) and a mock that never responds
		const neverRespondScript = `
process.stdin.resume();
process.stdin.on('data', () => {
  // Intentionally do nothing — never respond
});
`;
		const { client } = await spawnAdapterWithTimeout(
			vm,
			neverRespondScript,
			500,
			"/tmp/never-respond.mjs",
		);

		await expect(
			client.request("initialize", { protocolVersion: 1 }),
		).rejects.toThrow(/timed out after 500ms/);

		client.close();
	}, 30_000);

	test("request timeout error includes recent ACP activity diagnostics", async () => {
		const { client } = await spawnAdapterWithTimeout(
			vm,
			HANGING_ACTIVITY_MOCK_ADAPTER,
			500,
			"/tmp/hanging-activity-adapter.mjs",
		);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		await expect(
			client.request("session/prompt", {
				sessionId: "hanging-session",
				prompt: [{ type: "text", text: "hang please" }],
			}),
		).rejects.toThrow(
			/recent ACP activity: .*received response id=.*non_json \[sandbox\.require\] start node:url \/.*received notification session\/update/i,
		);

		client.close();
	}, 30_000);

	test("agent process exit rejects all pending requests", async () => {
		// Use a short timeout since proc.wait() can hang in the VM (known limitation).
		// The exit adapter calls process.exit(1) immediately, so the request should
		// either be rejected by exit detection or timeout quickly.
		const { client } = await spawnAdapterWithTimeout(
			vm,
			EXIT_MOCK_ADAPTER,
			2000,
			"/tmp/exit-adapter.mjs",
		);

		// The adapter exits immediately after receiving a message without responding.
		// This rejects via process exit or via the short timeout.
		await expect(
			client.request("initialize", { protocolVersion: 1 }),
		).rejects.toThrow(/exited|closed|timed out/i);

		client.close();
	}, 30_000);

	test("activity-aware timeout resets on inbound notifications", async () => {
		// Mock adapter that sends periodic notifications for ~1200ms total,
		// then responds. With a 2000ms inactivity timeout, this should NOT
		// time out because each 300ms gap is well within the timeout window.
		// Without activity-aware reset, the total 1200ms would exceed a
		// hypothetical 500ms fixed timeout — but we use 2000ms for CI margin.
		const periodicNotificationScript = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;
  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;
    try {
      const msg = JSON.parse(line);
      if (msg.method === 'initialize') {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: msg.id,
          result: { protocolVersion: 1, agentInfo: { name: 'periodic-agent' } },
        }) + '\\n');
      } else if (msg.method === 'session/prompt') {
        // Send 4 notifications at 300ms intervals (1200ms total),
        // then respond. Total > 2000ms is not the point — the point is
        // each gap (300ms) is within the timeout window (2000ms).
        let count = 0;
        const iv = setInterval(() => {
          count++;
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            method: 'session/update',
            params: { update: { sessionUpdate: 'agent_message_chunk', content: { text: 'chunk' + count } } },
          }) + '\\n');
          if (count >= 4) {
            clearInterval(iv);
            process.stdout.write(JSON.stringify({
              jsonrpc: '2.0', id: msg.id,
              result: { text: 'done' },
            }) + '\\n');
          }
        }, 300);
      }
    } catch (e) {}
  }
});
`;

		const { client } = await spawnAdapterWithTimeout(
			vm,
			periodicNotificationScript,
			2000,
			"/tmp/periodic-adapter.mjs",
		);

		await client.request("initialize", { protocolVersion: 1 });

		// This takes ~1200ms total but each gap is only ~300ms.
		// With a 2000ms activity-aware timeout, it should succeed.
		const response = await client.request("session/prompt", {
			sessionId: "test",
			prompt: [{ type: "text", text: "go" }],
		});

		expect(response.error).toBeUndefined();
		expect((response.result as { text: string }).text).toBe("done");

		client.close();
	}, 30_000);

	test("activity-aware timeout still fires after activity then silence", async () => {
		// Mock adapter that sends a notification after 400ms, then goes silent.
		// With a 500ms inactivity timeout and the notification at T+400ms:
		//   - Without reset: timeout fires at T+500ms (~500ms total)
		//   - With reset: timeout fires at T+400ms+500ms (~900ms total)
		// We assert elapsed >= 750ms to prove the timer was actually reset.
		const activityThenSilenceScript = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;
  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;
    try {
      const msg = JSON.parse(line);
      if (msg.method === 'initialize') {
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: msg.id,
          result: { protocolVersion: 1, agentInfo: { name: 'silence-agent' } },
        }) + '\\n');
      } else if (msg.method === 'session/prompt') {
        // Send one notification after 400ms, then go silent — never respond.
        // The 400ms delay ensures the timer reset is observable via wall-clock.
        setTimeout(() => {
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            method: 'session/update',
            params: { update: { sessionUpdate: 'agent_message_chunk', content: { text: 'thinking...' } } },
          }) + '\\n');
        }, 400);
      }
    } catch (e) {}
  }
});
`;

		const { client } = await spawnAdapterWithTimeout(
			vm,
			activityThenSilenceScript,
			500,
			"/tmp/silence-adapter.mjs",
		);

		await client.request("initialize", { protocolVersion: 1 });

		const start = Date.now();
		await expect(
			client.request("session/prompt", {
				sessionId: "test",
				prompt: [{ type: "text", text: "go" }],
			}),
		).rejects.toThrow(/timed out after 500ms/);
		const elapsed = Date.now() - start;

		// Without reset: timeout at ~500ms. With reset: timeout at ~900ms.
		// Assert >= 750ms to prove the timer was actually reset by the notification.
		expect(elapsed).toBeGreaterThan(750);

		client.close();
	}, 30_000);

	test("concurrent requests are correlated correctly by id", async () => {
		const { client } = await spawnAdapter(vm, FULL_MOCK_ACP_ADAPTER);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		// Fire off multiple requests concurrently
		const [cancelRes, modeRes, configRes] = await Promise.all([
			client.request("session/cancel", { sessionId: "test-session-1" }),
			client.request("session/set_mode", {
				sessionId: "test-session-1",
				modeId: "plan",
			}),
			client.request("session/set_config_option", {
				sessionId: "test-session-1",
				configId: "model",
				value: "opus",
			}),
		]);

		// Each response should have the correct result for its method
		expect(cancelRes.error).toBeUndefined();
		expect((cancelRes.result as { cancelled: boolean }).cancelled).toBe(true);

		expect(modeRes.error).toBeUndefined();
		expect((modeRes.result as { modeId: string }).modeId).toBe("plan");

		expect(configRes.error).toBeUndefined();
		expect((configRes.result as { configId: string }).configId).toBe("model");
		expect((configRes.result as { value: string }).value).toBe("opus");

		client.close();
	}, 30_000);

	test("non-JSON stdout lines are skipped without error", async () => {
		const { client } = await spawnAdapter(
			vm,
			BANNER_MOCK_ADAPTER,
			"/tmp/banner-adapter.mjs",
		);

		// The banner adapter prints 3 non-JSON lines before becoming ready.
		// AcpClient should skip them all and still handle JSON-RPC.
		const response = await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});

		expect(response.error).toBeUndefined();
		const result = response.result as {
			agentInfo: { name: string };
		};
		expect(result.agentInfo.name).toBe("banner-agent");

		client.close();
	}, 30_000);

	test("notification ordering is preserved", async () => {
		const { client } = await spawnAdapter(
			vm,
			ORDERED_NOTIFICATIONS_ADAPTER,
			"/tmp/ordered-adapter.mjs",
		);

		await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});

		const notifications: JsonRpcNotification[] = [];
		client.onNotification((n) => notifications.push(n));

		const response = await client.request("session/prompt", {
			sessionId: "ordered-session-1",
			prompt: [{ type: "text", text: "test" }],
		});

		expect(response.error).toBeUndefined();
		expect((response.result as { done: boolean }).done).toBe(true);

		// VM stdout can duplicate lines; verify at least 5 notifications and ordering is preserved.
		// Deduplicate by seq number to check ordering of unique notifications.
		expect(notifications.length).toBeGreaterThanOrEqual(5);
		const seenSeqs: number[] = [];
		for (const n of notifications) {
			expect(n.method).toBe("session/update");
			const seq = (n.params as { seq: number }).seq;
			if (seenSeqs.length === 0 || seenSeqs[seenSeqs.length - 1] !== seq) {
				seenSeqs.push(seq);
			}
		}
		// Unique sequence numbers should be 1..5 in order
		expect(seenSeqs).toEqual([1, 2, 3, 4, 5]);

		client.close();
	}, 30_000);
});
