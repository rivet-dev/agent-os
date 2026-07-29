import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { resolve } from "node:path";
import test from "node:test";

const packageDir = resolve(import.meta.dirname, "..");
const manifest = JSON.parse(readFileSync(resolve(packageDir, "dist/opencode-acp.manifest.json"), "utf8"));

test("bundles a byte-verified upstream tree with an explicit Node compatibility boundary", () => {
	assert.equal(manifest.sourceVersion, "1.17.20");
	assert.equal(manifest.sourceCommit, "4473fc3c9055046183990a965d68df3db7ea6f62");
	assert.equal(manifest.entrypoint, "packages/opencode/src/cli/cmd/acp.ts");
	assert.equal(manifest.bundleSplitting, true);
	assert.equal(manifest.semanticSourceModifications, true);
	assert.equal(manifest.sourceTreeModified, false);
	assert.equal(manifest.sourceTreeSha256Before, manifest.sourceTreeSha256After);
	assert.equal(existsSync(resolve(packageDir, "upstream/opencode-v1.3.13.patch")), false);
	assert.deepEqual(manifest.nodeCompatibilitySubstitutions, [
		{
			define: "Bun.hash=globalThis.__agentOSOpenCodeHashFast",
			upstreamExpression: "Bun.hash(base).toString(16)",
			nodeSemantics: "Hash.fast(base)",
			reason: "Remote skill cache naming is the remaining Bun-only expression in the native ACP dependency graph",
			removalBlocker: "Remove after a released OpenCode source replaces Bun.hash(base) with Hash.fast(base)",
		},
	]);
	assert.deepEqual(manifest.acpCompatibilitySubstitutions, [
		{
			upstreamBehavior: "Issue the event stream and all initial control-plane requests concurrently over the same loopback origin",
			nodeSemantics: "Keep the event stream independent and serialize short control-plane requests over the real HTTP transport",
			reason: "Embedded HTTP transports do not reliably drain a burst of same-process loopback connections",
			removalBlocker: "Remove after the AgentOS HTTP bridge reliably supports concurrent same-process loopback requests",
		},
		{
			upstreamBehavior: "Pre-apply approved edits through ACP fs/write_text_file before replying to OpenCode",
			nodeSemantics: "Let OpenCode's approved edit tool write directly inside the guest",
			reason: "The redundant ACP host write can re-enter and deadlock synchronous actor transports",
			removalBlocker: "Remove after OpenCode gates the pre-write on the advertised fs.writeTextFile capability or removes it",
		},
		{
			upstreamBehavior: "Rely exclusively on the asynchronous global event stream for completed prompt content and tool parts",
			nodeSemantics: "Reconcile the completed turn from authoritative session messages and suppress late duplicate text deltas",
			reason: "Fast providers can complete before message metadata is queryable, causing the global event bridge to drop text or tool updates",
			removalBlocker: "Remove after OpenCode orders part metadata before deltas or reconciles completed prompt turns upstream",
		},
		{
			upstreamBehavior: "Collapse SDK failures from session/load into an opaque OpenCode service failure",
			nodeSemantics: "Return the original SDK error name/message and distinguish session metadata from message-history loading",
			reason: "AgentOS callers need the underlying failure stage and cause to diagnose persisted-session resume failures",
			removalBlocker: "Remove after OpenCode preserves actionable SDK failure details in its ACP error response or stderr",
		},
		{
			upstreamBehavior: "Expose effort choices only for the currently selected model",
			nodeSemantics: "Also expose variant-qualified model choices so one ACP session describes every model's native reasoning levels",
			reason: "AgentOS model discovery must build an accurate per-model OpenCode-compatible catalog without opening one session per model",
			removalBlocker: "Remove after ACP provides a model metadata operation with per-model configuration options",
		},
	]);
	const bundle = Object.keys(manifest.outputs)
		.filter((relativePath) => relativePath.endsWith(".js"))
		.map((relativePath) =>
			readFileSync(resolve(packageDir, "dist/opencode-acp", relativePath), "utf8"),
		)
		.join("\n");
	assert.match(bundle, /globalThis\.__agentOSOpenCodeHashFast\([^)]*\)\.toString\(16\)/);
	assert.doesNotMatch(bundle, /__agentOSBunHash/);
	assert.doesNotMatch(bundle, /permission3\.permission === "edit"/);
	assert.match(bundle, /completedContent/);
	assert.match(bundle, /completedPromptMessages\(completedMessages\d*, response\d*\)/);
	assert.match(bundle, /\[opencode-acp\] service request failed/);
	assert.match(bundle, /OpenCode .* failed:/);
	assert.match(bundle, /errorName/);
	assert.match(bundle, /session\.messages/);
	assert.match(bundle, /includeModelVariants: true/);
	assert.match(
		bundle,
		/acp\.directory\.provider\.list[\s\S]*?await [\s\S]*?acp\.directory\.mode\.defaultAgent\.load[\s\S]*?await [\s\S]*?acp\.directory\.command\.list/,
	);

	for (const [relativePath, expected] of Object.entries(manifest.outputs)) {
		const path = resolve(packageDir, "dist/opencode-acp", relativePath);
		assert.ok(statSync(path).size > 0, `${relativePath} should be non-empty`);
		assert.equal(createHash("sha256").update(readFileSync(path)).digest("hex"), expected);
	}
});
