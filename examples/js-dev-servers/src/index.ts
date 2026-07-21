import { AgentOs } from "@rivet-dev/agentos";

const serverSource = `
import http from "node:http";
const app = http.createServer((req, res) => {
  res.writeHead(200, { "content-type": "application/json" });
  res.end(JSON.stringify({ ok: true, path: req.url }));
});
app.listen(3000, "127.0.0.1", () => console.log("ready"));
await new Promise(() => {});
`;

const runtime = await AgentOs.create({ permissions: { network: "allow" } });

try {
	const server = await runtime.javascript.spawn(serverSource, {
		onStdout: (chunk) => process.stdout.write(new TextDecoder().decode(chunk)),
	});

	// The listener stays inside the VM — no host port is exposed.
	const response = await runtime.network.httpRequest({
		port: 3000,
		path: "/health",
	});
	console.log(response.status); // 200

	await runtime.process.signal(server.pid, "SIGTERM");
	await runtime.process.wait(server.pid);
} finally {
	await runtime.dispose();
}
