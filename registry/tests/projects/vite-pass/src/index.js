"use strict";

var fs = require("fs");
var path = require("path");

var projectDir = path.resolve(__dirname, "..");
var distDir = path.join(projectDir, "dist");

function ensureBuild() {
	try {
		fs.statSync(path.join(distDir, "index.html"));
		return;
	} catch (e) {
		// Build output missing — run build
	}
	var execSync = require("child_process").execSync;
	var viteBin = path.join(projectDir, "node_modules", ".bin", "vite");
	var buildEnv = Object.assign({}, process.env);
	if (!buildEnv.PATH) {
		buildEnv.PATH =
			"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
	}
	execSync(viteBin + " build", {
		cwd: projectDir,
		stdio: "pipe",
		timeout: 30000,
		env: buildEnv,
	});
}

function main() {
	ensureBuild();

	var results = [];

	// Check index.html was generated
	var indexHtml = fs.readFileSync(path.join(distDir, "index.html"), "utf8");
	results.push({
		check: "index-html",
		exists: true,
		hasReactRoot: indexHtml.indexOf('id="root"') !== -1,
		hasScript: indexHtml.indexOf(".js") !== -1,
	});

	// Check assets directory
	var assetsDir = path.join(distDir, "assets");
	var assets = fs.readdirSync(assetsDir).sort();
	var hasJs = assets.some(function (f) {
		return f.endsWith(".js");
	});
	results.push({
		check: "assets",
		hasJs: hasJs,
	});

	// Check compiled JS contains React component
	var jsContent = "";
	assets.forEach(function (f) {
		if (f.endsWith(".js")) {
			jsContent += fs.readFileSync(path.join(assetsDir, f), "utf8");
		}
	});
	results.push({
		check: "react-compiled",
		hasComponent: jsContent.indexOf("Hello from Vite") !== -1,
	});

	console.log(JSON.stringify(results));
}

main();
