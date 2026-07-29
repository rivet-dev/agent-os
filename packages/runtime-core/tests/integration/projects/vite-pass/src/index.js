"use strict";

var path = require("path");

var projectDir = path.resolve(__dirname, "..");

async function runBuild() {
	var vite = await import("vite");
	return vite.build({
		root: projectDir,
		configFile: false,
		esbuild: false,
		logLevel: "silent",
		build: { minify: false, write: false },
	});
}

async function main() {
	var build = await runBuild();
	var builds = Array.isArray(build) ? build : [build];
	var output = builds.flatMap(function (result) {
		return result.output;
	});
	var html = output.find(function (entry) {
		return entry.fileName === "index.html";
	});
	var chunks = output.filter(function (entry) {
		return entry.type === "chunk";
	});
	var results = [
		{
			check: "index-html",
			hasRoot: String(html && html.source).indexOf('id="root"') !== -1,
			hasScript: String(html && html.source).indexOf(".js") !== -1,
		},
		{
			check: "bundle",
			hasJs: chunks.length > 0,
			hasContent: chunks.some(function (entry) {
				return entry.code.indexOf("Hello from Vite") !== -1;
			}),
		},
	];

	console.log(JSON.stringify(results));
}

main().catch(function (error) {
	console.error(error);
	process.exitCode = 1;
});
