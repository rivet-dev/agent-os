"use strict";

const SftpClient = require("ssh2-sftp-client");

const result = {
	classExists: typeof SftpClient === "function",
	methods: [
		"connect",
		"list",
		"get",
		"put",
		"mkdir",
		"rmdir",
		"delete",
		"rename",
		"exists",
		"stat",
		"end",
	].filter((m) => typeof SftpClient.prototype[m] === "function"),
};

// Create a Client instance and verify it has expected properties
const client = new SftpClient();
result.instanceCreated = client instanceof SftpClient;

console.log(JSON.stringify(result));
