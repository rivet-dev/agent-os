"use strict";

const http = require("http");
const axios = require("axios");

const client = axios.create({ adapter: "fetch" });

const server = http.createServer((req, res) => {
	if (req.method === "GET" && req.url === "/hello") {
		res.writeHead(200, { "Content-Type": "application/json" });
		res.end(JSON.stringify({ message: "hello" }));
	} else if (req.method === "GET" && req.url === "/users/42") {
		res.writeHead(200, { "Content-Type": "application/json" });
		res.end(JSON.stringify({ id: "42", name: "test-user" }));
	} else if (req.method === "POST" && req.url === "/data") {
		let body = "";
		req.on("data", (chunk) => (body += chunk));
		req.on("end", () => {
			res.writeHead(200, { "Content-Type": "application/json" });
			res.end(JSON.stringify({ method: "POST", received: JSON.parse(body) }));
		});
	} else {
		res.writeHead(404);
		res.end();
	}
});

async function main() {
	await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
	const port = server.address().port;
	const base = "http://127.0.0.1:" + port;

	try {
		const results = [];

		const r1 = await client.get(base + "/hello");
		results.push({ route: "GET /hello", status: r1.status, body: r1.data });

		const r2 = await client.get(base + "/users/42");
		results.push({ route: "GET /users/42", status: r2.status, body: r2.data });

		const r3 = await client.post(base + "/data", { key: "value" });
		results.push({ route: "POST /data", status: r3.status, body: r3.data });

		console.log(JSON.stringify(results));
	} finally {
		await new Promise((resolve) => server.close(resolve));
	}
}

main().catch((err) => {
	console.error(err.message);
	process.exit(1);
});
