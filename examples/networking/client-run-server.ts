import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// Write a simple Node HTTP server and run it inside the VM. It binds a loopback
// port (3000) exactly like any normal Node process.
await agent.filesystem.writeFile(
  "/home/agentos/server.js",
  `const http = require("http");
http.createServer((req, res) => {
  res.writeHead(200, { "Content-Type": "text/plain" });
  res.end("Hello from inside the VM");
}).listen(3000, () => console.log("listening on http://127.0.0.1:3000"));`,
);
const { pid } = await agent.process.spawn("node", ["/home/agentos/server.js"]);
console.log("server pid:", pid);
