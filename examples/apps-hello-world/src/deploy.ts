import { deployApp } from "@rivet-dev/agentos-apps";

// An agent, upload endpoint, or any other part of the system can call
// deployApp() with the files it generated.
await deployApp({
	appId: "hello-world",
	files: {
		"package.json": JSON.stringify({
			name: "hello-world-app",
			version: "0.0.0",
			private: true,
			type: "module",
			main: "src/index.ts",
			dependencies: {
				hono: "^4.12.9",
			},
		}),
		"src/index.ts": `
import { Hono } from "hono";

const app = new Hono();

// Serve the application's frontend.
app.get("/", (c) => c.html("<h1>Hello from agentOS Apps</h1>"));

// Serve a REST API request from the same application.
app.get("/api/hello", (c) => c.json({ message: "Hello from agentOS Apps" }));

export default app;
`,
	},
});
