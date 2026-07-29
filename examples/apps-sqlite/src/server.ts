import { serve } from "@hono/node-server";
import { appsRouter, deployApp } from "@rivet-dev/agentos-apps";
import { Hono } from "hono";
import { registry } from "./actors.js";

registry.start();

const server = new Hono();

// In production, an agent or any other part of the system can upload app files
// here as multipart form data. This static example deploys its checked fixture.
server.post("/deploy/:name", async (context) => {
	return context.json(
		await deployApp({
			appId: context.req.param("name"),
			source: new URL("../fixtures/app/", import.meta.url),
		}),
	);
});

server.route("/apps", appsRouter);

serve({
	fetch: server.fetch,
	port: 3000,
});
