"use strict";

const http = require("http");
const Fastify = require("fastify");

const app = Fastify({ logger: false });

app.get("/hello", async () => {
	return { message: "hello" };
});

app.get("/users/:id", async (request) => {
	return { id: request.params.id, name: "test-user" };
});

app.post("/data", async (request) => {
	return { method: request.method, url: request.url, body: request.body };
});

app.get("/async", async () => {
	const value = await Promise.resolve(42);
	return { value };
});

function request(method, path, port, options) {
	return new Promise((resolve, reject) => {
		const headers = (options && options.headers) || {};
		const bodyData = options && options.body;
		const req = http.request(
			{ hostname: "127.0.0.1", port, path, method, headers },
			(res) => {
				let body = "";
				res.on("data", (chunk) => (body += chunk));
				res.on("end", () => resolve({ status: res.statusCode, body }));
			},
		);
		req.on("error", reject);
		if (bodyData) {
			req.write(typeof bodyData === "string" ? bodyData : JSON.stringify(bodyData));
		}
		req.end();
	});
}

async function main() {
	await app.listen({ port: 0, host: "127.0.0.1" });
	const port = app.server.address().port;

	try {
		const results = [];

		const r1 = await request("GET", "/hello", port);
		results.push({ route: "GET /hello", status: r1.status, body: JSON.parse(r1.body) });

		const r2 = await request("GET", "/users/42", port);
		results.push({ route: "GET /users/42", status: r2.status, body: JSON.parse(r2.body) });

		const r3 = await request("POST", "/data", port, {
			headers: { "Content-Type": "application/json" },
			body: { key: "value" },
		});
		results.push({ route: "POST /data", status: r3.status, body: JSON.parse(r3.body) });

		const r4 = await request("GET", "/async", port);
		results.push({ route: "GET /async", status: r4.status, body: JSON.parse(r4.body) });

		console.log(JSON.stringify(results));
	} finally {
		await app.close();
	}
}

main().catch((err) => {
	console.error(err.message);
	process.exit(1);
});
