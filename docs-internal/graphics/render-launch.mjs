// agentOS Apps — launch/social hero (2048×1024, site paper background).
//
//   • "agentOS Apps" lockup: agentOS wordmark + "Apps" (Manrope Semibold, ink)
//   • V8 + SQLite + Rivet Actors, each in its own rounded "app tile" (the
//     landing page FLOATING_AGENTS treatment), scattered with V8 dominant
//   • subtle letter-spaced "PREVIEW" label, top center
//
// Reproducible from a fresh checkout: fonts come from website/, logos + the
// Helvetica-clone OS font are vendored in ./assets, playwright is resolved from
// the repo's node_modules, and it drives system Chromium.
//
//   node docs-internal/launch-graphics/render-launch.mjs
import { createRequire } from "node:module";
import { mkdir, readFile, writeFile, readdir } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(SCRIPT_DIR, "../..");
const CHROMIUM = "/usr/bin/chromium";
// Rendered images are NOT committed — they land in ~/tmp (override with arg 1).
const OUT_DIR = process.argv[2] || path.join(os.homedir(), "tmp/agentos-graphics");

// Resolve playwright from the repo's pnpm store (version-agnostic).
async function loadChromium() {
	const pnpm = path.join(REPO, "node_modules/.pnpm");
	const dir = (await readdir(pnpm)).find((d) => /^playwright@/.test(d));
	if (!dir) throw new Error("playwright not found under node_modules/.pnpm — run `pnpm install`");
	return require(path.join(pnpm, dir, "node_modules/playwright")).chromium;
}

const WIDTH = 2048;
const HEIGHT = 1024;

