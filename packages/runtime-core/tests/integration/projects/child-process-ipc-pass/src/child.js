"use strict";

var fs = require("fs");

var initialized = false;

function handleMessage(message) {
	if (message.kind === "initialize") {
		initialized = message.sequence === 1;
		return;
	}

	try {
		process.send({
			kind: "reply",
			initialized: initialized,
			sequence: message.sequence,
			requestId: message.requestId,
			payload: message.payload.toUpperCase(),
			execArgv: process.execArgv,
			preloaded: globalThis.__agentOSPreloaded,
			circular: message.self === message,
			mapValue: message.map.get("answer"),
			bytes: Array.from(message.bytes),
			sharedAfterFork: fs.readFileSync(message.sharedPath, "utf8"),
		});
	} catch (error) {
		process.send({
			kind: "error",
			code: error && error.code,
			message: error && error.message,
		});
	}
}

process.on("message", handleMessage);
