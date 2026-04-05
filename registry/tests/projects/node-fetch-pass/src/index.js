"use strict";

const http = require("http");
const fetch = require("node-fetch");

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

		const r1 = await fetch(base + "/hello");
		const b1 = await r1.json();
		results.push({ route: "GET /hello", status: r1.status, body: b1 });

		const r2 = await fetch(base + "/users/42");
		const b2 = await r2.json();
		results.push({ route: "GET /users/42", status: r2.status, body: b2 });

		const r3 = await fetch(base + "/data", {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ key: "value" }),
		});
		const b3 = await r3.json();
		results.push({ route: "POST /data", status: r3.status, body: b3 });

		console.log(JSON.stringify(results));
	} finally {
		await new Promise((resolve) => server.close(resolve));
	}
}

main().catch((err) => {
	console.error(err.message);
	process.exit(1);
});
