"use strict";

const http = require("http");
const express = require("express");

const app = express();

app.use(express.json());
app.use(express.urlencoded({ extended: false }));

app.get("/hello", (req, res) => {
	res.json({ message: "hello" });
});

app.get("/users/:id", (req, res) => {
	res.json({ id: req.params.id, name: "test-user" });
});

app.post("/data", (req, res) => {
	res.json({ method: req.method, url: req.url });
});

function request(method, path, port) {
	return new Promise((resolve, reject) => {
		const req = http.request(
			{ hostname: "127.0.0.1", port, path, method },
			(res) => {
				let body = "";
				res.on("data", (chunk) => (body += chunk));
				res.on("end", () => resolve({ status: res.statusCode, body }));
			},
		);
		req.on("error", reject);
		req.end();
	});
}

async function main() {
	const server = http.createServer(app);
	await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
	const port = server.address().port;

	try {
		const results = [];

		const r1 = await request("GET", "/hello", port);
		results.push({ route: "GET /hello", status: r1.status, body: JSON.parse(r1.body) });

		const r2 = await request("GET", "/users/42", port);
		results.push({ route: "GET /users/42", status: r2.status, body: JSON.parse(r2.body) });

		const r3 = await request("POST", "/data", port);
		results.push({ route: "POST /data", status: r3.status, body: JSON.parse(r3.body) });

		console.log(JSON.stringify(results));
	} finally {
		await new Promise((resolve) => server.close(resolve));
	}
}

main().catch((err) => {
	console.error(err.message);
	process.exit(1);
});
