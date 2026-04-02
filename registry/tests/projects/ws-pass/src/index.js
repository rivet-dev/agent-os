"use strict";

const { WebSocket, WebSocketServer } = require("ws");

async function main() {
	const serverEvents = [];
	const clientEvents = [];

	// Start server on random port
	const wss = new WebSocketServer({ port: 0 });

	wss.on("connection", (ws) => {
		serverEvents.push("connection");

		ws.on("message", (data, isBinary) => {
			serverEvents.push(isBinary ? "binary-message" : "text-message");
			// Echo back
			ws.send(data, { binary: isBinary });
		});

		ws.on("close", () => {
			serverEvents.push("close");
		});
	});

	await new Promise((resolve) => wss.on("listening", resolve));
	const port = wss.address().port;

	try {
		const textEcho = await new Promise((resolve, reject) => {
			const ws = new WebSocket(`ws://127.0.0.1:${port}`);

			ws.on("open", () => {
				clientEvents.push("open");
				ws.send("hello-ws");
			});

			ws.on("message", (data) => {
				clientEvents.push("text-message");
				ws.close();
				resolve(data.toString());
			});

			ws.on("close", () => {
				clientEvents.push("text-close");
			});

			ws.on("error", reject);
		});

		// Wait briefly for server close event
		await new Promise((resolve) => setTimeout(resolve, 50));

		const binaryEcho = await new Promise((resolve, reject) => {
			const ws = new WebSocket(`ws://127.0.0.1:${port}`);

			ws.on("open", () => {
				clientEvents.push("binary-open");
				ws.send(Buffer.from([0xde, 0xad, 0xbe, 0xef]));
			});

			ws.on("message", (data, isBinary) => {
				clientEvents.push("binary-message");
				ws.close();
				resolve({
					isBinary,
					hex: Buffer.from(data).toString("hex"),
				});
			});

			ws.on("close", () => {
				clientEvents.push("binary-close");
			});

			ws.on("error", reject);
		});

		// Wait briefly for server close event
		await new Promise((resolve) => setTimeout(resolve, 50));

		const result = {
			textEcho,
			binaryEcho,
			serverEvents: serverEvents.sort(),
			clientEvents: clientEvents.sort(),
		};

		console.log(JSON.stringify(result));
	} finally {
		await new Promise((resolve) => wss.close(resolve));
	}
}

main().catch((err) => {
	console.error(err.message);
	process.exit(1);
});