const dataUrl = (mime, bytes) => `data:${mime};base64,${bytes.toString("base64")}`;
const clean = (s) => s.replace(/<\?xml[^>]*\?>/i, "").replace(/<!DOCTYPE[^>]*>/i, "");
// Prefix ids/refs so multiple inline SVGs don't collide (e.g. `_clip1`).
const nsIds = (svg, p) =>
	svg
		.replace(/id="([^"]+)"/g, `id="${p}$1"`)
		.replace(/url\(#([^)]+)\)/g, `url(#${p}$1)`)
		.replace(/(xlink:href|href)="#([^"]+)"/g, `$1="#${p}$2"`);

const A = (...p) => path.join(SCRIPT_DIR, "assets", ...p);
const [sansFont, osFont, rawWordmark, v8Raw, sqliteRaw, actorsRaw] = await Promise.all([
	readFile(path.join(REPO, "website/public/fonts/manrope/Manrope-Variable-latin.woff2")),
	readFile(A("NimbusSans-Bold.otf")), // URW Helvetica clone → the wordmark's live "OS" text
	readFile(path.join(REPO, "website/public/images/agent-os/agentos-hero-logo.svg"), "utf8"),
	readFile(A("v8.svg"), "utf8"),
	readFile(A("sqlite.svg"), "utf8"),
	readFile(A("actors.svg"), "utf8"),
]);

const wordmark = nsIds(
	clean(rawWordmark).replace(/font-family:'Helvetica-Bold', 'Helvetica'/g, "font-family:'AgentOS OS'"),
	"wm-",
);
const ART = {
	v8: nsIds(clean(v8Raw), "v8-"),
	sqlite: nsIds(clean(sqliteRaw), "sq-"),
	// Actors ships white-stroked for dark UIs; recolor to the site ink.
	actors: nsIds(clean(actorsRaw).replace(/white/g, "#1B1916").replace(/#fff(fff)?\b/gi, "#1B1916"), "ac-"),
};

// Site palette (tailwind.config / global.css)
const INK = "#1B1916";
const INK_SOFT = "#56524A";
const PAPER = "#EFEFEF";

// F5 tile arrangement — V8 dominant in the middle, gently scattered + rotated.
// Content scaled to ~0.95 to leave headroom for the PREVIEW pill.
const S = 0.95;
const TILES = [
	{ k: "sqlite", left: 24, top: 58, size: 222, rot: 7, inner: 0.56 },
	{ k: "v8", left: 52, top: 42, size: 286, rot: -6, inner: 0.60 },
	{ k: "actors", left: 79, top: 56, size: 240, rot: 5, inner: 0.60 },
];

const tileHtml = (t) => {
	const size = Math.round(t.size * S);
	const inner = Math.round(size * t.inner);
	const radius = Math.round(size * 0.2); // ~rounded-2xl proportion
	return `<div class="tile" style="left:${t.left}%;top:${t.top}%;width:${size}px;height:${size}px;
		border-radius:${radius}px;transform:translate(-50%,-50%) rotate(${t.rot}deg)">
		<div class="tlogo" style="height:${inner}px">${ART[t.k]}</div></div>`;
};

const html = `<!doctype html><html lang="en"><head><meta charset="utf-8" /><style>
	@font-face { font-family:"Manrope"; src:url("${dataUrl("font/woff2", sansFont)}") format("woff2"); font-weight:200 800; }
	@font-face { font-family:"AgentOS OS"; src:url("${dataUrl("font/otf", osFont)}") format("opentype"); font-weight:700; }
	* { box-sizing:border-box; }
	html, body { margin:0; background:${PAPER}; }
	.card { position:relative; width:${WIDTH}px; height:${HEIGHT}px; background:${PAPER};
		display:flex; flex-direction:column; align-items:center; justify-content:center; gap:${Math.round(64 * S)}px;
		padding-top:64px; /* bias content down a touch for the pill */ }

	/* subtle letter-spaced PREVIEW label, top center (no pill) */
	.preview { position:absolute; top:68px; left:50%; transform:translateX(-50%); }
	.preview span { font-family:"Manrope",sans-serif; font-weight:700; font-size:40px;
		letter-spacing:0.42em; text-indent:0.42em; color:${INK_SOFT}; }

	.lockup { display:flex; align-items:center; gap:${Math.round(70 * S)}px; }
	.wordmark { height:${Math.round(280 * S)}px; display:flex; align-items:center; }
	.wordmark svg { display:block; height:${Math.round(280 * S)}px; width:auto; }
	.product { color:${INK}; font-family:"Manrope",sans-serif; font-size:${Math.round(158 * S)}px;
		font-weight:600; line-height:${Math.round(280 * S)}px; letter-spacing:-0.015em; }

	.scatter { position:relative; width:1340px; height:${Math.round(360 * S)}px; }
	.tile { position:absolute; display:flex; align-items:center; justify-content:center;
		background:linear-gradient(to bottom, #ffffff, #f1f1f3);
		border:1px solid rgba(27,25,22,0.10); transform-origin:center;
		box-shadow:0 4px 12px -2px rgba(20,20,22,0.10), 0 34px 70px -26px rgba(20,20,22,0.26); }
	.tlogo { display:flex; align-items:center; justify-content:center; }
	.tlogo svg { display:block; height:100%; width:auto; }
</style></head><body>
	<main class="card">
		<div class="preview"><span>PREVIEW</span></div>
		<div class="lockup">
			<div class="wordmark" aria-label="agentOS">${wordmark}</div>
			<div class="product">Apps</div>
		</div>
		<div class="scatter">${TILES.map(tileHtml).join("")}</div>
	</main>
</body></html>`;

await mkdir(OUT_DIR, { recursive: true });
await writeFile(path.join(OUT_DIR, "launch.html"), html);

const chromium = await loadChromium();
const browser = await chromium.launch({ headless: true, executablePath: CHROMIUM });
try {
	const page = await browser.newPage({ viewport: { width: WIDTH, height: HEIGHT }, deviceScaleFactor: 1 });
	await page.setContent(html, { waitUntil: "load" });
	await page.evaluate(() => document.fonts.ready);
	// Optically center "Apps" on the wordmark's OS square badge.
	await page.evaluate(() => {
		const apps = document.querySelector(".product");
		const frame = document.querySelector(".wordmark #wm-frame-layer").getBoundingClientRect();
		const ar = apps.getBoundingClientRect();
		const fs = parseFloat(getComputedStyle(apps).fontSize);
		apps.style.transform = `translateY(${(frame.top + frame.bottom) / 2 - (ar.top + ar.bottom) / 2 - fs * 0.06}px)`;
	});
	await page.screenshot({ path: path.join(OUT_DIR, "launch.png"), fullPage: false });
	await page.close();
} finally {
	await browser.close();
}
console.log(`wrote ${path.join(OUT_DIR, "launch.png")}`);
