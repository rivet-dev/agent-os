// agentOS Apps — technical code-snippet image (white, 2048px wide, auto height).
// A titled "agentOS Apps" header + N syntax-highlighted steps, styled to match
// the Rivet launch technical-image treatment (JetBrains Mono, Ayu Dark @ #0a0a0a,
// 132px gutters). Steps come from ./snippets.json.
//
//   node docs-internal/graphics/render-technical.mjs
//
// Requires `shiki` (a dev-only highlighter, NOT a repo dependency). Resolved
// from the repo's node_modules, else set SHIKI_DIR=/path/to/shiki, else
// `pnpm add -D shiki`.
import { createRequire } from "node:module";
import { mkdir, readFile, writeFile, readdir } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(SCRIPT_DIR, "../..");
const CHROMIUM = "/usr/bin/chromium";
const OUT_DIR = process.argv[2] || path.join(os.homedir(), "tmp/agentos-graphics");
const WIDTH = 2048;

async function findPkg(name) {
	const stores = [path.join(REPO, "node_modules/.pnpm"), process.env.SHIKI_DIR && path.dirname(path.dirname(process.env.SHIKI_DIR))].filter(Boolean);
	for (const pnpm of stores) {
		try {
			const dir = (await readdir(pnpm)).find((d) => new RegExp(`^${name}@`).test(d));
			if (dir) return path.join(pnpm, dir, "node_modules", name);
		} catch {}
	}
	if (name === "shiki" && process.env.SHIKI_DIR) return process.env.SHIKI_DIR;
	return null;
}

const pwDir = await findPkg("playwright");
if (!pwDir) throw new Error("playwright not found under node_modules/.pnpm — run `pnpm install`");
const { chromium } = require(pwDir);

const shikiDir = await findPkg("shiki");
if (!shikiDir) throw new Error("shiki not found — `pnpm add -D shiki` or set SHIKI_DIR=/path/to/node_modules/shiki");
const { getSingletonHighlighter } = await import(path.join(shikiDir, "dist/index.mjs"));
const { default: ayuDark } = await import(path.join(shikiDir, "dist/themes/ayu-dark.mjs"));

// Ayu Dark with the panel background remapped to #0a0a0a (matches the website theme).
const theme = structuredClone(ayuDark);
theme.name = "ayu-dark-agentos";
for (const k in theme.colors ?? {}) if (theme.colors[k] === "#0b0e14") theme.colors[k] = "#0a0a0a";
for (const tc of theme.tokenColors ?? []) for (const k in tc.settings ?? {}) if (tc.settings[k] === "#0b0e14") tc.settings[k] = "#0a0a0a";

const dataUrl = (mime, bytes) => `data:${mime};base64,${bytes.toString("base64")}`;
const escapeHtml = (v) => v.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

const A = (...p) => path.join(SCRIPT_DIR, "assets", ...p);
const [monoFont, sansFont, osFont, rawLogo, sectionsRaw] = await Promise.all([
	readFile(path.join(REPO, "website/public/fonts/jetbrains-mono/JetBrainsMono-Variable-latin.woff2")),
	readFile(path.join(REPO, "website/public/fonts/manrope/Manrope-Variable-latin.woff2")),
	readFile(A("NimbusSans-Bold.otf")),
	readFile(path.join(REPO, "website/public/images/agent-os/agentos-hero-logo.svg"), "utf8"),
	readFile(path.join(SCRIPT_DIR, "snippets.json"), "utf8"),
]);
const sections = JSON.parse(sectionsRaw);

const logo = rawLogo
	.replace(/<\?xml[^>]*\?>/i, "")
	.replace(/<!DOCTYPE[^>]*>/i, "")
	.replace(/font-family:'Helvetica-Bold', 'Helvetica'/g, "font-family:'AgentOS OS'");

const highlighter = await getSingletonHighlighter({
	langs: [...new Set(sections.map((s) => s.language))],
	themes: [theme],
});
const renderedSections = sections
	.map(({ title, language, code }) => {
		const highlighted = highlighter.codeToHtml(code, { lang: language, theme: theme.name });
		return `<section class="section"><h2>${escapeHtml(title)}</h2>
			<div class="code-block"><div class="code-inner">${highlighted}</div></div></section>`;
	})
	.join("\n");

const html = `<!doctype html><html lang="en"><head><meta charset="utf-8" />
	<meta name="viewport" content="width=${WIDTH}, initial-scale=1" /><style>
	@font-face { font-family:"JetBrains Mono"; src:url("${dataUrl("font/woff2", monoFont)}") format("woff2"); font-weight:100 800; }
	@font-face { font-family:"Manrope"; src:url("${dataUrl("font/woff2", sansFont)}") format("woff2"); font-weight:200 800; }
	@font-face { font-family:"AgentOS OS"; src:url("${dataUrl("font/otf", osFont)}") format("opentype"); font-weight:700; }
	* { box-sizing:border-box; }
	html, body { margin:0; width:${WIDTH}px; height:auto; overflow-x:hidden; background:#fff; }
	.card { position:relative; width:${WIDTH}px; height:auto; padding:84px 132px 132px; background:#fff; }
	.header { display:flex; align-items:center; gap:30px; height:100px; }
	.logo { height:100px; display:flex; align-items:center; }
	.logo svg { display:block; height:100px; width:auto; }
	.product { color:#18181b; font-family:"Manrope",sans-serif; font-size:62px; font-weight:600; line-height:100px; letter-spacing:-0.02em; }
	.sections { display:grid; grid-template-columns:minmax(0,1fr); gap:44px; margin-top:72px; }
	.section { display:flex; min-width:0; flex-direction:column; }
	.section h2 { margin:0 0 16px; color:#18181b; font-family:"Manrope",sans-serif; font-size:30px; font-weight:600; line-height:42px; letter-spacing:-0.01em; }
	.code-block { flex:1; overflow:hidden; border:2px solid rgba(244,241,231,0.1); border-radius:24px; background:#0a0a0a; }
	.code-inner { padding:30px 32px; }
	.shiki { margin:0; padding:0; background:transparent !important; font-family:"JetBrains Mono",monospace; font-size:34px; font-weight:400; line-height:48px; tab-size:4; }
	.shiki code { display:block; }
</style></head><body>
	<main class="card">
		<header class="header">
			<div class="logo" aria-label="agentOS">${logo}</div>
			<div class="product">Apps</div>
		</header>
		<div class="sections">${renderedSections}</div>
	</main>
</body></html>`;

await mkdir(OUT_DIR, { recursive: true });
await writeFile(path.join(OUT_DIR, "technical.html"), html);

const browser = await chromium.launch({ headless: true, executablePath: CHROMIUM });
try {
	const page = await browser.newPage({ viewport: { width: WIDTH, height: 1238 }, deviceScaleFactor: 1 });
	await page.setContent(html, { waitUntil: "load" });
	await page.evaluate(() => document.fonts.ready);
	const height = await page.$eval(".card", (c) => Math.ceil(c.getBoundingClientRect().height));
	await page.setViewportSize({ width: WIDTH, height });
	const overflow = await page.$$eval(".code-block", (blocks) =>
		blocks.map((b, i) => ({ i, o: b.scrollWidth - b.clientWidth })).find(({ o }) => o > 1),
	);
	if (overflow) throw new Error(`Code section ${overflow.i + 1} overflows by ${Math.ceil(overflow.o)}px — shorten it`);
	await page.screenshot({ path: path.join(OUT_DIR, "technical.png"), fullPage: false });
	await page.close();
} finally {
	await browser.close();
}
console.log(`wrote ${path.join(OUT_DIR, "technical.png")}`);
