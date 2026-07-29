export const CONFORMANCE_AGENT_NAME = "conformance-agent";

// A deterministic ACP peer used by both the direct Core and real-actor suites.
// It deliberately covers notifications, permission callbacks, cancellation,
// native resume, modes/config, agent metadata, and arbitrary raw requests.
export const CONFORMANCE_ACP_ADAPTER = String.raw`
let buffer = "";
let nextSession = 0;
let nextRequest = 1000;
const pending = new Map();
const sessions = new Map();

const modes = () => ({
  currentModeId: "default",
  availableModes: [
    { id: "default", label: "Default" },
    { id: "plan", label: "Plan" },
  ],
});
const configOptions = () => [
  { id: "model", category: "model", label: "Model", currentValue: "test-model" },
  { id: "thought_level", category: "thought_level", label: "Thought Level", currentValue: "medium" },
];
function write(value) { process.stdout.write(JSON.stringify(value) + "\n"); }
function result(id, value) { write({ jsonrpc: "2.0", id, result: value }); }
function error(id, code, message) { write({ jsonrpc: "2.0", id, error: { code, message } }); }
function notify(method, params) { write({ jsonrpc: "2.0", method, params }); }
function request(method, params) {
  const id = nextRequest++;
  write({ jsonrpc: "2.0", id, method, params });
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}
function state(sessionId) {
  if (!sessions.has(sessionId)) sessions.set(sessionId, { mode: "default", model: "test-model", thought: "medium" });
  return sessions.get(sessionId);
}
function sessionResult(sessionId) {
  return { sessionId, modes: modes(), configOptions: configOptions() };
}
async function handle(msg) {
  if (msg.method === undefined && msg.id !== undefined) {
    const waiter = pending.get(msg.id);
    if (waiter) {
      pending.delete(msg.id);
      if (msg.error) waiter.reject(new Error(msg.error.message));
      else waiter.resolve(msg.result);
    }
    return;
  }
  if (msg.id === undefined) return;
  switch (msg.method) {
    case "initialize":
      result(msg.id, {
        protocolVersion: 1,
        agentInfo: { name: "conformance-agent", version: "1.0.0" },
        agentCapabilities: { loadSession: true, promptCapabilities: { image: true, audio: true, embeddedContext: true } },
        modes: modes(),
        configOptions: configOptions(),
      });
      return;
    case "session/new": {
      const sessionId = "conformance-session-" + (++nextSession);
      state(sessionId);
      result(msg.id, sessionResult(sessionId));
      return;
    }
    case "session/load":
    case "session/resume":
      state(msg.params.sessionId);
      result(msg.id, sessionResult(msg.params.sessionId));
      return;
    case "session/prompt": {
      const sessionId = msg.params.sessionId;
      const prompt = msg.params.prompt || [];
      const text = prompt.map((part) => part.type === "text" ? part.text : "").join("");
      if (text.includes("crash-adapter")) process.exit(23);
      if (text.includes("rich-media")) {
        notify("session/update", {
          sessionId,
          update: {
            sessionUpdate: "tool_call",
            toolCallId: "rich-media-tool-1",
            title: "Read rich media",
            kind: "read",
            status: "completed",
            content: prompt.slice(1).map((content) => ({ type: "content", content })),
          },
        });
        for (const content of prompt) {
          notify("session/update", { sessionId, update: { sessionUpdate: "agent_message_chunk", content } });
        }
        result(msg.id, { stopReason: "end_turn" });
        return;
      }
      notify("session/update", { sessionId, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "echo:" + text } } });
      if (text.includes("permission")) {
        const reply = await request("session/request_permission", {
          sessionId,
          toolCall: { toolCallId: "binding-call-1", title: "Conformance binding" },
          options: [
            { optionId: "allow_once", name: "Allow once", kind: "allow_once" },
            { optionId: "reject_once", name: "Reject", kind: "reject_once" },
          ],
        });
        notify("session/update", { sessionId, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: ":permission:" + JSON.stringify(reply) } } });
      }
      result(msg.id, { stopReason: "end_turn" });
      return;
    }
    case "session/cancel":
      result(msg.id, {});
      return;
    case "session/set_mode":
      state(msg.params.sessionId).mode = msg.params.modeId;
      result(msg.id, {});
      return;
    case "session/set_config_option": {
      const target = state(msg.params.sessionId);
      if (msg.params.configId === "model") target.model = msg.params.value;
      if (msg.params.configId === "thought_level") target.thought = msg.params.value;
      result(msg.id, { configOptions: configOptions().map((entry) => ({
        ...entry,
        currentValue: entry.id === "model" ? target.model : target.thought,
      })) });
      return;
    }
    case "conformance/echo":
      result(msg.id, { echoed: msg.params });
      return;
    default:
      error(msg.id, -32601, "Method not found: " + msg.method);
  }
}
process.stdin.resume();
process.stdin.on("data", (chunk) => {
  buffer += chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  while (true) {
    const index = buffer.indexOf("\n");
    if (index < 0) break;
    const line = buffer.slice(0, index);
    buffer = buffer.slice(index + 1);
    if (line.trim()) void handle(JSON.parse(line)).catch((cause) => process.stderr.write(String(cause) + "\n"));
  }
});
`;
