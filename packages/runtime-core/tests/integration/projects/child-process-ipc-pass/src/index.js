"use strict";

var path = require("path");
var fork = require("child_process").fork;
var fs = require("fs");

var child = fork(path.join(__dirname, "child.js"), [], {
	stdio: ["pipe", "pipe", "pipe", "ipc"],
	execArgv: ["--experimental-import-meta-resolve", "--require", "./src/preload.cjs"],
	serialization: "advanced",
});
async function main() {
	var streamMethods = {
		stdoutPipe: typeof child.stdout.pipe,
		stdoutUnpipe: typeof child.stdout.unpipe,
		stderrPipe: typeof child.stderr.pipe,
		stderrUnpipe: typeof child.stderr.unpipe,
	};
	var response = new Promise(function (resolve, reject) {
		child.on("message", resolve);
		child.once("error", reject);
	});
	var exited = new Promise(function (resolve) {
		child.once("exit", function (code, signal) {
			resolve({ code: code, signal: signal });
		});
	});

	child.send({ kind: "initialize", sequence: 1 });
	setTimeout(function () {
		fs.writeFileSync("/tmp/agentos-shared-after-fork.txt", "shared-after-fork");
		var request = {
			kind: "request",
			sequence: 2,
			requestId: 7,
			payload: "agentos",
			map: new Map([["answer", 42]]),
			bytes: Buffer.from([1, 2, 3]),
			sharedPath: "/tmp/agentos-shared-after-fork.txt",
		};
		request.self = request;
		child.send(request);
	}, 1000);
	var timeout;
	var timedResponse = new Promise(function (resolve, reject) {
		timeout = setTimeout(function () {
			child.kill("SIGKILL");
			reject(new Error("fork IPC response timed out"));
		}, 10000);
		response.then(resolve, reject);
	});

	var message = await timedResponse;
	fs.unlinkSync("/tmp/agentos-shared-after-fork.txt");
	clearTimeout(timeout);
	child.kill("SIGINT");
	var exit = await exited;
	var outputPath = path.join(__dirname, "ipc-output.tmp");
	await fs.promises.writeFile(outputPath, "post-exit");
	var postExit = await fs.promises.readFile(outputPath, "utf8");
	await fs.promises.unlink(outputPath);
	console.log(
		JSON.stringify({ message: message, exit: exit, postExit: postExit, streamMethods: streamMethods }),
	);
}

main().catch(function (error) {
	console.error(error);
	process.exitCode = 1;
});
