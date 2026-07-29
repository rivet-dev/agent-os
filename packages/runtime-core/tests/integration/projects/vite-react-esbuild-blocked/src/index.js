"use strict";

var path = require("path");

var projectDir = path.resolve(__dirname, "..");

async function main() {
	var vite = await import("vite");
	var timeout;
	try {
		await Promise.race([
			vite.build({ root: projectDir, logLevel: "silent" }),
			new Promise(function (_resolve, reject) {
				timeout = setTimeout(function () {
					reject(new Error("Vite React build did not settle"));
				}, 20000);
			}),
		]);
	} finally {
		clearTimeout(timeout);
	}
	console.log("Vite React build completed");
}

main().catch(function (error) {
	console.error(error);
	process.exit(1);
});
