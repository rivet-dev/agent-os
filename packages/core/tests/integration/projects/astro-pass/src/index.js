"use strict";

var fs = require("fs");
var path = require("path");

var projectDir = path.resolve(__dirname, "..");
var distDir = path.join(projectDir, "dist");

async function ensureBuild() {
	try {
		fs.statSync(path.join(distDir, "index.html"));
		return;
	} catch (e) {
		// Build output missing — run build
	}
	process.env.ASTRO_TELEMETRY_DISABLED = "1";
	var astro = await import("astro");
	var pathToFileURL = require("url").pathToFileURL;
	var timeout;
	try {
		await Promise.race([
			astro.build({
				root: pathToFileURL(projectDir + path.sep),
				logLevel: "silent",
			}),
			new Promise(function (_resolve, reject) {
				timeout = setTimeout(function () {
					reject(new Error("Astro React build did not settle"));
				}, 20000);
			}),
		]);
	} finally {
		clearTimeout(timeout);
	}
}

async function main() {
	await ensureBuild();

	var results = [];

	// Check index.html was generated
	var indexHtml = fs.readFileSync(path.join(distDir, "index.html"), "utf8");
	results.push({
		check: "index-html",
		exists: true,
		hasContent: indexHtml.indexOf("Hello from Astro") !== -1,
		hasScript: indexHtml.indexOf("<script") !== -1,
	});

	// Check for hydrated island (astro-island custom element)
	results.push({
		check: "island-hydration",
		hasIsland: indexHtml.indexOf("astro-island") !== -1,
	});

	// Check _astro assets directory for client JS
	var astroAssetsDir = path.join(distDir, "_astro");
	var hasClientJs = false;
	try {
		var assets = fs.readdirSync(astroAssetsDir);
		hasClientJs = assets.some(function (f) {
			return f.endsWith(".js");
		});
	} catch (e) {
		// _astro dir might not exist if no client-side JS
	}
	results.push({
		check: "client-assets",
		hasClientJs: hasClientJs,
	});

	console.log(JSON.stringify(results));
}

main().catch(function (error) {
	console.error(error);
	process.exit(1);
});
