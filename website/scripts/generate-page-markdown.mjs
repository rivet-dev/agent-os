import {
	copyFileSync,
	existsSync,
	mkdirSync,
	readFileSync,
	readdirSync,
	writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const websiteRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(websiteRoot, "..");
const distDir = join(websiteRoot, "dist");

function parseFrontmatter(raw) {
	const match = raw.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
	if (!match) return { data: {}, body: raw };
	const data = {};
	for (const line of match[1].split(/\r?\n/)) {
		const value = line.match(/^(\w+):\s*(.*)$/);
		if (value) data[value[1]] = value[2].trim().replace(/^["']|["']$/g, "");
	}
	return { data, body: match[2] };
}

function cookbookMarkdown(slug) {
	const raw = readFileSync(join(repoRoot, "examples", slug, "README.md"), "utf8");
	const { data, body } = parseFrontmatter(raw);
	const title = data.title || slug;
	const description = data.description ? `\n${data.description}\n` : "";
	const withoutSource = body.replace(/\n#{1,6}\s+Source\b[\s\S]*$/i, "\n").trimEnd();
	return `# ${title}\n${description}\n${withoutSource}\n\n## Source\n\n[View source on GitHub](https://github.com/rivet-dev/agentos/tree/main/examples/${slug})\n`;
}

function visit(directory) {
	for (const entry of readdirSync(directory, { withFileTypes: true })) {
		const path = join(directory, entry.name);
		if (entry.isDirectory()) {
			visit(path);
			continue;
		}
		if (entry.name !== "index.html") continue;
		const html = readFileSync(path, "utf8");
		if (!html.includes('component-export="DocsPageDropdown"')) continue;

		const route = relative(distDir, dirname(path)).replace(/\\/g, "/");
		const output = join(distDir, `${route}.md`);
		mkdirSync(dirname(output), { recursive: true });
		if (route.startsWith("docs/")) {
			const source = join(websiteRoot, "public", "docs", `${route}.md`);
			if (!existsSync(source)) throw new Error(`missing generated Markdown source: ${source}`);
			copyFileSync(source, output);
		} else if (route.startsWith("cookbooks/")) {
			writeFileSync(output, cookbookMarkdown(route.slice("cookbooks/".length)));
		}
	}
}

visit(distDir);
