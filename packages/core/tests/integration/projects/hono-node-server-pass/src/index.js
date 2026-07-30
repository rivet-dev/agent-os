"use strict";

const http = require("http");
const { serve } = require("@hono/node-server");
const { Hono } = require("hono");

const app = new Hono();

app.get("/hello", (context) => context.json({ message: "hello" }));
app.get("/users/:id", (context) =>
	context.json({ id: context.req.param("id"), name: "test-user" }),
);
app.post("/data", async (context) =>
	context.json({ method: context.req.method, received: await context.req.json() }),
);

function request(method, path, port, body) {
	return new Promise((resolve, reject) => {
		const payload = body === undefined ? undefined : JSON.stringify(body);
		const req = http.request(
			{
				hostname: "127.0.0.1",
				port,
				path,
				method,
				headers: payload
					? {
							"Content-Type": "application/json",
							"Content-Length": Buffer.byteLength(payload),
						}
					: undefined,
			},
			(res) => {
				let responseBody = "";
				res.on("data", (chunk) => (responseBody += chunk));
				res.on("end", () =>
					resolve({ status: res.statusCode, body: JSON.parse(responseBody) }),
				);
			},
		);
		req.on("error", reject);
		if (payload !== undefined) req.write(payload);
		req.end();
	});
}

async function main() {
	const server = serve({ fetch: app.fetch, hostname: "127.0.0.1", port: 0 });
	await new Promise((resolve) => server.once("listening", resolve));
	const port = server.address().port;

	try {
		const results = [];
		results.push({ route: "GET /hello", ...(await request("GET", "/hello", port)) });
		results.push({
			route: "GET /users/42",
			...(await request("GET", "/users/42", port)),
		});
		results.push({
			route: "POST /data",
			...(await request("POST", "/data", port, { key: "value" })),
		});
		console.log(JSON.stringify(results));
	} finally {
		await new Promise((resolve, reject) => {
			server.close((error) => (error ? reject(error) : resolve()));
		});
	}
}

main().catch((error) => {
	console.error(error.stack || error.message);
	process.exit(1);
});
