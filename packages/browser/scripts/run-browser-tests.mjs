import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const packageDir = path.resolve(__dirname, "..");
const cacheDir = path.join(packageDir, ".cache", "playwright-system-libs");
const browsersCacheDir = path.join(packageDir, ".cache", "ms-playwright");
const debDir = path.join(cacheDir, "deb");
const extractedDir = path.join(cacheDir, "root");
const libraryDirs = [
	path.join(extractedDir, "usr", "lib", "x86_64-linux-gnu"),
	path.join(extractedDir, "lib", "x86_64-linux-gnu"),
];

const linuxRuntimePackages = [
	{ debPrefix: "libatk1.0-0t64_", specs: ["libatk1.0-0t64"] },
	{ debPrefix: "libatk-bridge2.0-0t64_", specs: ["libatk-bridge2.0-0t64"] },
	{ debPrefix: "libatspi2.0-0t64_", specs: ["libatspi2.0-0t64"] },
	{ debPrefix: "libxcomposite1_", specs: ["libxcomposite1"] },
	{ debPrefix: "libxdamage1_", specs: ["libxdamage1"] },
	{ debPrefix: "libxfixes3_", specs: ["libxfixes3"] },
	{ debPrefix: "libxrandr2_", specs: ["libxrandr2"] },
	{ debPrefix: "libxkbcommon0_", specs: ["libxkbcommon0"] },
	{
		debPrefix: "libasound2t64_",
		specs: ["libasound2t64", "libasound2t64=1.2.11-1build2"],
	},
	{
		debPrefix: "libgbm1_",
		specs: ["libgbm1", "libgbm1=24.0.5-1ubuntu1"],
	},
	{
		debPrefix: "libdrm2_",
		specs: ["libdrm2", "libdrm2=2.4.120-2build1"],
	},
	{ debPrefix: "libwayland-server0_", specs: ["libwayland-server0"] },
	{ debPrefix: "libxcb-randr0_", specs: ["libxcb-randr0"] },
	{ debPrefix: "libxi6_", specs: ["libxi6"] },
];

const requiredLibraries = [
	"libatk-1.0.so.0",
	"libatk-bridge-2.0.so.0",
	"libatspi.so.0",
	"libXcomposite.so.1",
	"libXdamage.so.1",
	"libXfixes.so.3",
	"libXrandr.so.2",
	"libasound.so.2",
	"libgbm.so.1",
	"libxkbcommon.so.0",
	"libdrm.so.2",
	"libwayland-server.so.0",
	"libxcb-randr.so.0",
	"libXi.so.6",
];

function run(command, args, options = {}) {
	const result = spawnSync(command, args, {
		cwd: options.cwd ?? packageDir,
		env: options.env ?? process.env,
		stdio: options.stdio ?? "inherit",
		encoding: "utf8",
	});
	if (result.status !== 0) {
		const commandLine = [command, ...args].join(" ");
		const error = new Error(`Command failed (${result.status}): ${commandLine}`);
		error.stdout = result.stdout ?? "";
		error.stderr = result.stderr ?? "";
		throw error;
	}
	return result;
}

function tryRun(command, args, options = {}) {
	return spawnSync(command, args, {
		cwd: options.cwd ?? packageDir,
		env: options.env ?? process.env,
		stdio: options.stdio ?? "pipe",
		encoding: "utf8",
	});
}

function libraryPresent(name) {
	return libraryDirs.some((dir) => existsSync(path.join(dir, name)));
}

function headlessShellPresent() {
	if (!existsSync(browsersCacheDir)) {
		return false;
	}

	for (const entry of readdirSync(browsersCacheDir)) {
		if (!entry.startsWith("chromium_headless_shell-")) {
			continue;
		}

		const executablePath = path.join(
			browsersCacheDir,
			entry,
			"chrome-headless-shell-linux64",
			"chrome-headless-shell",
		);
		if (existsSync(executablePath)) {
			return true;
		}
	}

	return false;
}

function ensurePlaywrightBrowser(env) {
	if (headlessShellPresent()) {
		return;
	}

	mkdirSync(browsersCacheDir, { recursive: true });
	run("pnpm", ["exec", "playwright", "install", "--only-shell", "chromium"], {
		env,
	});
}

function ensureBrowserRuntimeLibraries() {
	if (process.platform !== "linux") {
		return [];
	}
	if (requiredLibraries.every(libraryPresent)) {
		return libraryDirs.filter(existsSync);
	}

	mkdirSync(debDir, { recursive: true });
	rmSync(extractedDir, { recursive: true, force: true });
	mkdirSync(extractedDir, { recursive: true });

	for (const pkg of linuxRuntimePackages) {
		let debFile = readdirSync(debDir).find((entry) => entry.startsWith(pkg.debPrefix) && entry.endsWith(".deb"));
		if (!debFile) {
			let lastFailure = null;
			for (const spec of pkg.specs) {
				const result = tryRun("apt-get", ["download", spec], { cwd: debDir });
				if (result.status === 0) {
					debFile = readdirSync(debDir).find(
						(entry) => entry.startsWith(pkg.debPrefix) && entry.endsWith(".deb"),
					);
					lastFailure = null;
					break;
				}
				lastFailure = result;
			}
			if (!debFile) {
				const failureText = lastFailure
					? [lastFailure.stdout, lastFailure.stderr].filter(Boolean).join("\n")
					: "unknown apt-get failure";
				throw new Error(`Unable to download ${pkg.debPrefix}: ${failureText}`.trim());
			}
		}
		run("dpkg-deb", ["-x", path.join(debDir, debFile), extractedDir], { stdio: "inherit" });
	}

	const missing = requiredLibraries.filter((library) => !libraryPresent(library));
	if (missing.length > 0) {
		throw new Error(`Missing extracted browser runtime libraries: ${missing.join(", ")}`);
	}

	return libraryDirs.filter(existsSync);
}

function main() {
	run("pnpm", ["--dir", "../playground", "run", "setup-vendor"]);
	run("pnpm", ["--dir", "../playground", "build:assets"]);

	const extraLibraryDirs = ensureBrowserRuntimeLibraries();
	const env = {
		...process.env,
		PLAYWRIGHT_BROWSERS_PATH: browsersCacheDir,
		LD_LIBRARY_PATH: [...extraLibraryDirs, process.env.LD_LIBRARY_PATH]
			.filter(Boolean)
			.join(":"),
	};

	ensurePlaywrightBrowser(env);
	run("pnpm", ["exec", "playwright", "test", "--project=chromium", "--workers=1"], {
		env,
	});
}

main();
