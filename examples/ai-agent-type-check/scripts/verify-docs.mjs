import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");
const docsPath = path.join(repoRoot, "docs/features/typescript.mdx");

const expectedFiles = new Map([
  ["Type-Checked Execution", path.join(repoRoot, "examples/ai-agent-type-check/src/index.ts")],
]);

function normalizeTitle(title) {
  return title.trim().replace(/^"|"$/g, "");
}

function normalizeCode(source) {
  const normalized = source.replace(/\r\n/g, "\n").replace(/^\n+|\n+$/g, "");
  const lines = normalized.split("\n");
  const nonEmptyLines = lines.filter((line) => line.trim().length > 0);
  const minIndent = nonEmptyLines.reduce((indent, line) => {
    const lineIndent = line.match(/^ */)?.[0].length ?? 0;
    return Math.min(indent, lineIndent);
  }, Number.POSITIVE_INFINITY);

  if (!Number.isFinite(minIndent) || minIndent === 0) {
    return normalized;
  }

  return lines.map((line) => line.slice(minIndent)).join("\n");
}

const docsSource = await readFile(docsPath, "utf8");
const blockPattern = /^\s*```ts(?:\s+([^\n]+))?\n([\s\S]*?)^\s*```/gm;
const docBlocks = new Map();

for (const match of docsSource.matchAll(blockPattern)) {
  const rawTitle = match[1];
  if (!rawTitle) {
    continue;
  }

  const title = normalizeTitle(rawTitle);
  if (!expectedFiles.has(title)) {
    continue;
  }

  docBlocks.set(title, normalizeCode(match[2] ?? ""));
}

const mismatches = [];

for (const [title, filePath] of expectedFiles) {
  const fileSource = normalizeCode(await readFile(filePath, "utf8"));
  const docSource = docBlocks.get(title);

  if (!docSource) {
    mismatches.push(`Missing docs snippet for ${title}`);
    continue;
  }

  if (docSource !== fileSource) {
    mismatches.push(`Snippet mismatch for ${title}`);
  }
}

if (mismatches.length > 0) {
  console.error(mismatches.join("\n"));
  process.exit(1);
}

console.log("AI agent docs match example sources.");
