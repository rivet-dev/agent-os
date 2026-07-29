import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { readFile, writeFile } from "node:fs/promises";

const [sourceRoot, outputDirectory, compatibilityModule] = process.argv.slice(2);
if (!sourceRoot || !outputDirectory || !compatibilityModule) {
	throw new Error("usage: build-upstream-node.ts <source-root> <output-directory> <node-pty-module>");
}

const opencodeDirectory = resolve(sourceRoot, "packages", "opencode");
process.chdir(opencodeDirectory);
const generated = await import(pathToFileURL(resolve(opencodeDirectory, "script", "generate.ts")).href);

const proposedEditPattern = /\n\s*if \(permission\.permission === "edit"\) \{\n\s*await this\.writeProposedEdit\(session\.id, permission\.metadata\)\.catch\(\(\) => \{\}\)\n\s*\}\n/;
const configDependencyInstallPattern = /          const dep = yield\* npmSvc[\s\S]*?          deps\.push\(dep\)\n/;
const eventContentStatePattern = /  private readonly toolStarts = new Set<string>\(\)\n/;
const replayContentPattern = /  private async replayContentPart\(message: SessionMessageResponse, part: Part\) \{[\s\S]*?\n  \}\n\n  private async run\(\)/;
const deltaTextPattern = /    if \(metadata\.partType === "text" && props\.field === "text" && metadata\.ignored !== true\) \{\n      await this\.input\.connection\.sessionUpdate\(\{/;
const deltaReasoningPattern = /    if \(metadata\.partType === "reasoning" && props\.field === "text"\) \{\n      await this\.input\.connection\.sessionUpdate\(\{/;
const promptReplayPattern = /        yield\* sendUsageUpdate\(input\.usage, input\.sdk, input\.connection, current\.id, current\.cwd\)\n        return yield\* promptResponse\(response\.info, params\.messageId\)/g;
const replayMessagesPattern = /function replayMessages\(subscription: ACPEvent\.Subscription \| undefined, messages: SessionMessageResponse\[\]\) \{/;
const serviceFailurePattern = /  return new ACPError\.ServiceFailureError\(\{ safeMessage: "OpenCode service failure", service \}\)\n/;
const loadMessagesServicePattern = /    const messages = yield\* request\(\n      \(\) => input\.sdk\.session\.messages\(\{ directory: params\.cwd, sessionID: params\.sessionId \}, \{ throwOnError: true \}\),\n      "session",\n    \)\n/;
const concurrentDirectoryRequestsPattern = /    const \[providersResponse, agentsResponse, commandsResponse, skillsResponse, configResponse\] = await Promise\.all\(\[[\s\S]*?\n    \]\)\n/;
const configOptionsPattern = /  return buildConfigOptions\(\{\n    providers: Object\.values\(snapshot\.providers\),\n    currentModel: session\.model,\n    currentVariant: session\.variant,\n/;

const result = await Bun.build({
	target: "node",
	entrypoints: [resolve(opencodeDirectory, "src", "cli", "cmd", "acp.ts")],
	outdir: outputDirectory,
	format: "esm",
	// Preserve native ESM boundaries. A single async bundle creates cyclic
	// initializer promises that embedded V8 cannot settle reliably.
	splitting: true,
	external: ["jsonc-parser"],
	define: {
		OPENCODE_MODELS_DEV: "undefined",
		OPENCODE_CHANNEL: JSON.stringify("latest"),
		"Bun.hash": "globalThis.__agentOSOpenCodeHashFast",
	},
	files: {
		"opencode-web-ui.gen.ts": "",
	},
	plugins: [
		{
			name: "agentos-opencode-disable-config-dependency-install",
			setup(build) {
				build.onLoad({ filter: /[/\\]src[/\\]config[/\\]config\.ts$/ }, async ({ path }) => {
					const source = await readFile(path, "utf8");
					const matches = source.match(new RegExp(configDependencyInstallPattern.source, "g"));
					if (matches?.length !== 1) {
						throw new Error(
							`Expected exactly one OpenCode config dependency install, found ${matches?.length ?? 0}`,
						);
					}
					const install = matches[0].trimEnd();
					const guarded = [
						'          if (process.env.OPENCODE_DISABLE_CONFIG_DEP_INSTALL !== "1") {',
						...install.split("\n").map((line) => `  ${line}`),
						"          }",
						"",
					].join("\n");
					return {
						contents: source.replace(configDependencyInstallPattern, guarded),
						loader: "ts",
					};
				});
			},
		},
		{
			name: "agentos-opencode-acp-permission-compatibility",
			setup(build) {
				build.onLoad({ filter: /[/\\]src[/\\]acp[/\\]permission\.ts$/ }, async ({ path }) => {
					const source = await readFile(path, "utf8");
					const matches = source.match(new RegExp(proposedEditPattern.source, "g"));
					if (matches?.length !== 1) {
						throw new Error(
							`Expected exactly one OpenCode ACP proposed-edit prewrite, found ${matches?.length ?? 0}`,
						);
					}
					return {
						contents: source.replace(
							proposedEditPattern,
							"\n    // The approved OpenCode edit tool writes inside the guest. Avoid a redundant\n    // ACP host write, which can re-enter and deadlock synchronous actor transports.\n",
						),
						loader: "ts",
					};
				});
			},
		},
		{
			name: "agentos-opencode-acp-completed-message-reconciliation",
			setup(build) {
				build.onLoad({ filter: /[/\\]src[/\\]acp[/\\]event\.ts$/ }, async ({ path }) => {
					let source = await readFile(path, "utf8");
					for (const [label, pattern, expected] of [
						["content state", eventContentStatePattern, 1],
						["content replay", replayContentPattern, 1],
						["text delta", deltaTextPattern, 1],
						["reasoning delta", deltaReasoningPattern, 1],
					] as const) {
						const count = source.match(new RegExp(pattern.source, pattern.flags.includes("g") ? pattern.flags : `${pattern.flags}g`))?.length ?? 0;
						if (count !== expected) throw new Error(`Expected exactly ${expected} OpenCode ACP ${label} site, found ${count}`);
					}

					source = source.replace(
						eventContentStatePattern,
						'  private readonly toolStarts = new Set<string>()\n  private readonly deliveredContent = new Map<string, string>()\n  private readonly completedContent = new Set<string>()\n',
					);
					source = source.replace(
						replayContentPattern,
						`  private async replayContentPart(message: SessionMessageResponse, part: Part) {
    if (part.type !== "text" && part.type !== "file" && part.type !== "reasoning") return

    const sessionUpdate =
      part.type === "reasoning"
        ? "agent_thought_chunk"
        : message.info.role === "user"
          ? "user_message_chunk"
          : "agent_message_chunk"

    const contentKey = \`${"${message.info.sessionID}:${message.info.id}:${part.id}"}\`
    const delivered = this.deliveredContent.get(contentKey) ?? ""
    const replayPart =
      part.type === "text" || part.type === "reasoning"
        ? { ...part, text: part.text.startsWith(delivered) ? part.text.slice(delivered.length) : part.text }
        : part
    if (part.type === "text" || part.type === "reasoning") {
      this.deliveredContent.set(contentKey, part.text)
      this.completedContent.add(contentKey)
    }

    for (const chunk of partsToContentChunks([replayPart as ReplayPart])) {
      await this.input.connection.sessionUpdate({
        sessionId: message.info.sessionID,
        update: {
          sessionUpdate,
          messageId: message.info.id,
          ...chunk,
        },
      })
    }
  }

  private async run()`,
					);
					source = source.replace(
						deltaTextPattern,
						`    if (metadata.partType === "text" && props.field === "text" && metadata.ignored !== true) {
      const contentKey = \`${"${session.id}:${props.messageID}:${props.partID}"}\`
      if (this.completedContent.has(contentKey)) return
      this.deliveredContent.set(contentKey, (this.deliveredContent.get(contentKey) ?? "") + props.delta)
      await this.input.connection.sessionUpdate({`,
					);
					source = source.replace(
						deltaReasoningPattern,
						`    if (metadata.partType === "reasoning" && props.field === "text") {
      const contentKey = \`${"${session.id}:${props.messageID}:${props.partID}"}\`
      if (this.completedContent.has(contentKey)) return
      this.deliveredContent.set(contentKey, (this.deliveredContent.get(contentKey) ?? "") + props.delta)
      await this.input.connection.sessionUpdate({`,
					);
					return { contents: source, loader: "ts" };
				});

				build.onLoad({ filter: /[/\\]src[/\\]acp[/\\]service\.ts$/ }, async ({ path }) => {
					let source = await readFile(path, "utf8");
					const matches = source.match(promptReplayPattern);
					if (matches?.length !== 2) {
						throw new Error(`Expected exactly two OpenCode ACP prompt completion sites, found ${matches?.length ?? 0}`);
					}
					for (const [label, pattern] of [
						["service failure", serviceFailurePattern],
						["session/load messages", loadMessagesServicePattern],
						["directory control-plane concurrency", concurrentDirectoryRequestsPattern],
						["message replay helper", replayMessagesPattern],
						["model variant catalog", configOptionsPattern],
					] as const) {
						const count = source.match(new RegExp(pattern.source, "g"))?.length ?? 0;
						if (count !== 1) throw new Error(`Expected exactly one OpenCode ACP ${label} site, found ${count}`);
					}
					source = source.replace(
						serviceFailurePattern,
						`  const record = typeof error === "object" && error !== null ? error as Record<string, unknown> : undefined
  const data = record && typeof record.data === "object" && record.data !== null
    ? record.data as Record<string, unknown>
    : undefined
  const cause = record && typeof record.cause === "object" && record.cause !== null
    ? record.cause as Record<string, unknown>
    : undefined
  const message = [record?.message, data?.message, cause?.message]
    .find((value): value is string => typeof value === "string" && value.length > 0)
    ?? String(error)
  const errorName = typeof record?.name === "string" ? record.name : undefined
  console.error("[opencode-acp] service request failed", { service, errorName, message, error })
  return new ACPError.ServiceFailureError({
    safeMessage: \`OpenCode \${service ?? "service"} failed: \${message.slice(0, 2000)}\`,
    service,
    errorName,
  })
`,
					);
					source = source.replace(
						loadMessagesServicePattern,
						'    const messages = yield* request(\n      () => input.sdk.session.messages({ directory: params.cwd, sessionID: params.sessionId }, { throwOnError: true }),\n      "session.messages",\n    )\n',
					);
					source = source.replace(
						concurrentDirectoryRequestsPattern,
						`    // The embedded Node HTTP bridge currently cannot drain the upstream
    // five-request same-origin burst reliably. Keep the event stream separate,
    // but serialize these short catalog requests until the bridge supports it.
    const providersResponse = await ACPProfile.measure("acp.directory.provider.list", () =>
      sdk.config.providers({ directory }, { throwOnError: true }),
    )
    const agentsResponse = await ACPProfile.measure("acp.directory.mode.defaultAgent.load", () =>
      sdk.app.agents({ directory }, { throwOnError: true }),
    )
    const commandsResponse = await ACPProfile.measure("acp.directory.command.list", () =>
      sdk.command.list({ directory }, { throwOnError: true }),
    )
    const skillsResponse = await ACPProfile.measure("acp.directory.skill.list", () =>
      sdk.app.skills({ directory }, { throwOnError: true }),
    )
    const configResponse = await ACPProfile.measure("acp.directory.defaultModel.config", () =>
      sdk.config.get({ directory }, { throwOnError: true }).catch(() => undefined),
    )
`,
					);
					source = source.replace(
						configOptionsPattern,
						`  return buildConfigOptions({
    providers: Object.values(snapshot.providers),
    currentModel: session.model,
    currentVariant: session.variant,
    // ACP effort options are otherwise limited to the current model. Include
    // variant-qualified model values so catalog clients can discover every
    // model's native reasoning levels without opening one session per model.
    includeModelVariants: true,
`,
					);
					return {
						contents: source
							.replace(
								promptReplayPattern,
								`        const completedMessages = yield* request(
          () => input.sdk.session.messages(
            { directory: current.cwd, sessionID: current.id, limit: 100 },
            { throwOnError: true },
          ),
          "session.messages",
        )
        yield* replayMessages(events, completedPromptMessages(completedMessages, response))
        yield* sendUsageUpdate(input.usage, input.sdk, input.connection, current.id, current.cwd)
        return yield* promptResponse(response.info, params.messageId)`,
							)
							.replace(
								replayMessagesPattern,
								`function completedPromptMessages(
  messages: SessionMessageResponse[],
  response: SessionMessageResponse,
) {
  const responseIndex = messages.findIndex((message) => message.info.id === response.info.id)
  const parentId = response.info.parentID
  const parentIndex = parentId
    ? messages.findIndex((message) => message.info.id === parentId)
    : -1
  if (responseIndex < 0 || parentIndex < 0 || responseIndex === parentIndex) return [response]
  if (parentIndex < responseIndex) return messages.slice(parentIndex + 1, responseIndex + 1)
  return messages.slice(responseIndex, parentIndex).reverse()
}

function replayMessages(subscription: ACPEvent.Subscription | undefined, messages: SessionMessageResponse[]) {`,
							),
						loader: "ts",
					};
				});
			},
		},
		{
			name: "agentos-node-pty",
			setup(build) {
				build.onResolve({ filter: /^@lydell\/node-pty$/ }, () => ({
					path: resolve(compatibilityModule),
				}));
			},
		},
	],
});

if (!result.success) {
	for (const log of result.logs) console.error(log);
	throw new Error("Bun failed to build the upstream OpenCode ACP entrypoint for Node");
}

await writeFile(resolve(outputDirectory, "models.json"), generated.modelsData);
