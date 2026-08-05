import { defineConfig, devices } from "@playwright/test";

// Dedicated config for the converged agentos wasm sidecar browser tests
// (tests/browser-wasm/*), served by the minimal static server in
// tests/browser-wasm/serve.mjs. Separate from the playground-based runtime-driver
// tests so it can boot the wasm-web package directly.
const PORT = Number(process.env.AGENTOS_WASM_TEST_PORT ?? 43175);
const CHROME_EXECUTABLE_PATH = process.env.AGENTOS_CHROME_EXECUTABLE_PATH;
const CHROME_CHANNEL = process.env.AGENTOS_CHROME_CHANNEL;
const CHROME_ARGS = (process.env.AGENTOS_CHROME_ARGS ?? "")
	.split(/\s+/)
	.map((arg) => arg.trim())
	.filter(Boolean);
const MODEL_DOWNLOAD_DEFAULT_IGNORES = [
	"--disable-background-networking",
	"--disable-component-update",
	"--disable-features=AvoidUnnecessaryBeforeUnloadCheckSync,BoundaryEventDispatchTracksNodeRemoval,DestroyProfileOnBrowserClose,DialMediaRouteProvider,GlobalMediaControls,HttpsUpgrades,LensOverlay,MediaRouter,PaintHolding,ThirdPartyStoragePartitioning,Translate,AutoDeElevate,RenderDocument,OptimizationHints",
];

function parseIgnoreDefaultArgs(value: string | undefined): true | string[] {
	const entries =
		value?.trim().startsWith("[")
			? (JSON.parse(value) as string[])
			: (value ?? "")
					.split(/\n/)
					.map((arg) => arg.trim())
					.filter(Boolean);
	if (process.env.AGENTOS_CHROME_ALLOW_MODEL_DOWNLOAD === "1") {
		entries.push(...MODEL_DOWNLOAD_DEFAULT_IGNORES);
	}
	return process.env.AGENTOS_CHROME_IGNORE_DEFAULT_ARGS === "1"
		? true
		: Array.from(new Set(entries));
}

const CHROME_IGNORE_DEFAULT_ARGS = parseIgnoreDefaultArgs(
	process.env.AGENTOS_CHROME_IGNORE_DEFAULT_ARGS,
);
const HEADLESS =
	process.env.AGENTOS_CHROME_HEADLESS === "0" ? false : undefined;

const chromeUse = {
	...devices["Desktop Chrome"],
	...(CHROME_CHANNEL ? { channel: CHROME_CHANNEL } : {}),
	...(HEADLESS !== undefined ? { headless: HEADLESS } : {}),
	...(CHROME_EXECUTABLE_PATH || CHROME_ARGS.length > 0
		? {
				launchOptions: {
					...(CHROME_EXECUTABLE_PATH
						? { executablePath: CHROME_EXECUTABLE_PATH }
						: {}),
					...(CHROME_ARGS.length > 0 ? { args: CHROME_ARGS } : {}),
					...(CHROME_IGNORE_DEFAULT_ARGS === true ||
					CHROME_IGNORE_DEFAULT_ARGS.length > 0
						? { ignoreDefaultArgs: CHROME_IGNORE_DEFAULT_ARGS }
						: {}),
				},
			}
		: {}),
};

export default defineConfig({
	testDir: "./tests/browser-wasm",
	timeout: 30_000,
	use: {
		baseURL: `http://localhost:${PORT}`,
		trace: "retain-on-failure",
	},
	webServer: {
		// Build the wasm-web package + ACP codec bundle, then serve them. Makes the
		// Chromium test self-contained (no hand-run esbuild/wasm-pack step).
		command: `node ./scripts/build-wasm-test-assets.mjs && PORT=${PORT} node ./tests/browser-wasm/serve.mjs`,
		port: PORT,
		reuseExistingServer: false,
		timeout: 180_000,
	},
	projects: [{ name: "chromium", use: chromeUse }],
});
