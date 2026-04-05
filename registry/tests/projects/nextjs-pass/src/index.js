"use strict";

var fs = require("fs");
var path = require("path");

var projectDir = path.resolve(__dirname, "..");
var buildManifestPath = path.join(
	projectDir,
	".next",
	"build-manifest.json",
);

function readManifest() {
	return JSON.parse(fs.readFileSync(buildManifestPath, "utf8"));
}

function ensureBuild() {
	try {
		readManifest();
		return;
	} catch (e) {
		// Build manifest missing — run build
	}
	var execSync = require("child_process").execSync;
	var nextBin = path.join(projectDir, "node_modules", ".bin", "next");
	var buildEnv = Object.assign({}, process.env);
	if (!buildEnv.PATH) {
		buildEnv.PATH =
			"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
	}
	buildEnv.NEXT_TELEMETRY_DISABLED = "1";
	execSync(nextBin + " build", {
		cwd: projectDir,
		stdio: "pipe",
		timeout: 30000,
		env: buildEnv,
	});
}

function main() {
	ensureBuild();

	var manifest = readManifest();
	var pages = Object.keys(manifest.pages).sort();

	var results = [];

	results.push({ check: "build-manifest", pages: pages });

	var indexHtml = fs.readFileSync(
		path.join(projectDir, ".next", "server", "pages", "index.html"),
		"utf8",
	);
	results.push({
		check: "ssr-page",
		rendered: indexHtml.indexOf("Hello from Next.js") !== -1,
	});

	var apiRouteExists = true;
	try {
		fs.readFileSync(
			path.join(
				projectDir,
				".next",
				"server",
				"pages",
				"api",
				"hello.js",
			),
			"utf8",
		);
	} catch (e) {
		apiRouteExists = false;
	}
	results.push({ check: "api-route", compiled: apiRouteExists });

	console.log(JSON.stringify(results));
}

main();
