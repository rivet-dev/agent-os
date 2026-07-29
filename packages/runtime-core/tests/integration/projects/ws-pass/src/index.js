"use strict";

const { WebSocket, WebSocketServer } = require("ws");

async function main() {
	const serverEvents = [];
	const clientEvents = [];
	const serverCloseEvents = [];

	// Start server on random port
	const wss = new WebSocketServer({ port: 0 });

	wss.on("connection", (ws) => {
		serverEvents.push("connection");
		serverCloseEvents.push(
			new Promise((resolve) => {
				ws.once("close", resolve);
			}),
		);

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
			let echo;

			ws.on("open", () => {
				clientEvents.push("open");
				ws.send("hello-ws");
			});

			ws.on("message", (data) => {
				clientEvents.push("text-message");
				echo = data.toString();
				ws.close();
			});

			ws.on("close", () => {
				clientEvents.push("text-close");
				if (echo === undefined) {
					reject(new Error("text socket closed before echo"));
					return;
				}
				resolve(echo);
			});

			ws.on("error", reject);
		});

		await serverCloseEvents[0];

		const binaryEcho = await new Promise((resolve, reject) => {
			const ws = new WebSocket(`ws://127.0.0.1:${port}`);
			let echo;

			ws.on("open", () => {
				clientEvents.push("binary-open");
				ws.send(Buffer.from([0xde, 0xad, 0xbe, 0xef]));
			});

			ws.on("message", (data, isBinary) => {
				clientEvents.push("binary-message");
				echo = {
					isBinary,
					hex: Buffer.from(data).toString("hex"),
				};
				ws.close();
			});

			ws.on("close", () => {
				clientEvents.push("binary-close");
				if (echo === undefined) {
					reject(new Error("binary socket closed before echo"));
					return;
				}
				resolve(echo);
			});

			ws.on("error", reject);
		});

		await serverCloseEvents[1];

		if (typeof globalThis.WebSocket !== "function") {
			throw new Error("Node global WebSocket is unavailable");
		}
		const globalEcho = await new Promise((resolve, reject) => {
			const socket = new globalThis.WebSocket(`ws://127.0.0.1:${port}`);
			let echo;
			socket.addEventListener("open", () => {
				clientEvents.push("global-open");
				socket.send("hello-global-ws");
			});
			socket.addEventListener("message", (event) => {
				clientEvents.push("global-message");
				echo = String(event.data);
				socket.close();
			});
			socket.addEventListener("close", () => {
				clientEvents.push("global-close");
				if (echo === undefined) {
					reject(new Error("global socket closed before echo"));
					return;
				}
				resolve(echo);
			});
			socket.addEventListener("error", (event) => {
				reject(
					event.error instanceof Error
						? event.error
						: new Error(`global websocket error: ${event.message || event.type || "unknown"}`),
				);
			});
		});

		await serverCloseEvents[2];

		const result = {
			textEcho,
			binaryEcho,
			globalEcho,
			serverEvents: serverEvents.sort(),
			clientEvents: clientEvents.sort(),
		};

		console.log(JSON.stringify(result));
	} finally {
		await new Promise((resolve) => wss.close(resolve));
	}
}

main().catch((err) => {
	console.error(err?.stack || err?.message || JSON.stringify(err));
	process.exit(1);
});
