// Generates src/generated/registry.json from the monorepo's software/ catalog.
//
// A package is listed iff its agentos-package.json has a `registry` block with
// both `title` and `description` — no fallbacks. Everything else is derived:
// slug from the directory name (overridable via `registry.slug`), type from
// the manifest's `agent` descriptor, npm package name from package.json, and
// for agents the agent id from the manifest `name` plus docs status when
// `registry.docsHref` is set. `featured` is deliberately not part of the
// block — the website hardcodes featured slugs in src/data/registry.ts.
//
// The output is committed. When software/ is not present (e.g. the website
// Docker build, whose context is website/ only), the committed file is used
// as-is.
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const websiteDir = dirname(dirname(fileURLToPath(import.meta.url)));
const softwareRoot = join(websiteDir, "..", "software");
const outPath = join(websiteDir, "src", "generated", "registry.json");

if (!existsSync(softwareRoot)) {
	if (existsSync(outPath)) {
		console.log("gen-registry: software/ not found, using committed registry.json");
		process.exit(0);
	}
	console.error("gen-registry: software/ not found and no committed registry.json");
	process.exit(1);
}

const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

const entries = [];
for (const dir of readdirSync(softwareRoot, { withFileTypes: true })) {
	if (!dir.isDirectory()) continue;
	const pkgDir = join(softwareRoot, dir.name);
	const manifestPath = join(pkgDir, "agentos-package.json");
	if (!existsSync(manifestPath)) continue;
	const manifest = readJson(manifestPath);
	const meta = manifest.registry;
	if (!meta?.title || !meta?.description) continue;

	const type = manifest.agent ? "agent" : "software";
	const pkg = readJson(join(pkgDir, "package.json"));
	const entry = {
		slug: meta.slug ?? dir.name,
		title: meta.title,
		description: meta.description,
		// A package's section defaults to its agent descriptor; `types` overrides it
		// (e.g. browserbase is a software package listed under Browsers).
		types: meta.types ?? [type],
		category: meta.category,
		priority: meta.priority ?? 0,
		package: pkg.name,
		status: meta.docsHref ? "docs" : "available",
	};
	if (meta.beta) entry.beta = true;
	if (meta.icon) entry.icon = meta.icon;
	if (meta.image) entry.image = meta.image;
	if (type === "agent") {
		// Every agent has a docs page; link it even for plain "available"
		// entries (which keep their npm install rendering). Agents whose
		// manifest carries no runtime `name` (e.g. codex, which only has a
		// registry block) fall back to the directory name as agent id.
		const agentId = manifest.name ?? dir.name;
		entry.docsHref = meta.docsHref ?? `/docs/agents/${agentId}`;
		entry.agentId = agentId;
	} else if (meta.docsHref) {
		entry.docsHref = meta.docsHref;
	}
	entries.push(entry);
}

const seen = new Set();
for (const entry of entries) {
	if (seen.has(entry.slug)) {
		console.error(`gen-registry: duplicate slug "${entry.slug}"`);
		process.exit(1);
	}
	seen.add(entry.slug);
	if (entry.image) {
		const imagePath = join(websiteDir, "public", entry.image);
		if (!existsSync(imagePath)) {
			console.error(`gen-registry: ${entry.slug} references missing image ${entry.image}`);
			process.exit(1);
		}
	}
}

entries.sort(
	(a, b) =>
		a.types[0].localeCompare(b.types[0]) ||
		b.priority - a.priority ||
		a.title.localeCompare(b.title),
);

writeFileSync(outPath, JSON.stringify({ entries }, null, "\t") + "\n");
console.log(`gen-registry: wrote ${entries.length} entries`);
