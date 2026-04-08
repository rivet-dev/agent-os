#!/usr/bin/env node

// Postinstall script: verifies the platform-specific binary is available.
// If the optionalDependency wasn't installed (unsupported platform or
// registry issue), attempts to download the prebuilt binary from GitHub
// releases as a fallback.

"use strict";

const { existsSync } = require("fs");
const { join, dirname } = require("path");

const PLATFORM_PACKAGES = {
	"linux-x64": "@secure-exec/v8-linux-x64-gnu",
	"linux-arm64": "@secure-exec/v8-linux-arm64-gnu",
	"darwin-x64": "@secure-exec/v8-darwin-x64",
	"darwin-arm64": "@secure-exec/v8-darwin-arm64",
	"win32-x64": "@secure-exec/v8-win32-x64",
};

const BINARY_NAME =
	process.platform === "win32" ? "secure-exec-v8.exe" : "secure-exec-v8";

function hasPlatformBinary() {
	const key = `${process.platform}-${process.arch}`;
	const pkg = PLATFORM_PACKAGES[key];
	if (!pkg) return false;

	try {
		const pkgDir = dirname(require.resolve(`${pkg}/package.json`));
		return existsSync(join(pkgDir, BINARY_NAME));
	} catch {
		return false;
	}
}

function hasLocalBinary() {
	// Check crate target paths (development)
	const paths = [
		join(__dirname, "../../native/v8-runtime/target/release/secure-exec-v8"),
		join(__dirname, "../../native/v8-runtime/target/debug/secure-exec-v8"),
	];
	return paths.some((p) => existsSync(p));
}

async function downloadFallback() {
	const { version } = require("./package.json");
	const key = `${process.platform}-${process.arch}`;
	const pkg = PLATFORM_PACKAGES[key];
	if (!pkg) {
		console.warn(
			`@secure-exec/v8: No prebuilt binary available for ${process.platform}-${process.arch}. ` +
				"Build from source: cd native/v8-runtime && cargo build --release",
		);
		return;
	}

	// Extract platform suffix from package name (e.g. "linux-x64-gnu" from "@secure-exec/v8-linux-x64-gnu")
	const suffix = pkg.replace("@secure-exec/v8-", "");
	const url = `https://github.com/rivet-dev/secure-exec/releases/download/v${version}/${BINARY_NAME}-${suffix}`;

	console.log(`@secure-exec/v8: Downloading binary from ${url}...`);

	try {
		const https = require("https");
		const fs = require("fs");
		const destDir = join(__dirname, "bin");
		if (!existsSync(destDir)) fs.mkdirSync(destDir, { recursive: true });
		const dest = join(destDir, BINARY_NAME);

		await new Promise((resolve, reject) => {
			function fetch(fetchUrl, redirects) {
				if (redirects > 5) {
					reject(new Error("Too many redirects"));
					return;
				}
				https
					.get(fetchUrl, (res) => {
						if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
							fetch(res.headers.location, redirects + 1);
							return;
						}
						if (res.statusCode !== 200) {
							reject(
								new Error(`Download failed: HTTP ${res.statusCode}`),
							);
							return;
						}
						const file = fs.createWriteStream(dest, { mode: 0o755 });
						res.pipe(file);
						file.on("finish", () => file.close(resolve));
						file.on("error", reject);
					})
					.on("error", reject);
			}
			fetch(url, 0);
		});

		console.log(`@secure-exec/v8: Binary installed to ${dest}`);
	} catch (err) {
		console.warn(
			`@secure-exec/v8: Failed to download binary: ${err.message}. ` +
				"Build from source: cd native/v8-runtime && cargo build --release",
		);
	}
}

async function main() {
	// Skip in development (local cargo builds available)
	if (hasPlatformBinary() || hasLocalBinary()) {
		return;
	}
	await downloadFallback();
}

main().catch((err) => {
	// Postinstall failures should warn, not break install
	console.warn(`@secure-exec/v8 postinstall: ${err.message}`);
});
