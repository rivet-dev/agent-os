"use strict";

var fs = require("fs");
var path = require("path");

var projectDir = path.resolve(__dirname, "..");
var buildManifestPath = path.join(
	projectDir,
	".next",
	"build-manifest.json",
);
var pagesManifestPath = path.join(
	projectDir,
	".next",
	"server",
	"pages-manifest.json",
);

function readManifest() {
	return JSON.parse(fs.readFileSync(buildManifestPath, "utf8"));
}

async function ensureBuild() {
	if (!fs.existsSync(path.join(projectDir, ".babelrc"))) {
		throw new Error("Next.js Babel fallback configuration is missing");
	}
	try {
		readManifest();
		return;
	} catch (e) {
		// Build manifest missing — run build
	}
	process.env.NEXT_TELEMETRY_DISABLED = "1";
	var stdoutWrite = process.stdout.write;
	var stderrWrite = process.stderr.write;
	var processExit = process.exit;
	var capturedOutput = [];
	var capturedBytes = 0;
	var maxCapturedBytes = 1024 * 1024;
	function captureOutput(chunk) {
		var text = Buffer.isBuffer(chunk) ? chunk.toString("utf8") : String(chunk);
		var remainingBytes = maxCapturedBytes - capturedBytes;
		if (remainingBytes > 0) {
			var captured = Buffer.from(text).subarray(0, remainingBytes).toString();
			capturedOutput.push(captured);
			capturedBytes += Buffer.byteLength(captured);
		}
		return true;
	}
	process.stdout.write = captureOutput;
	process.stderr.write = captureOutput;
	process.exit = function (code) {
		var error = new Error(
			"Next.js build called process.exit(" + String(code ?? 0) + ")",
		);
		error.code = "NEXT_BUILD_PROCESS_EXIT";
		error.exitCode = code ?? 0;
		throw error;
	};
	var buildError;
	try {
		await require("../run-next-build.cjs")();
	} catch (error) {
		buildError = error;
	} finally {
		process.stdout.write = stdoutWrite;
		process.stderr.write = stderrWrite;
		process.exit = processExit;
	}
	if (buildError) {
		var output = capturedOutput.join("");
		if (capturedBytes === maxCapturedBytes) {
			output += "\n[Next.js build output truncated at 1048576 bytes]\n";
		}
		if (output) {
			stderrWrite.call(process.stderr, output);
		}
		throw buildError;
	}
}

async function main() {
	await ensureBuild();

	var manifest = readManifest();
	var pages = Object.keys(manifest.pages).sort();

	var results = [];

	results.push({ check: "build-manifest", pages: pages });

	var pagesManifest = JSON.parse(fs.readFileSync(pagesManifestPath, "utf8"));
	results.push({
		check: "pages-manifest",
		hasIndex: pagesManifest["/"] === "pages/index.js",
		hasApiRoute: pagesManifest["/api/hello"] === "pages/api/hello.js",
	});

	var indexModule = fs.readFileSync(
		path.join(projectDir, ".next", "server", "pages", "index.js"),
		"utf8",
	);
	results.push({
		check: "compiled-page",
		rendered: indexModule.indexOf("Hello from Next.js") !== -1,
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

main().catch(function (error) {
	console.error(error);
	process.exitCode = 1;
});
