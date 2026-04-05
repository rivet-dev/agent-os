"use strict";

const { Client, Server, utils } = require("ssh2");

const result = {
	clientExists: typeof Client === "function",
	clientMethods: [
		"connect",
		"end",
		"exec",
		"sftp",
		"shell",
		"forwardIn",
		"forwardOut",
	].filter((m) => typeof Client.prototype[m] === "function"),
	serverExists: typeof Server === "function",
	utilsExists: typeof utils === "object" && utils !== null,
	parseKey: typeof utils.parseKey === "function",
};

// Create a Client instance and verify it has expected properties
const client = new Client();
result.instanceCreated = client instanceof Client;
result.hasOn = typeof client.on === "function";
result.hasEmit = typeof client.emit === "function";
client.removeAllListeners();

console.log(JSON.stringify(result));
