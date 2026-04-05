// Extension example: custom mounts and MCP servers.
//
// To mount a host directory, provide a VirtualFileSystem driver:
//
//   const os = await AgentOs.create({
//     mounts: [{ path: "/project", driver: myHostDriver, readOnly: true }],
//   });

import { AgentOs } from "@rivet-dev/agent-os";

const os = await AgentOs.create();

const { sessionId } = await os.createSession("pi", {
  mcpServers: [{ type: "local", command: "npx", args: ["@playwright/mcp"] }],
  cwd: "/project",
});
