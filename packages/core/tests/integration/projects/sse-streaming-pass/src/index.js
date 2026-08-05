"use strict";

const http = require("http");

// SSE events to send — exercises data-only, named events, id field, retry field
const sseEvents = [
	"retry: 3000\n\n",
	"data: hello-world\n\n",
	"event: status\ndata: {\"connected\":true}\n\n",
	"id: msg-3\nevent: update\ndata: first line\ndata: second line\n\n",
	"id: msg-4\ndata: final-event\n\n",
];

function createSSEServer() {
	return http.createServer((req, res) => {
		if (req.url !== "/events") {
			res.writeHead(404);
			res.end();
			return;
		}

		res.writeHead(200, {
			"Content-Type": "text/event-stream",
			"Cache-Control": "no-cache",
			Connection: "keep-alive",
		});

		// Send all events then close
		for (const event of sseEvents) {
			res.write(event);
		}
		res.end();
	});
}

// Parse SSE text/event-stream format into structured events
function parseSSEStream(raw) {
	const events = [];
	let current = {};

	for (const line of raw.split("\n")) {
		if (line === "") {
			// Empty line = event boundary
			if (Object.keys(current).length > 0) {
				events.push(current);
				current = {};
			}
			continue;
		}

		const colonIdx = line.indexOf(":");
		if (colonIdx === 0) continue; // comment line

		let field, value;
		if (colonIdx > 0) {
			field = line.slice(0, colonIdx);
			// Strip single leading space after colon per SSE spec
			value = line.slice(colonIdx + 1);
			if (value.startsWith(" ")) value = value.slice(1);
		} else {
			field = line;
			value = "";
		}

		if (field === "data") {
			// Multiple data fields are joined with newline
			current.data = current.data != null ? current.data + "\n" + value : value;
		} else {
			current[field] = value;
		}
	}

	// Trailing event without final blank line
	if (Object.keys(current).length > 0) {
		events.push(current);
	}

	return events;
}

async function main() {
	const server = createSSEServer();
	await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
	const port = server.address().port;

	try {
		const response = await new Promise((resolve, reject) => {
			http.get(
				{ hostname: "127.0.0.1", port, path: "/events" },
				(res) => {
					let body = "";
					res.on("data", (chunk) => (body += chunk));
					res.on("end", () =>
						resolve({
							statusCode: res.statusCode,
							headers: res.headers,
							body,
						}),
					);
				},
			).on("error", reject);
		});

		const headers = {
			contentType: response.headers["content-type"],
			connection: response.headers["connection"],
			cacheControl: response.headers["cache-control"],
		};

		const events = parseSSEStream(response.body);

		const result = {
			statusCode: response.statusCode,
			headers,
			eventCount: events.length,
			events,
		};

		console.log(JSON.stringify(result));
	} finally {
		await new Promise((resolve) => server.close(resolve));
	}
}

main().catch((err) => {
	console.error(err.message);
	process.exit(1);
});
