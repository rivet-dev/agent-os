import { serve } from "@hono/node-server";
import { appsRouter } from "@rivet-dev/agentos-apps";
import { Hono } from "hono";
import { registry } from "./actors.js";

// Start the actor registry before routing applications.
registry.start();

const server = new Hono();

// Mount every deployed application at /apps/:appId.
server.route("/apps", appsRouter);

// Serve the host router over HTTP.
serve({
	fetch: server.fetch,
	port: 3000,
});
