#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
	copyFileSync,
	existsSync,
	mkdirSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const SOURCE_REPOSITORY = "anomalyco/opencode";
const SOURCE_VERSION = "1.3.13";
const SOURCE_TARBALL_URL = `https://github.com/${SOURCE_REPOSITORY}/archive/refs/tags/v${SOURCE_VERSION}.tar.gz`;

const __dirname = dirname(fileURLToPath(import.meta.url));
const packageDir = resolve(__dirname, "..");
const distDir = resolve(packageDir, "dist");
const cacheDir = resolve(packageDir, "node_modules", ".cache", "opencode-build");
const patchPath = resolve(packageDir, "upstream", `opencode-v${SOURCE_VERSION}.patch`);
const bundleDir = resolve(distDir, "opencode-acp");
const manifestPath = resolve(distDir, "opencode-acp.manifest.json");
const SQL_JS_VERSION = "1.14.1";
const bunBin = resolve(
	packageDir,
	"node_modules",
	".bin",
	process.platform === "win32" ? "bun.cmd" : "bun",
);

function run(command, args, options = {}) {
	const result = spawnSync(command, args, {
		stdio: "inherit",
		...options,
	});
	if (result.status !== 0) {
		throw new Error(
			`Command failed (${result.status ?? "unknown"}): ${command} ${args.join(" ")}`,
		);
	}
	return result;
}

const PATCHED_SOURCE_FILES = [
	"packages/opencode/src/cli/cmd/acp.ts",
	"packages/opencode/src/config/config.ts",
	"packages/opencode/src/plugin/index.ts",
	"packages/opencode/src/server/instance.ts",
	"packages/opencode/src/server/server.ts",
];

async function ensureNodeAcpPatch(sourceRoot, tarballPath) {
	const serverFile = resolve(
		sourceRoot,
		"packages",
		"opencode",
		"src",
		"server",
		"server.ts",
	);
	const pluginFile = resolve(
		sourceRoot,
		"packages",
		"opencode",
		"src",
		"plugin",
		"index.ts",
	);
	const acpFile = resolve(
		sourceRoot,
		"packages",
		"opencode",
		"src",
		"cli",
		"cmd",
		"acp.ts",
	);
	const serverSource = readFileSync(serverFile, "utf-8");
	const pluginSource = readFileSync(pluginFile, "utf-8");
	const acpSource = readFileSync(acpFile, "utf-8");
	const alreadyPatched =
		serverSource.includes('from "node:http"') &&
		!serverSource.includes('from "hono/bun"') &&
		pluginSource.includes("Bun shell is unavailable in the Node ACP build") &&
		acpSource.includes("const server = await Server.listen(opts)");

	if (alreadyPatched) {
		return;
	}

	const scratchRoot = await mkdtemp(join(tmpdir(), "agentos-opencode-patch-"));
	try {
		run("tar", ["-xzf", tarballPath, "--strip-components=1", "-C", scratchRoot]);
		run("git", ["apply", "--whitespace=nowarn", patchPath], { cwd: scratchRoot });
		for (const relativePath of PATCHED_SOURCE_FILES) {
			copyFileSync(
				resolve(scratchRoot, relativePath),
				resolve(sourceRoot, relativePath),
			);
		}
	} finally {
		rmSync(scratchRoot, { recursive: true, force: true });
	}

	const verifiedServerSource = readFileSync(serverFile, "utf-8");
	const verifiedPluginSource = readFileSync(pluginFile, "utf-8");
	const verifiedAcpSource = readFileSync(acpFile, "utf-8");
	if (
		!verifiedServerSource.includes('from "node:http"') ||
		verifiedServerSource.includes('from "hono/bun"') ||
		!verifiedPluginSource.includes("Bun shell is unavailable in the Node ACP build") ||
		!verifiedAcpSource.includes("const server = await Server.listen(opts)")
	) {
		throw new Error("Failed to stage the Node ACP patches into the prepared OpenCode source tree");
	}
}

async function downloadFile(url, destination) {
	const response = await fetch(url);
	if (!response.ok) {
		throw new Error(`Failed to download ${url}: ${response.status} ${response.statusText}`);
	}

	const buffer = Buffer.from(await response.arrayBuffer());
	await writeFile(destination, buffer);
}

async function readMigrations(sourceRoot) {
	const migrationRoot = join(sourceRoot, "packages", "opencode", "migration");
	const entries = (await readdir(migrationRoot, { withFileTypes: true }))
		.filter((entry) => entry.isDirectory() && /^\d{14}/.test(entry.name))
		.map((entry) => entry.name)
		.sort();

	return Promise.all(
		entries.map(async (name) => {
			const sql = await readFile(
				join(migrationRoot, name, "migration.sql"),
				"utf8",
			);
			const match = /^(\d{4})(\d{2})(\d{2})(\d{2})(\d{2})(\d{2})/.exec(name);
			const timestamp = match
				? Date.UTC(
						Number(match[1]),
						Number(match[2]) - 1,
						Number(match[3]),
						Number(match[4]),
						Number(match[5]),
						Number(match[6]),
					)
				: 0;
			return { name, sql, timestamp };
		}),
	);
}

async function rewriteSourceFile(sourceRoot, relativePath, transform) {
	const filePath = resolve(sourceRoot, relativePath);
	const original = await readFile(filePath, "utf8");
	const next = transform(original);
	if (next !== original) {
		await writeFile(filePath, next);
	}
}

async function ensureSqlJsDependency(sourceRoot) {
	const packageJsonPath = resolve(
		sourceRoot,
		"packages",
		"opencode",
		"package.json",
	);
	const packageJson = JSON.parse(await readFile(packageJsonPath, "utf8"));
	const dependencies = packageJson.dependencies ?? {};
	const bunStoreDir = resolve(sourceRoot, "node_modules", ".bun");
	const hasInstalledSqlJs =
		existsSync(bunStoreDir) &&
		(await readdir(bunStoreDir)).some((entry) => entry.startsWith("sql.js@"));
	if (dependencies["sql.js"] === SQL_JS_VERSION && hasInstalledSqlJs) {
		return;
	}

	packageJson.dependencies = {
		...dependencies,
		"sql.js": SQL_JS_VERSION,
	};
	await writeFile(packageJsonPath, `${JSON.stringify(packageJson, null, 2)}\n`);
	run(bunBin, ["install"], { cwd: sourceRoot });
}

async function applyNodeAcpRuntimeTweaks(sourceRoot) {
	await writeFile(
		resolve(sourceRoot, "packages/opencode/src/cli/cmd/acp.ts"),
		`import type { Argv, InferredOptionTypes } from "yargs"
import { cmd } from "./cmd"
import { AgentSideConnection, ndJsonStream } from "@agentclientprotocol/sdk"
import { Log } from "../../util/log"

const options = {
  port: {
    type: "number" as const,
    describe: "port to listen on",
    default: 0,
  },
  hostname: {
    type: "string" as const,
    describe: "hostname to listen on",
    default: "127.0.0.1",
  },
  mdns: {
    type: "boolean" as const,
    describe: "enable mDNS service discovery (defaults hostname to 0.0.0.0)",
    default: false,
  },
  "mdns-domain": {
    type: "string" as const,
    describe: "custom domain name for mDNS service (default: opencode.local)",
    default: "opencode.local",
  },
  cors: {
    type: "string" as const,
    array: true,
    describe: "additional domains to allow for CORS",
    default: [] as string[],
  },
}

type NetworkOptions = InferredOptionTypes<typeof options>

function withNetworkOptions<T>(yargs: Argv<T>) {
  return yargs.options(options)
}

async function resolveNetworkOptions(args: NetworkOptions) {
  const { Config } = await import("../../config/config")
  const config = await Config.getGlobal()
  const portExplicitlySet = process.argv.includes("--port")
  const hostnameExplicitlySet = process.argv.includes("--hostname")
  const mdnsExplicitlySet = process.argv.includes("--mdns")
  const mdnsDomainExplicitlySet = process.argv.includes("--mdns-domain")
  const corsExplicitlySet = process.argv.includes("--cors")

  const mdns = mdnsExplicitlySet ? args.mdns : (config?.server?.mdns ?? args.mdns)
  const mdnsDomain = mdnsDomainExplicitlySet ? args["mdns-domain"] : (config?.server?.mdnsDomain ?? args["mdns-domain"])
  const port = portExplicitlySet ? args.port : (config?.server?.port ?? args.port)
  const hostname = hostnameExplicitlySet
    ? args.hostname
    : mdns && !config?.server?.hostname
      ? "0.0.0.0"
      : (config?.server?.hostname ?? args.hostname)
  const configCors = config?.server?.cors ?? []
  const argsCors = Array.isArray(args.cors) ? args.cors : args.cors ? [args.cors] : []
  const cors = [...configCors, ...argsCors]

  return { hostname, port, mdns, mdnsDomain, cors }
}

function wrapData<T>(data: T) {
  return { data }
}

async function loadProviderCatalog(directory: string | undefined) {
  const [{ Config }] = await Promise.all([import("../../config/config")])

  return withDirectory(directory, async () => {
    const config = await Config.get()
    const providers: any[] = []

    if (config?.provider?.anthropic || config?.model?.startsWith("anthropic/") || process.env.ANTHROPIC_API_KEY) {
      providers.push({
        id: "anthropic",
        name: "Anthropic",
        source: "config",
        env: ["ANTHROPIC_API_KEY"],
        options: {
          ...(config?.provider?.anthropic?.options ?? {}),
        },
        models: {
          "claude-sonnet-4-20250514": {
            id: "claude-sonnet-4-20250514",
            providerID: "anthropic",
            api: { id: "claude-sonnet-4-20250514", url: "", npm: "@ai-sdk/anthropic" },
            name: "Claude Sonnet 4",
            family: "claude-sonnet-4",
            capabilities: {
              temperature: true,
              reasoning: true,
              attachment: true,
              toolcall: true,
              input: { text: true, audio: false, image: true, video: false, pdf: true },
              output: { text: true, audio: false, image: false, video: false, pdf: false },
              interleaved: false,
            },
            cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
            limit: { context: 200000, output: 32000 },
            status: "active",
            options: {},
            headers: {},
            release_date: "2025-05-14",
            variants: {},
          },
          "claude-opus-4-1-20250805": {
            id: "claude-opus-4-1-20250805",
            providerID: "anthropic",
            api: { id: "claude-opus-4-1-20250805", url: "", npm: "@ai-sdk/anthropic" },
            name: "Claude Opus 4.1",
            family: "claude-opus-4-1",
            capabilities: {
              temperature: true,
              reasoning: true,
              attachment: true,
              toolcall: true,
              input: { text: true, audio: false, image: true, video: false, pdf: true },
              output: { text: true, audio: false, image: false, video: false, pdf: false },
              interleaved: false,
            },
            cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
            limit: { context: 200000, output: 32000 },
            status: "active",
            options: {},
            headers: {},
            release_date: "2025-08-05",
            variants: {},
          },
          "claude-haiku-4-5-20251001": {
            id: "claude-haiku-4-5-20251001",
            providerID: "anthropic",
            api: { id: "claude-haiku-4-5-20251001", url: "", npm: "@ai-sdk/anthropic" },
            name: "Claude Haiku 4.5",
            family: "claude-haiku-4-5",
            capabilities: {
              temperature: true,
              reasoning: false,
              attachment: true,
              toolcall: true,
              input: { text: true, audio: false, image: true, video: false, pdf: true },
              output: { text: true, audio: false, image: false, video: false, pdf: false },
              interleaved: false,
            },
            cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
            limit: { context: 200000, output: 16000 },
            status: "active",
            options: {},
            headers: {},
            release_date: "2025-10-01",
            variants: {},
          },
        },
      })
    }

    if (config?.provider?.openai || config?.model?.startsWith("openai/") || process.env.OPENAI_API_KEY) {
      providers.push({
        id: "openai",
        name: "OpenAI",
        source: "config",
        env: ["OPENAI_API_KEY"],
        options: {
          ...(config?.provider?.openai?.options ?? {}),
        },
        models: {
          "gpt-5": {
            id: "gpt-5",
            providerID: "openai",
            api: { id: "gpt-5", url: "", npm: "@ai-sdk/openai" },
            name: "GPT-5",
            family: "gpt-5",
            capabilities: {
              temperature: true,
              reasoning: true,
              attachment: true,
              toolcall: true,
              input: { text: true, audio: false, image: true, video: false, pdf: true },
              output: { text: true, audio: false, image: false, video: false, pdf: false },
              interleaved: false,
            },
            cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
            limit: { context: 200000, output: 32000 },
            status: "active",
            options: {},
            headers: {},
            release_date: "2025-01-01",
            variants: {},
          },
          "gpt-5-mini": {
            id: "gpt-5-mini",
            providerID: "openai",
            api: { id: "gpt-5-mini", url: "", npm: "@ai-sdk/openai" },
            name: "GPT-5 Mini",
            family: "gpt-5-mini",
            capabilities: {
              temperature: true,
              reasoning: true,
              attachment: true,
              toolcall: true,
              input: { text: true, audio: false, image: true, video: false, pdf: true },
              output: { text: true, audio: false, image: false, video: false, pdf: false },
              interleaved: false,
            },
            cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
            limit: { context: 200000, output: 16000 },
            status: "active",
            options: {},
            headers: {},
            release_date: "2025-01-01",
            variants: {},
          },
        },
      })
    }

    if (providers.length === 0) {
      providers.push({
        id: "anthropic",
        name: "Anthropic",
        source: "custom",
        env: ["ANTHROPIC_API_KEY"],
        options: {},
        models: {
          "claude-sonnet-4-20250514": {
            id: "claude-sonnet-4-20250514",
            providerID: "anthropic",
            api: { id: "claude-sonnet-4-20250514", url: "", npm: "@ai-sdk/anthropic" },
            name: "Claude Sonnet 4",
            family: "claude-sonnet-4",
            capabilities: {
              temperature: true,
              reasoning: true,
              attachment: true,
              toolcall: true,
              input: { text: true, audio: false, image: true, video: false, pdf: true },
              output: { text: true, audio: false, image: false, video: false, pdf: false },
              interleaved: false,
            },
            cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
            limit: { context: 200000, output: 32000 },
            status: "active",
            options: {},
            headers: {},
            release_date: "2025-05-14",
            variants: {},
          },
        },
      })
    }

    const defaults = Object.fromEntries(
      providers.map((provider) => {
        const specifiedModel =
          typeof config.model === "string" && config.model.startsWith(provider.id + "/")
            ? config.model.slice(provider.id.length + 1)
            : undefined
        return [
          provider.id,
          specifiedModel ?? Object.keys(provider.models)[0] ?? "",
        ]
      }),
    )

    return { providers, default: defaults }
  })
}

async function runServiceWithCurrentInstance<T>(
  serviceModule: { Service: any; defaultLayer: any },
  ctx: any,
  fn: (service: any) => any,
): Promise<T> {
  const [{ Effect, ManagedRuntime }, { InstanceRef }, { memoMap }, { Instance }] = await Promise.all([
    import("effect"),
    import("../../effect/instance-ref"),
    import("../../effect/run-service"),
    import("../../project/instance"),
  ])
  console.error("[opencode-acp] serviceRuntime:ctx", ctx.directory)
  const runtime = ManagedRuntime.make(serviceModule.defaultLayer, { memoMap })
  try {
    const result = await Instance.restore(
      ctx,
      () =>
        runtime.runPromise(
          Effect.provideService(serviceModule.Service.use(fn), InstanceRef, ctx),
        ),
    )
    console.error("[opencode-acp] serviceRuntime:done", ctx.directory)
    return result
  } catch (error) {
    console.error(
      "[opencode-acp] serviceRuntime:error",
      error instanceof Error ? error.stack ?? error.message : String(error),
    )
    throw error
  }
}

async function loadAgentCatalog(directory: string | undefined) {
  const { Config } = await import("../../config/config")

  return withDirectory(directory, async () => {
    const cfg = await Config.get()
    const agents = new Map<string, any>([
      [
        "build",
        {
          name: "build",
          description: "The default agent. Executes tools based on configured permissions.",
          mode: "primary",
        },
      ],
      [
        "plan",
        {
          name: "plan",
          description: "Plan mode. Disallows all edit tools.",
          mode: "primary",
        },
      ],
      [
        "general",
        {
          name: "general",
          description:
            "General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel.",
          mode: "subagent",
        },
      ],
      [
        "explore",
        {
          name: "explore",
          description:
            "Fast agent specialized for exploring codebases. Use this when you need to quickly find files, search code, or answer questions about the codebase.",
          mode: "subagent",
        },
      ],
      ["compaction", { name: "compaction", mode: "primary", hidden: true }],
      ["title", { name: "title", mode: "primary", hidden: true }],
      ["summary", { name: "summary", mode: "primary", hidden: true }],
    ])

    for (const [key, value] of Object.entries(cfg.agent ?? {})) {
      if (value?.disable) {
        agents.delete(key)
        continue
      }
      const current = agents.get(key) ?? {
        name: key,
        mode: "all",
      }
      agents.set(key, {
        ...current,
        ...(value?.name ? { name: value.name } : {}),
        ...(value?.description ? { description: value.description } : {}),
        ...(value?.mode ? { mode: value.mode } : {}),
        ...(value?.hidden !== undefined ? { hidden: value.hidden } : {}),
      })
    }

    const defaultAgent = cfg.default_agent ?? "build"
    return Array.from(agents.values()).sort((a, b) => {
      const aRank = a.name === defaultAgent ? 0 : 1
      const bRank = b.name === defaultAgent ? 0 : 1
      if (aRank !== bRank) return aRank - bRank
      return a.name.localeCompare(b.name)
    })
  })
}

async function loadCommandCatalog(directory: string | undefined) {
  const { Config } = await import("../../config/config")

  return withDirectory(directory, async () => {
    const cfg = await Config.get()
    const commands = new Map<string, any>([
      ["init", { name: "init", description: "create/update AGENTS.md" }],
      ["review", { name: "review", description: "review changes [commit|branch|pr], defaults to uncommitted" }],
    ])

    for (const [name, command] of Object.entries(cfg.command ?? {})) {
      commands.set(name, {
        name,
        ...(command?.description ? { description: command.description } : {}),
      })
    }

    return Array.from(commands.values()).sort((a, b) => a.name.localeCompare(b.name))
  })
}

async function withDirectory<T>(
  directory: string | undefined,
  fn: (ctx: any) => Promise<T>,
): Promise<T> {
  console.error("[opencode-acp] withDirectory:start", directory ?? process.cwd())
  const [{ Instance }, { InstanceBootstrap }] = await Promise.all([
    import("../../project/instance"),
    import("../../project/bootstrap"),
  ])
  console.error("[opencode-acp] withDirectory:modules:done", directory ?? process.cwd())
  return Instance.provide({
    directory: directory ?? process.cwd(),
    init: InstanceBootstrap,
    fn: () => fn(Instance.current),
  })
}

async function ensureProjectors() {
  if ((globalThis as any).__agentosOpencodeProjectorsReady) return
  console.error("[opencode-acp] projectors:ensure:start")
  const [{ SyncEvent }, { default: sessionProjectors }] = await Promise.all([
    import("../../sync"),
    import("../../session/projectors"),
  ])
  SyncEvent.init({
    projectors: sessionProjectors,
  })
  ;(globalThis as any).__agentosOpencodeProjectorsReady = true
  console.error("[opencode-acp] projectors:ensure:done")
}

async function createGlobalEventStream(signal?: AbortSignal) {
  const { GlobalBus } = await import("../../bus/global")
  let closed = false
  const queue: any[] = []
  let nextResolve: ((result: IteratorResult<any>) => void) | undefined

  const close = () => {
    if (closed) return
    closed = true
    GlobalBus.off("event", onEvent)
    signal?.removeEventListener("abort", close)
    nextResolve?.({ done: true, value: undefined })
    nextResolve = undefined
  }

  const onEvent = (event: any) => {
    if (closed) return
    if (nextResolve) {
      const resolve = nextResolve
      nextResolve = undefined
      resolve({ done: false, value: event })
      return
    }
    queue.push(event)
  }

  GlobalBus.on("event", onEvent)
  if (signal) {
    if (signal.aborted) close()
    else signal.addEventListener("abort", close, { once: true })
  }

  return {
    stream: (async function* () {
      try {
        while (true) {
          if (queue.length > 0) {
            yield queue.shift()
            continue
          }

          const next = await new Promise<IteratorResult<any>>((resolve) => {
            nextResolve = resolve
          })
          if (next.done) return
          yield next.value
        }
      } finally {
        close()
      }
    })(),
  }
}

function createLocalSdk() {
  return {
    global: {
      event: async ({ signal }: { signal?: AbortSignal }) => createGlobalEventStream(signal),
    },
    permission: {
      reply: async ({ requestID, reply, directory }: any) =>
        wrapData(
          await withDirectory(directory, async () => {
            const { Permission } = await import("../../permission")
            await Permission.reply({ requestID, reply })
            return true
          }),
        ),
    },
    config: {
      get: async ({ directory }: any) =>
        wrapData(
          await (async () => {
            const { Config } = await import("../../config/config")
            return withDirectory(directory, async () => {
              console.error("[opencode-acp] sdk.config.get:start", directory ?? process.cwd())
              const result = await Config.get()
              console.error("[opencode-acp] sdk.config.get:done", directory ?? process.cwd())
              return result
            })
          })(),
        ),
      providers: async ({ directory }: any) =>
        wrapData(
          await (async () => {
            console.error("[opencode-acp] sdk.config.providers:start", directory ?? process.cwd())
            return withDirectory(directory, async () => {
              const result = await loadProviderCatalog(directory)
              console.error(
                "[opencode-acp] sdk.config.providers:done",
                directory ?? process.cwd(),
                result.providers.length,
              )
              return result
            })
          })(),
        ),
    },
    app: {
      agents: async ({ directory }: any) =>
        wrapData(
          await loadAgentCatalog(directory),
        ),
    },
    command: {
      list: async ({ directory }: any) =>
        wrapData(
          await loadCommandCatalog(directory),
        ),
    },
    mcp: {
      add: async ({ directory, name, config }: any) =>
        wrapData(
          await (async () => {
            const { MCP } = await import("../../mcp")
            return withDirectory(directory, async () => MCP.add(name, config))
          })(),
        ),
    },
    session: {
      create: async ({ directory, title, permission, workspaceID, parentID }: any) =>
        wrapData(
          await (async () => {
            await ensureProjectors()
            return withDirectory(directory, async (ctx) => {
              const { Session } = await import("../../session")
              console.error("[opencode-acp] sdk.session.create:start", directory ?? process.cwd())
              try {
                const result = await runServiceWithCurrentInstance(Session, ctx, (service) =>
                  service.create({
                    ...(title ? { title } : {}),
                    ...(permission ? { permission } : {}),
                    ...(workspaceID ? { workspaceID } : {}),
                    ...(parentID ? { parentID } : {}),
                  }),
                )
                console.error("[opencode-acp] sdk.session.create:done", directory ?? process.cwd())
                return result
              } catch (error) {
                console.error(
                  "[opencode-acp] sdk.session.create:error",
                  error instanceof Error ? error.stack ?? error.message : String(error),
                )
                throw error
              }
            })
          })(),
        ),
      get: async ({ sessionID, directory }: any) =>
        wrapData(
          await (async () => {
            const { Session } = await import("../../session")
            return withDirectory(directory, async (ctx) =>
              runServiceWithCurrentInstance(Session, ctx, (service) => service.get(sessionID)),
            )
          })(),
        ),
      list: async ({ directory, roots, start, search, limit }: any) =>
        wrapData(
          await (async () => {
            const { Session } = await import("../../session")
            return withDirectory(directory, async (ctx) => {
              return runServiceWithCurrentInstance(Session, ctx, (service) =>
                service.list({ directory, roots, start, search, limit }),
              )
            })
          })(),
        ),
      fork: async ({ sessionID, messageID, directory }: any) =>
        wrapData(
          await (async () => {
            const { Session } = await import("../../session")
            await ensureProjectors()
            return withDirectory(directory, async (ctx) =>
              runServiceWithCurrentInstance(Session, ctx, (service) =>
                service.fork({
                  sessionID,
                  ...(messageID ? { messageID } : {}),
                }),
              ),
            )
          })(),
        ),
      messages: async ({ sessionID, limit, directory }: any) =>
        wrapData(
          await (async () => {
            const { Session } = await import("../../session")
            return withDirectory(directory, async (ctx) =>
              runServiceWithCurrentInstance(Session, ctx, (service) =>
                service.messages({ sessionID, ...(limit ? { limit } : {}) }),
              ),
            )
          })(),
        ),
      message: async ({ sessionID, messageID, directory }: any) =>
        wrapData(
          await (async () => {
            const { MessageV2 } = await import("../../session/message-v2")
            return withDirectory(directory, async () => MessageV2.get({ sessionID, messageID }))
          })(),
        ),
      prompt: async ({ sessionID, directory, ...input }: any) =>
        wrapData(
          await (async () => {
            const { SessionPrompt } = await import("../../session/prompt")
            await ensureProjectors()
            return withDirectory(directory, async (ctx) =>
              runServiceWithCurrentInstance(SessionPrompt, ctx, (service) =>
                service.prompt({ sessionID, ...input }),
              ),
            )
          })(),
        ),
      command: async ({ sessionID, directory, ...input }: any) =>
        wrapData(
          await (async () => {
            const { SessionPrompt } = await import("../../session/prompt")
            await ensureProjectors()
            return withDirectory(directory, async (ctx) =>
              runServiceWithCurrentInstance(SessionPrompt, ctx, (service) =>
                service.command({ sessionID, ...input }),
              ),
            )
          })(),
        ),
      summarize: async ({ sessionID, directory, providerID, modelID, auto = false }: any) =>
        wrapData(
          await (async () => {
            const [{ Session }, { SessionRevert }, { SessionCompaction }, { SessionPrompt }, { Agent }] =
              await Promise.all([
                import("../../session"),
                import("../../session/revert"),
                import("../../session/compaction"),
                import("../../session/prompt"),
                import("../../agent/agent"),
              ])
            await ensureProjectors()
            return withDirectory(directory, async (ctx) => {
              const session = await Session.get(sessionID)
              await SessionRevert.cleanup(session)
              const messages = await Session.messages({ sessionID })
              let currentAgent = await Agent.defaultAgent()
              for (let i = messages.length - 1; i >= 0; i--) {
                const info = messages[i].info
                if (info.role === "user") {
                  currentAgent = info.agent || (await Agent.defaultAgent())
                  break
                }
              }
              await SessionCompaction.create({
                sessionID,
                agent: currentAgent,
                model: { providerID, modelID },
                auto,
              })
              await runServiceWithCurrentInstance(SessionPrompt, ctx, (service) =>
                service.loop({ sessionID }),
              )
              return true
            })
          })(),
        ),
      abort: async ({ sessionID, directory }: any) =>
        wrapData(
          await (async () => {
            const { SessionPrompt } = await import("../../session/prompt")
            return withDirectory(directory, async (ctx) => {
              await runServiceWithCurrentInstance(SessionPrompt, ctx, (service) =>
                service.cancel(sessionID),
              )
              return true
            })
          })(),
        ),
    },
  } as any
}

export const AcpCommand = cmd({
  command: "acp",
  describe: "start ACP (Agent Client Protocol) server",
  builder: (yargs) => {
    return withNetworkOptions(yargs).option("cwd", {
      describe: "working directory",
      type: "string",
      default: process.cwd(),
    })
  },
  handler: async (args: NetworkOptions) => {
    process.env.OPENCODE_CLIENT = "acp"
    process.env.OPENCODE_DISABLE_MODELS_FETCH = process.env.OPENCODE_DISABLE_MODELS_FETCH ?? "1"
    process.env.OPENCODE_DISABLE_DEFAULT_PLUGINS = process.env.OPENCODE_DISABLE_DEFAULT_PLUGINS ?? "1"
    const [{ ACP }] = await Promise.all([import("../../acp/agent")])

    const log = Log.create({ service: "acp-command" })
    console.error("[opencode-acp] bootstrap:entered")
    console.error("[opencode-acp] network:resolve:start")
    const opts = await resolveNetworkOptions(args)
    console.error("[opencode-acp] network:resolve:done", JSON.stringify(opts))

    console.error("[opencode-acp] sdk:create:start")
    const sdk = createLocalSdk()
    console.error("[opencode-acp] sdk:create:done")

    console.error("[opencode-acp] streams:create:start")
    const input = new WritableStream<Uint8Array>({
      write(chunk) {
        return new Promise<void>((resolve, reject) => {
          process.stdout.write(chunk, (err) => {
            if (err) {
              reject(err)
            } else {
              resolve()
            }
          })
        })
      },
    })
    const output = new ReadableStream<Uint8Array>({
      start(controller) {
        process.stdin.on("data", (chunk: Buffer) => {
          controller.enqueue(new Uint8Array(chunk))
        })
        process.stdin.on("end", () => controller.close())
        process.stdin.on("error", (err) => controller.error(err))
      },
    })
    console.error("[opencode-acp] streams:create:done")

    console.error("[opencode-acp] ndjson:start")
    const stream = ndJsonStream(input, output)
    console.error("[opencode-acp] ndjson:done")
    console.error("[opencode-acp] agent:init:start")
    const agent = await ACP.init({ sdk })
    console.error("[opencode-acp] agent:init:done")

    console.error("[opencode-acp] connection:start")
    new AgentSideConnection((conn) => {
      return agent.create(conn, { sdk })
    }, stream)
    console.error("[opencode-acp] connection:done")

    log.info("setup connection")
    process.stdin.resume()
    await new Promise((resolve, reject) => {
      process.stdin.on("end", resolve)
      process.stdin.on("error", reject)
    })
  },
})
`,
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/server/instance.ts",
		(contents) =>
			contents
				.replace('import { TuiRoutes } from "./routes/tui"\n', "")
				.replace('import { PtyRoutes } from "./routes/pty"\n', "")
				.replace('    .route("/pty", PtyRoutes())\n', "")
				.replace('    .route("/tui", TuiRoutes())\n', ""),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/agent/agent.ts",
		(contents) =>
			contents.replace(
				`                    [path.relative(Instance.worktree, path.join(Global.Path.data, path.join("plans", "*.md")))]:
                      "allow",`,
				`                    [path.relative(ctx.worktree, path.join(Global.Path.data, path.join("plans", "*.md")))]:
                      "allow",`,
			),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/shell/shell.ts",
		(contents) => contents.replaceAll("Bun.which(", "which("),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/server/server.ts",
		(contents) =>
			contents
				.replace('import { MDNS } from "./mdns"\n', "")
				.replace(
					`    if (shouldPublishMDNS) {
      MDNS.publish(server.port, opts.mdnsDomain)
    } else if (opts.mdns) {`,
					`    let mdns:
      | {
          publish(port: number, domain?: string): void
          unpublish(): void
        }
      | undefined
    if (shouldPublishMDNS) {
      ;({ MDNS: mdns } = await import("./mdns"))
      mdns.publish(server.port, opts.mdnsDomain)
    } else if (opts.mdns) {`,
				)
				.replace(
					"      if (shouldPublishMDNS) MDNS.unpublish()\n",
					"      if (shouldPublishMDNS) mdns?.unpublish()\n",
				),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/project/bootstrap.ts",
		(contents) =>
			contents.replace(
				`  Bus.subscribe(Command.Event.Executed, async (payload) => {
    if (payload.properties.name === Command.Default.INIT) {
      Project.setInitialized(Instance.project.id)
    }
  })
`,
				`  Log.Default.info("bootstrap step", { step: "bus.subscribe:skipped" })
`,
			),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/acp/agent.ts",
		(contents) =>
			contents
				.replaceAll(
					`      console.error("[opencode-acp] agent.loadSessionMode:commands:done", directory, commands.length)
`,
					"",
				)
				.replace(
					`          const defaultAgentName = await AgentModule.defaultAgent()
          const resolvedModeId =
            availableModes.find((mode) => mode.name === defaultAgentName)?.id ?? availableModes[0].id
`,
					`          const resolvedModeId = availableModes[0].id
`,
				)
				.replace(
					`        const model = await defaultModel(this.config, directory)
`,
					`        console.error("[opencode-acp] agent.newSession:defaultModel:start", directory)
        const model = await defaultModel(this.config, directory)
        console.error("[opencode-acp] agent.newSession:defaultModel:done", directory, model.providerID, model.modelID)
`,
				)
				.replace(
					`        const state = await this.sessionManager.create(params.cwd, params.mcpServers, model)
`,
					`        console.error("[opencode-acp] agent.newSession:sessionManager.create:start", directory)
        const state = await this.sessionManager.create(params.cwd, params.mcpServers, model)
        console.error("[opencode-acp] agent.newSession:sessionManager.create:done", directory, state.id)
`,
				)
				.replace(
					`        const load = await this.loadSessionMode({
`,
					`        console.error("[opencode-acp] agent.newSession:loadSessionMode:start", directory)
        const load = await this.loadSessionMode({
`,
				)
				.replace(
					`      const providers = await this.sdk.config.providers({ directory }).then((x) => x.data!.providers)
`,
					`      console.error("[opencode-acp] agent.loadSessionMode:providers:start", directory)
      const providers = await this.sdk.config.providers({ directory }).then((x) => x.data!.providers)
      console.error("[opencode-acp] agent.loadSessionMode:providers:done", directory, providers.length)
`,
				)
				.replace(
					`      const modeState = await this.resolveModeState(directory, sessionId)
`,
					`      console.error("[opencode-acp] agent.loadSessionMode:resolveModeState:start", directory)
      const modeState = await this.resolveModeState(directory, sessionId)
      console.error("[opencode-acp] agent.loadSessionMode:resolveModeState:done", directory, modeState.availableModes.length)
`,
				)
				.replace(
					`      const commands = await this.config.sdk.command
`,
					`      console.error("[opencode-acp] agent.loadSessionMode:commands:start", directory)
      const commands = await this.config.sdk.command
`,
				),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/project/bootstrap.ts",
		(contents) =>
			contents.replace(
				`  Log.Default.info("bootstrap step", { step: "snapshot.init:start" })
  Snapshot.init()
  Log.Default.info("bootstrap step", { step: "snapshot.init:done" })
`,
				`  Log.Default.info("bootstrap step", { step: "snapshot.init:skipped" })
`,
			),
	);

	for (const relativePath of [
		"packages/opencode/src/cli/cmd/mcp.ts",
		"packages/opencode/src/config/config.ts",
		"packages/opencode/src/config/migrate-tui-config.ts",
		"packages/opencode/src/config/paths.ts",
		"packages/opencode/src/plugin/install.ts",
	]) {
		await rewriteSourceFile(sourceRoot, relativePath, (contents) =>
			contents.replaceAll(
				'"jsonc-parser"',
				'"jsonc-parser/lib/esm/main.js"',
			),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/plugin/index.ts",
		(contents) =>
			contents.replace(
				`  const state = Instance.state(async () => {
    const client = createOpencodeClient({
`,
				`  const state = Instance.state(async () => {
    if (Flag.OPENCODE_CLIENT === "acp") {
      log.info("skipping plugin runtime in ACP mode")
      return {
        hooks: [],
        input: {
          client: undefined as any,
          project: Instance.project,
          worktree: Instance.worktree,
          directory: Instance.directory,
          serverUrl: "",
          $: Bun.$,
        } as PluginInput,
      }
    }

    const client = createOpencodeClient({
`,
			),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/session/index.ts",
		(contents) =>
			contents
				.replace('import { SessionPrompt } from "./prompt"\n', "")
				.replace('import { Command } from "../command"\n', "")
				.replace(
					`      const initialize = Effect.fn("Session.initialize")(function* (input: {
        sessionID: SessionID
        modelID: ModelID
        providerID: ProviderID
        messageID: MessageID
      }) {
        yield* Effect.promise(() =>
          SessionPrompt.command({
            sessionID: input.sessionID,
            messageID: input.messageID,
            model: input.providerID + "/" + input.modelID,
            command: Command.Default.INIT,
            arguments: "",
          }),
        )
      })
`,
					`      const initialize = Effect.fn("Session.initialize")(function* (input: {
        sessionID: SessionID
        modelID: ModelID
        providerID: ProviderID
        messageID: MessageID
      }) {
        const [{ SessionPrompt }, { Command }] = yield* Effect.promise(() =>
          Promise.all([import("./prompt"), import("../command")]),
        )
        yield* Effect.promise(() =>
          SessionPrompt.command({
            sessionID: input.sessionID,
            messageID: input.messageID,
            model: input.providerID + "/" + input.modelID,
            command: Command.Default.INIT,
            arguments: "",
          }),
        )
      })
`,
			),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/session/prompt.ts",
		(contents) =>
			contents
				.replace(
					/        const text = yield\* Effect\.promise\(async \(signal\) => \{\n[\s\S]*?          return result\.text\n        \}\)\n/,
					`        const instanceCtx = yield* InstanceState.context
        const text = yield* Effect.promise((signal) =>
          Instance.restore(instanceCtx, async () => {
            const mdl = ag.model
              ? await Provider.getModel(ag.model.providerID, ag.model.modelID)
              : ((await Provider.getSmallModel(input.providerID)) ??
                (await Provider.getModel(input.providerID, input.modelID)))
            const msgs = onlySubtasks
              ? [{ role: "user" as const, content: subtasks.map((p) => p.prompt).join("\\n") }]
              : await MessageV2.toModelMessages(context, mdl)
            const result = await LLM.stream({
              agent: ag,
              user: firstInfo,
              system: [],
              small: true,
              tools: {},
              model: mdl,
              abort: signal,
              sessionID: input.session.id,
              retries: 2,
              messages: [{ role: "user", content: "Generate a title for this conversation:\\n" }, ...msgs],
            })
            return result.text
          }),
        )
`,
				)
				.replace(
					`      const getModel = (providerID: ProviderID, modelID: ModelID, sessionID: SessionID) =>
        Effect.promise(() =>
          Provider.getModel(providerID, modelID).catch((e) => {
            if (Provider.ModelNotFoundError.isInstance(e)) {
              const hint = e.data.suggestions?.length ? \` Did you mean: \${e.data.suggestions.join(", ")}?\` : ""
              Bus.publish(Session.Event.Error, {
                sessionID,
                error: new NamedError.Unknown({
                  message: \`Model not found: \${e.data.providerID}/\${e.data.modelID}.\${hint}\`,
                }).toObject(),
              })
            }
            throw e
          }),
        )
`,
					`      const getModel = (providerID: ProviderID, modelID: ModelID, sessionID: SessionID) =>
        Effect.gen(function* () {
          const instanceCtx = yield* InstanceState.context
          return yield* Effect.promise(() =>
            Instance.restore(instanceCtx, () =>
              Provider.getModel(providerID, modelID).catch((e) => {
                if (Provider.ModelNotFoundError.isInstance(e)) {
                  const hint = e.data.suggestions?.length ? \` Did you mean: \${e.data.suggestions.join(", ")}?\` : ""
                  Bus.publish(Session.Event.Error, {
                    sessionID,
                    error: new NamedError.Unknown({
                      message: \`Model not found: \${e.data.providerID}/\${e.data.modelID}.\${hint}\`,
                    }).toObject(),
                  })
                }
                throw e
              }),
            ),
          )
        })
`,
				)
				.replace(
					`        const model = input.model ?? ag.model ?? (yield* lastModel(input.sessionID))
        const full =
          !input.variant && ag.variant
            ? yield* Effect.promise(() => Provider.getModel(model.providerID, model.modelID).catch(() => undefined))
            : undefined
`,
					`        const model = input.model ?? ag.model ?? (yield* lastModel(input.sessionID))
        const instanceCtx = yield* InstanceState.context
        const full =
          !input.variant && ag.variant
            ? yield* Effect.promise(() =>
                Instance.restore(instanceCtx, () =>
                  Provider.getModel(model.providerID, model.modelID).catch(() => undefined),
                ),
              )
            : undefined
`,
				)
				.replace(
					`                      Effect.promise(() => Provider.getModel(info.model.providerID, info.model.modelID)).pipe(
`,
					`                      Effect.gen(function* () {
                        const instanceCtx = yield* InstanceState.context
                        return yield* Effect.promise(() =>
                          Instance.restore(instanceCtx, () =>
                            Provider.getModel(info.model.providerID, info.model.modelID),
                          ),
                        )
                      }).pipe(
`,
				)
				.replace(
					`                const tools = yield* resolveTools({
                  agent,
                  session,
                  model,
                  tools: lastUser.tools,
                  processor: handle,
                  bypassAgentCheck,
                  messages: msgs,
                })
`,
					`                const tools = yield* resolveTools({
                  agent,
                  session,
                  model,
                  tools: lastUser.tools,
                  processor: handle,
                  bypassAgentCheck,
                  messages: msgs,
                })
`,
				)
				.replace(
					`                const [skills, env, instructions, modelMsgs] = yield* Effect.promise(() =>
                  Promise.all([
                    SystemPrompt.skills(agent),
                    SystemPrompt.environment(model),
                    InstructionPrompt.system(),
                    MessageV2.toModelMessages(msgs, model),
                  ]),
                )
`,
					`                const instanceCtx = yield* InstanceState.context
                const [skills, env, instructions, modelMsgs] = yield* Effect.promise(() =>
                  Instance.restore(instanceCtx, () =>
                    (async () => {
                      console.error("[opencode-acp] prompt.system:skills:start", sessionID)
                      const skills = await Instance.restore(instanceCtx, () => SystemPrompt.skills(agent))
                      console.error("[opencode-acp] prompt.system:skills:done", sessionID)
                      console.error("[opencode-acp] prompt.system:env:start", sessionID)
                      const env = await Instance.restore(instanceCtx, () => SystemPrompt.environment(model))
                      console.error("[opencode-acp] prompt.system:env:done", sessionID)
                      console.error("[opencode-acp] prompt.system:instructions:start", sessionID)
                      const instructions = await Instance.restore(instanceCtx, () => InstructionPrompt.system())
                      console.error("[opencode-acp] prompt.system:instructions:done", sessionID)
                      console.error("[opencode-acp] prompt.system:modelMessages:start", sessionID)
                      const modelMsgs = await Instance.restore(instanceCtx, () =>
                        MessageV2.toModelMessages(msgs, model),
                      )
                      console.error("[opencode-acp] prompt.system:modelMessages:done", sessionID)
                      return [skills, env, instructions, modelMsgs] as const
                    })(),
                  ),
                )
`,
				)
				.replace(
					`  const defaultLayer = Layer.unwrap(
    Effect.sync(() =>
      layer.pipe(
        Layer.provide(SessionStatus.layer),
        Layer.provide(SessionCompaction.defaultLayer),
        Layer.provide(SessionProcessor.defaultLayer),
        Layer.provide(Command.defaultLayer),
        Layer.provide(Permission.layer),
        Layer.provide(MCP.defaultLayer),
        Layer.provide(LSP.defaultLayer),
        Layer.provide(FileTime.defaultLayer),
        Layer.provide(ToolRegistry.defaultLayer),
        Layer.provide(Truncate.layer),
        Layer.provide(AppFileSystem.defaultLayer),
        Layer.provide(Plugin.defaultLayer),
        Layer.provide(Session.defaultLayer),
        Layer.provide(Agent.defaultLayer),
        Layer.provide(Bus.layer),
      ),
    ),
  )
`,
					`  export const defaultLayer = Layer.unwrap(
    Effect.sync(() =>
      layer.pipe(
        Layer.provide(SessionStatus.layer),
        Layer.provide(SessionCompaction.defaultLayer),
        Layer.provide(SessionProcessor.defaultLayer),
        Layer.provide(Command.defaultLayer),
        Layer.provide(Permission.layer),
        Layer.provide(MCP.defaultLayer),
        Layer.provide(LSP.defaultLayer),
        Layer.provide(FileTime.defaultLayer),
        Layer.provide(ToolRegistry.defaultLayer),
        Layer.provide(Truncate.layer),
        Layer.provide(AppFileSystem.defaultLayer),
        Layer.provide(Plugin.defaultLayer),
        Layer.provide(Session.defaultLayer),
        Layer.provide(Agent.defaultLayer),
        Layer.provide(Bus.layer),
      ),
    ),
  )
`,
				),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/session/llm.ts",
		(contents) =>
				contents
					.replace(
						'import { Installation } from "@/installation"\n',
						'import { Installation } from "@/installation"\nimport { InstanceState } from "@/effect/instance-state"\n',
					)
					.replace(
						`        stream(input) {
	          return Stream.scoped(
	            Stream.unwrap(
              Effect.gen(function* () {
                const ctrl = yield* Effect.acquireRelease(
                  Effect.sync(() => new AbortController()),
                  (ctrl) => Effect.sync(() => ctrl.abort()),
                )

                const result = yield* Effect.promise(() => LLM.stream({ ...input, abort: ctrl.signal }))

                return Stream.fromAsyncIterable(result.fullStream, (e) =>
                  e instanceof Error ? e : new Error(String(e)),
                )
              }),
            ),
          )
	        },
`,
						`        stream(input) {
	          return Stream.scoped(
	            Stream.unwrap(
	              Effect.gen(function* () {
	                const instanceCtx = yield* InstanceState.context
	                const ctrl = yield* Effect.acquireRelease(
	                  Effect.sync(() => new AbortController()),
	                  (ctrl) => Effect.sync(() => ctrl.abort()),
	                )

	                const result = yield* Effect.promise(() =>
	                  Instance.restore(instanceCtx, () => LLM.stream({ ...input, abort: ctrl.signal })),
	                )

	                return Stream.fromAsyncIterable(result.fullStream, (e) =>
	                  e instanceof Error ? e : new Error(String(e)),
	                )
              }),
            ),
	          )
	        },
`,
					)
				.replace(
					`    const [language, cfg, provider, auth] = await Promise.all([
      Provider.getLanguage(input.model),
      Config.get(),
      Provider.getProvider(input.model.providerID),
      Auth.get(input.model.providerID),
    ])
`,
					`    const instanceCtx = Instance.current
    const [language, cfg, provider, auth] = await Instance.restore(instanceCtx, () =>
      Promise.all([
        Provider.getLanguage(input.model),
        Config.get(),
        Provider.getProvider(input.model.providerID),
        Auth.get(input.model.providerID),
      ]),
    )
`,
				)
				.replace(
					`    await Plugin.trigger(
      "experimental.chat.system.transform",
      { sessionID: input.sessionID, model: input.model },
      { system },
    )
`,
					`    await Instance.restore(instanceCtx, () =>
      Plugin.trigger(
        "experimental.chat.system.transform",
        { sessionID: input.sessionID, model: input.model },
        { system },
      ),
    )
`,
				)
				.replace(
					`    const params = await Plugin.trigger(
      "chat.params",
      {
        sessionID: input.sessionID,
        agent: input.agent.name,
        model: input.model,
        provider,
        message: input.user,
      },
      {
        temperature: input.model.capabilities.temperature
          ? (input.agent.temperature ?? ProviderTransform.temperature(input.model))
          : undefined,
        topP: input.agent.topP ?? ProviderTransform.topP(input.model),
        topK: ProviderTransform.topK(input.model),
        options,
      },
    )
`,
					`    const params = await Instance.restore(instanceCtx, () =>
      Plugin.trigger(
        "chat.params",
        {
          sessionID: input.sessionID,
          agent: input.agent.name,
          model: input.model,
          provider,
          message: input.user,
        },
        {
          temperature: input.model.capabilities.temperature
            ? (input.agent.temperature ?? ProviderTransform.temperature(input.model))
            : undefined,
          topP: input.agent.topP ?? ProviderTransform.topP(input.model),
          topK: ProviderTransform.topK(input.model),
          options,
        },
      ),
    )
`,
				)
				.replace(
					`    const { headers } = await Plugin.trigger(
      "chat.headers",
      {
        sessionID: input.sessionID,
        agent: input.agent.name,
        model: input.model,
        provider,
        message: input.user,
      },
      {
        headers: {},
      },
    )
`,
					`    const { headers } = await Instance.restore(instanceCtx, () =>
      Plugin.trigger(
        "chat.headers",
        {
          sessionID: input.sessionID,
          agent: input.agent.name,
          model: input.model,
          provider,
          message: input.user,
        },
        {
          headers: {},
        },
      ),
    )
`,
				)
				.replace(
					`    const tools = await resolveTools(input)
`,
					`    const tools = await Instance.restore(instanceCtx, () => resolveTools(input))
`,
				)
				.replace(
					`      headers: {
        ...(input.model.providerID.startsWith("opencode")
          ? {
              "x-opencode-project": Instance.project.id,
              "x-opencode-session": input.sessionID,
              "x-opencode-request": input.user.id,
              "x-opencode-client": Flag.OPENCODE_CLIENT,
            }
          : {
              "User-Agent": \`opencode/\${Installation.VERSION}\`,
            }),
        ...input.model.headers,
        ...headers,
      },
`,
					`      headers: {
        ...(input.model.providerID.startsWith("opencode")
          ? Instance.restore(instanceCtx, () => ({
              "x-opencode-project": Instance.project.id,
              "x-opencode-session": input.sessionID,
              "x-opencode-request": input.user.id,
              "x-opencode-client": Flag.OPENCODE_CLIENT,
            }))
          : {
              "User-Agent": \`opencode/\${Installation.VERSION}\`,
            }),
        ...input.model.headers,
        ...headers,
      },
`,
				)
				.replace(
					`    return streamText({
`,
					`    return Instance.restore(instanceCtx, () => streamText({
`,
				)
				.replace(
					`    })
  }
`,
					`    }))
  }
`,
				),
		);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/session/instruction.ts",
		(contents) =>
			contents
				.replace(
					`async function resolveRelative(instruction: string): Promise<string[]> {
  if (!Flag.OPENCODE_DISABLE_PROJECT_CONFIG) {
    return Filesystem.globUp(instruction, Instance.directory, Instance.worktree).catch(() => [])
  }
  if (!Flag.OPENCODE_CONFIG_DIR) {
    log.warn(
      \`Skipping relative instruction "\${instruction}" - no OPENCODE_CONFIG_DIR set while project config is disabled\`,
    )
    return []
  }
  return Filesystem.globUp(instruction, Flag.OPENCODE_CONFIG_DIR, Flag.OPENCODE_CONFIG_DIR).catch(() => [])
}
`,
					`async function resolveRelative(instruction: string): Promise<string[]> {
  const ctx = Instance.current
  if (!Flag.OPENCODE_DISABLE_PROJECT_CONFIG) {
    return Instance.restore(ctx, () =>
      Filesystem.globUp(instruction, ctx.directory, ctx.worktree).catch(() => []),
    )
  }
  if (!Flag.OPENCODE_CONFIG_DIR) {
    log.warn(
      \`Skipping relative instruction "\${instruction}" - no OPENCODE_CONFIG_DIR set while project config is disabled\`,
    )
    return []
  }
  return Filesystem.globUp(instruction, Flag.OPENCODE_CONFIG_DIR, Flag.OPENCODE_CONFIG_DIR).catch(() => [])
}
`,
				)
				.replace(
					`  export async function systemPaths() {
    const config = await Config.get()
`,
					`  export async function systemPaths() {
    const ctx = Instance.current
    const config = await Instance.restore(ctx, () => Config.get())
`,
				)
				.replace(
					`        const matches = await Filesystem.findUp(file, Instance.directory, Instance.worktree)
`,
					`        const matches = await Instance.restore(ctx, () =>
          Filesystem.findUp(file, ctx.directory, ctx.worktree),
        )
`,
				)
				.replace(
					`          : await resolveRelative(instruction)
`,
					`          : await Instance.restore(ctx, () => resolveRelative(instruction))
`,
				)
				.replace(
					`  export async function system() {
    const config = await Config.get()
    const paths = await systemPaths()
`,
					`  export async function system() {
    const ctx = Instance.current
    const config = await Instance.restore(ctx, () => Config.get())
    const paths = await Instance.restore(ctx, () => systemPaths())
`,
				)
				.replace(
					`  export async function resolve(messages: MessageV2.WithParts[], filepath: string, messageID: string) {
    const system = await systemPaths()
    const already = loaded(messages)
    const results: { filepath: string; content: string }[] = []

    const target = path.resolve(filepath)
    let current = path.dirname(target)
    const root = path.resolve(Instance.directory)
`,
					`  export async function resolve(messages: MessageV2.WithParts[], filepath: string, messageID: string) {
    const ctx = Instance.current
    const system = await Instance.restore(ctx, () => systemPaths())
    const already = loaded(messages)
    const results: { filepath: string; content: string }[] = []

    const target = path.resolve(filepath)
    let current = path.dirname(target)
    const root = path.resolve(ctx.directory)
`,
				),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/provider/provider.ts",
		(contents) =>
			contents
				.replace(
					'import { Config } from "../config/config"\n',
					'import { Config } from "../config/config"\nimport { Instance } from "../project/instance"\n',
				)
				.replace(
					`  async function loadProviders() {
    const cfg = await Config.get()
`,
					`  async function loadProviders() {
    const ctx = Instance.current
    const cfg = await Instance.restore(ctx, () => Config.get())
`,
				)
				.replace(
					`  export async function defaultModel() {
    const cfg = await Config.get()
`,
					`  export async function defaultModel() {
    const ctx = Instance.current
    const cfg = await Instance.restore(ctx, () => Config.get())
`,
				)
				.replace(
					`    const providers = await loadProviders()
    for (const provider of Object.values(providers)) {
`,
					`    const providers = await Instance.restore(ctx, () => loadProviders())
    for (const provider of Object.values(providers)) {
`,
				),
		);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/effect/instance-state.ts",
		(contents) =>
			contents.replace(
				`  export const context = Effect.fnUntraced(function* () {
    return (yield* InstanceRef) ?? Instance.current
  })()
`,
				`  export const context = Effect.fnUntraced(function* () {
    const ref = yield* InstanceRef
    if (ref) return ref
    try {
      return Instance.current
    } catch (error) {
      console.error("[opencode-acp] missing-instance-context", new Error().stack)
      throw error
    }
  })()
`,
			),
	);

	await writeFile(
		resolve(sourceRoot, "packages/opencode/src/effect/run-service.ts"),
		`import { Effect, Layer, ManagedRuntime } from "effect"
import * as ServiceMap from "effect/ServiceMap"
import { Instance } from "@/project/instance"
import { Context } from "@/util/context"
import { InstanceRef } from "./instance-ref"

export const memoMap = Layer.makeMemoMapUnsafe()

function attach<A, E, R>(effect: Effect.Effect<A, E, R>) {
  try {
    const ctx = Instance.current
    return {
      ctx,
      effect: Effect.provideService(effect, InstanceRef, ctx),
    }
  } catch (err) {
    if (!(err instanceof Context.NotFound)) throw err
  }
  return { ctx: undefined, effect }
}

export function makeRuntime<I, S, E>(service: ServiceMap.Service<I, S>, layer: Layer.Layer<I, E>) {
  let rt: ManagedRuntime.ManagedRuntime<I, E> | undefined
  const getRuntime = () => (rt ??= ManagedRuntime.make(layer, { memoMap }))

  return {
    runSync: <A, Err>(fn: (svc: S) => Effect.Effect<A, Err, I>) => {
      const attached = attach(service.use(fn))
      return attached.ctx
        ? Instance.restore(attached.ctx, () => getRuntime().runSync(attached.effect))
        : getRuntime().runSync(attached.effect)
    },
    runPromiseExit: <A, Err>(fn: (svc: S) => Effect.Effect<A, Err, I>, options?: Effect.RunOptions) => {
      const attached = attach(service.use(fn))
      return attached.ctx
        ? Instance.restore(attached.ctx, () => getRuntime().runPromiseExit(attached.effect, options))
        : getRuntime().runPromiseExit(attached.effect, options)
    },
    runPromise: <A, Err>(fn: (svc: S) => Effect.Effect<A, Err, I>, options?: Effect.RunOptions) => {
      const attached = attach(service.use(fn))
      return attached.ctx
        ? Instance.restore(attached.ctx, () => getRuntime().runPromise(attached.effect, options))
        : getRuntime().runPromise(attached.effect, options)
    },
    runFork: <A, Err>(fn: (svc: S) => Effect.Effect<A, Err, I>) => {
      const attached = attach(service.use(fn))
      return attached.ctx
        ? Instance.restore(attached.ctx, () => getRuntime().runFork(attached.effect))
        : getRuntime().runFork(attached.effect)
    },
    runCallback: <A, Err>(fn: (svc: S) => Effect.Effect<A, Err, I>) => {
      const attached = attach(service.use(fn))
      return attached.ctx
        ? Instance.restore(attached.ctx, () => getRuntime().runCallback(attached.effect))
        : getRuntime().runCallback(attached.effect)
    },
  }
}
`,
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/tool/tool.ts",
		(contents) =>
			contents
				.replace(
					'import { Truncate } from "./truncate"\n',
					'import { Truncate } from "./truncate"\nimport { InstanceState } from "@/effect/instance-state"\n',
				)
				.replace(
					`        const toolInfo = init instanceof Function ? await init(initCtx) : init
        const execute = toolInfo.execute
        toolInfo.execute = async (args, ctx) => {
`,
					`        const toolInfo =
          init instanceof Function ? await InstanceState.bind(() => init(initCtx))() : init
        const execute = toolInfo.execute
        toolInfo.execute = InstanceState.bind(async (args, ctx) => {
`,
				)
				.replace(
					`        }
        return toolInfo
`,
					`        })
        return toolInfo
`,
				),
		);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/storage/db.ts",
		(contents) =>
			contents.replace(
				`    db.run("PRAGMA journal_mode = WAL")
    db.run("PRAGMA synchronous = NORMAL")
    db.run("PRAGMA busy_timeout = 5000")
    db.run("PRAGMA cache_size = -64000")
    db.run("PRAGMA foreign_keys = ON")
    db.run("PRAGMA wal_checkpoint(PASSIVE)")`,
				`    db.$client.exec("PRAGMA journal_mode = WAL")
    db.$client.exec("PRAGMA synchronous = NORMAL")
    db.$client.exec("PRAGMA busy_timeout = 5000")
    db.$client.exec("PRAGMA cache_size = -64000")
    db.$client.exec("PRAGMA foreign_keys = ON")
    db.$client.exec("PRAGMA wal_checkpoint(PASSIVE)")`,
			),
	);
}

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/provider/provider.ts",
		(contents) =>
			contents
				.replace(
					`// Direct imports for bundled providers
import { createAmazonBedrock, type AmazonBedrockProviderSettings } from "@ai-sdk/amazon-bedrock"
import { createAnthropic } from "@ai-sdk/anthropic"
import { createAzure } from "@ai-sdk/azure"
import { createGoogleGenerativeAI } from "@ai-sdk/google"
import { createVertex } from "@ai-sdk/google-vertex"
import { createVertexAnthropic } from "@ai-sdk/google-vertex/anthropic"
import { createOpenAI } from "@ai-sdk/openai"
import { createOpenAICompatible } from "@ai-sdk/openai-compatible"
import { createOpenRouter, type LanguageModelV2 } from "@openrouter/ai-sdk-provider"
import { createOpenaiCompatible as createGitHubCopilotOpenAICompatible } from "./sdk/openai-compatible/src"
import { createXai } from "@ai-sdk/xai"
import { createMistral } from "@ai-sdk/mistral"
import { createGroq } from "@ai-sdk/groq"
import { createDeepInfra } from "@ai-sdk/deepinfra"
import { createCerebras } from "@ai-sdk/cerebras"
import { createCohere } from "@ai-sdk/cohere"
import { createGateway } from "@ai-sdk/gateway"
import { createTogetherAI } from "@ai-sdk/togetherai"
import { createPerplexity } from "@ai-sdk/perplexity"
import { createVercel } from "@ai-sdk/vercel"
import { createGitLab } from "@gitlab/gitlab-ai-provider"
import { ProviderTransform } from "./transform"
`,
					`import type { LanguageModelV2 } from "@openrouter/ai-sdk-provider"
import { ProviderTransform } from "./transform"
`,
				)
				.replace(
					`  const BUNDLED_PROVIDERS: Record<string, (options: any) => SDK> = {
    "@ai-sdk/amazon-bedrock": createAmazonBedrock,
    "@ai-sdk/anthropic": createAnthropic,
    "@ai-sdk/azure": createAzure,
    "@ai-sdk/google": createGoogleGenerativeAI,
    "@ai-sdk/google-vertex": createVertex,
    "@ai-sdk/google-vertex/anthropic": createVertexAnthropic,
    "@ai-sdk/openai": createOpenAI,
    "@ai-sdk/openai-compatible": createOpenAICompatible,
    "@openrouter/ai-sdk-provider": createOpenRouter,
    "@ai-sdk/xai": createXai,
    "@ai-sdk/mistral": createMistral,
    "@ai-sdk/groq": createGroq,
    "@ai-sdk/deepinfra": createDeepInfra,
    "@ai-sdk/cerebras": createCerebras,
    "@ai-sdk/cohere": createCohere,
    "@ai-sdk/gateway": createGateway,
    "@ai-sdk/togetherai": createTogetherAI,
    "@ai-sdk/perplexity": createPerplexity,
    "@ai-sdk/vercel": createVercel,
    "@gitlab/gitlab-ai-provider": createGitLab,
    // @ts-ignore (TODO: kill this code so we dont have to maintain it)
    "@ai-sdk/github-copilot": createGitHubCopilotOpenAICompatible,
  }
`,
					`  const BUNDLED_PROVIDERS: Record<string, (options: any) => Promise<SDK>> = {
    "@ai-sdk/amazon-bedrock": async (options) =>
      (await import("@ai-sdk/amazon-bedrock")).createAmazonBedrock(options),
    "@ai-sdk/anthropic": async (options) => (await import("@ai-sdk/anthropic")).createAnthropic(options),
    "@ai-sdk/azure": async (options) => (await import("@ai-sdk/azure")).createAzure(options),
    "@ai-sdk/google": async (options) => (await import("@ai-sdk/google")).createGoogleGenerativeAI(options),
    "@ai-sdk/google-vertex": async (options) => (await import("@ai-sdk/google-vertex")).createVertex(options),
    "@ai-sdk/google-vertex/anthropic": async (options) =>
      (await import("@ai-sdk/google-vertex/anthropic")).createVertexAnthropic(options),
    "@ai-sdk/openai": async (options) => (await import("@ai-sdk/openai")).createOpenAI(options),
    "@ai-sdk/openai-compatible": async (options) =>
      (await import("@ai-sdk/openai-compatible")).createOpenAICompatible(options),
    "@openrouter/ai-sdk-provider": async (options) =>
      (await import("@openrouter/ai-sdk-provider")).createOpenRouter(options),
    "@ai-sdk/xai": async (options) => (await import("@ai-sdk/xai")).createXai(options),
    "@ai-sdk/mistral": async (options) => (await import("@ai-sdk/mistral")).createMistral(options),
    "@ai-sdk/groq": async (options) => (await import("@ai-sdk/groq")).createGroq(options),
    "@ai-sdk/deepinfra": async (options) => (await import("@ai-sdk/deepinfra")).createDeepInfra(options),
    "@ai-sdk/cerebras": async (options) => (await import("@ai-sdk/cerebras")).createCerebras(options),
    "@ai-sdk/cohere": async (options) => (await import("@ai-sdk/cohere")).createCohere(options),
    "@ai-sdk/gateway": async (options) => (await import("@ai-sdk/gateway")).createGateway(options),
    "@ai-sdk/togetherai": async (options) => (await import("@ai-sdk/togetherai")).createTogetherAI(options),
    "@ai-sdk/perplexity": async (options) => (await import("@ai-sdk/perplexity")).createPerplexity(options),
    "@ai-sdk/vercel": async (options) => (await import("@ai-sdk/vercel")).createVercel(options),
    "@gitlab/gitlab-ai-provider": async (options) =>
      (await import("@gitlab/gitlab-ai-provider")).createGitLab(options),
    "@ai-sdk/github-copilot": async (options) =>
      (await import("./sdk/openai-compatible/src")).createOpenaiCompatible(options),
  }
`,
				)
				.replace(
					"        async getModel(sdk: ReturnType<typeof createGitLab>, modelID: string) {\n",
					"        async getModel(sdk: any, modelID: string) {\n",
				)
				.replace(
					`        const loaded = bundledFn({
          name: model.providerID,
          ...options,
        })
`,
					`        const loaded = await bundledFn({
          name: model.providerID,
          ...options,
        })
`,
				),
	);

	await writeFile(
		resolve(sourceRoot, "packages/opencode/src/provider/provider.ts"),
		`import z from "zod"
import { createAnthropic } from "@ai-sdk/anthropic"
import { createOpenAI } from "@ai-sdk/openai"
import type { LanguageModelV3 } from "@ai-sdk/provider"
import { NamedError } from "@opencode-ai/util/error"
import { Config } from "../config/config"
import { ModelID, ProviderID } from "./schema"

type ProviderConfig = {
  name?: string
  env?: string[]
  npm?: string
  api?: string
  options?: Record<string, any>
  models?: Record<string, any>
}

type ProviderSeed = {
  name: string
  env: string[]
  npm: string
  models: Array<{
    id: string
    name: string
    family?: string
    reasoning?: boolean
    attachment?: boolean
    input?: Partial<{ text: boolean; audio: boolean; image: boolean; video: boolean; pdf: boolean }>
    output?: Partial<{ text: boolean; audio: boolean; image: boolean; video: boolean; pdf: boolean }>
    limit?: Partial<{ context: number; input: number; output: number }>
    cost?: Partial<{
      input: number
      output: number
      cache: Partial<{ read: number; write: number }>
    }>
    releaseDate?: string
  }>
}

export namespace Provider {
  export const Model = z
    .object({
      id: ModelID.zod,
      providerID: ProviderID.zod,
      api: z.object({
        id: z.string(),
        url: z.string(),
        npm: z.string(),
      }),
      name: z.string(),
      family: z.string().optional(),
      capabilities: z.object({
        temperature: z.boolean(),
        reasoning: z.boolean(),
        attachment: z.boolean(),
        toolcall: z.boolean(),
        input: z.object({
          text: z.boolean(),
          audio: z.boolean(),
          image: z.boolean(),
          video: z.boolean(),
          pdf: z.boolean(),
        }),
        output: z.object({
          text: z.boolean(),
          audio: z.boolean(),
          image: z.boolean(),
          video: z.boolean(),
          pdf: z.boolean(),
        }),
        interleaved: z.union([
          z.boolean(),
          z.object({
            field: z.enum(["reasoning_content", "reasoning_details"]),
          }),
        ]),
      }),
      cost: z.object({
        input: z.number(),
        output: z.number(),
        cache: z.object({
          read: z.number(),
          write: z.number(),
        }),
        experimentalOver200K: z
          .object({
            input: z.number(),
            output: z.number(),
            cache: z.object({
              read: z.number(),
              write: z.number(),
            }),
          })
          .optional(),
      }),
      limit: z.object({
        context: z.number(),
        input: z.number().optional(),
        output: z.number(),
      }),
      status: z.enum(["alpha", "beta", "deprecated", "active"]),
      options: z.record(z.string(), z.any()),
      headers: z.record(z.string(), z.string()),
      release_date: z.string(),
      variants: z.record(z.string(), z.record(z.string(), z.any())).optional(),
    })
    .meta({
      ref: "Model",
    })
  export type Model = z.infer<typeof Model>

  export const Info = z
    .object({
      id: ProviderID.zod,
      name: z.string(),
      source: z.enum(["env", "config", "custom", "api"]),
      env: z.string().array(),
      key: z.string().optional(),
      options: z.record(z.string(), z.any()),
      models: z.record(z.string(), Model),
    })
    .meta({
      ref: "Provider",
    })
  export type Info = z.infer<typeof Info>

  const DEFAULT_CONTEXT_LIMIT = 200_000
  const DEFAULT_OUTPUT_LIMIT = 32_000

  const PROVIDER_SEEDS: Record<string, ProviderSeed> = {
    anthropic: {
      name: "Anthropic",
      env: ["ANTHROPIC_API_KEY"],
      npm: "@ai-sdk/anthropic",
      models: [
        {
          id: "claude-sonnet-4-20250514",
          name: "Claude Sonnet 4",
          family: "claude-sonnet-4",
          reasoning: true,
          attachment: true,
          input: { image: true, pdf: true },
          releaseDate: "2025-05-14",
        },
        {
          id: "claude-opus-4-1-20250805",
          name: "Claude Opus 4.1",
          family: "claude-opus-4-1",
          reasoning: true,
          attachment: true,
          input: { image: true, pdf: true },
          releaseDate: "2025-08-05",
        },
        {
          id: "claude-haiku-4-5-20251001",
          name: "Claude Haiku 4.5",
          family: "claude-haiku-4-5",
          reasoning: false,
          attachment: true,
          input: { image: true, pdf: true },
          limit: { output: 16_000 },
          releaseDate: "2025-10-01",
        },
      ],
    },
    openai: {
      name: "OpenAI",
      env: ["OPENAI_API_KEY"],
      npm: "@ai-sdk/openai",
      models: [
        {
          id: "gpt-5",
          name: "GPT-5",
          family: "gpt-5",
          reasoning: true,
          attachment: true,
          input: { image: true, pdf: true },
          releaseDate: "2025-01-01",
        },
        {
          id: "gpt-5-mini",
          name: "GPT-5 Mini",
          family: "gpt-5-mini",
          reasoning: true,
          attachment: true,
          input: { image: true, pdf: true },
          limit: { output: 16_000 },
          releaseDate: "2025-01-01",
        },
        {
          id: "gpt-5-nano",
          name: "GPT-5 Nano",
          family: "gpt-5-nano",
          reasoning: false,
          attachment: true,
          input: { image: true, pdf: true },
          limit: { output: 8_000 },
          releaseDate: "2025-01-01",
        },
      ],
    },
  }

  const providerCache = new Map<string, any>()
  const languageCache = new Map<string, LanguageModelV3>()
  const priority = ["gpt-5", "claude-sonnet-4", "big-pickle", "gemini-3-pro"]

  function firstEnv(names: string[]) {
    for (const name of names) {
      const value = process.env[name]
      if (typeof value === "string" && value.length > 0) {
        return value
      }
    }
    return undefined
  }

  function cloneRecord<T extends Record<string, any>>(value: T | undefined): T {
    return { ...(value ?? ({} as T)) }
  }

  function buildModel(
    providerID: ProviderID,
    seed: ProviderSeed,
    input: {
      id: string
      name?: string
      family?: string
      reasoning?: boolean
      attachment?: boolean
      toolCall?: boolean
      status?: "alpha" | "beta" | "deprecated" | "active"
      input?: Partial<{ text: boolean; audio: boolean; image: boolean; video: boolean; pdf: boolean }>
      output?: Partial<{ text: boolean; audio: boolean; image: boolean; video: boolean; pdf: boolean }>
      limit?: Partial<{ context: number; input: number; output: number }>
      cost?: Partial<{
        input: number
        output: number
        cache: Partial<{ read: number; write: number }>
      }>
      headers?: Record<string, string>
      options?: Record<string, any>
      api?: {
        id?: string
        url?: string
        npm?: string
      }
      releaseDate?: string
      variants?: Record<string, Record<string, any>>
    },
  ): Model {
    return {
      id: ModelID.make(input.id),
      providerID,
      api: {
        id: input.api?.id ?? input.id,
        url: input.api?.url ?? "",
        npm: input.api?.npm ?? seed.npm,
      },
      name: input.name ?? input.id,
      family: input.family,
      capabilities: {
        temperature: true,
        reasoning: input.reasoning ?? false,
        attachment: input.attachment ?? false,
        toolcall: input.toolCall ?? true,
        input: {
          text: input.input?.text ?? true,
          audio: input.input?.audio ?? false,
          image: input.input?.image ?? false,
          video: input.input?.video ?? false,
          pdf: input.input?.pdf ?? false,
        },
        output: {
          text: input.output?.text ?? true,
          audio: input.output?.audio ?? false,
          image: input.output?.image ?? false,
          video: input.output?.video ?? false,
          pdf: input.output?.pdf ?? false,
        },
        interleaved: false,
      },
      cost: {
        input: input.cost?.input ?? 0,
        output: input.cost?.output ?? 0,
        cache: {
          read: input.cost?.cache?.read ?? 0,
          write: input.cost?.cache?.write ?? 0,
        },
      },
      limit: {
        context: input.limit?.context ?? DEFAULT_CONTEXT_LIMIT,
        ...(input.limit?.input !== undefined ? { input: input.limit.input } : {}),
        output: input.limit?.output ?? DEFAULT_OUTPUT_LIMIT,
      },
      status: input.status ?? "active",
      options: cloneRecord(input.options),
      headers: cloneRecord(input.headers),
      release_date: input.releaseDate ?? "2025-01-01",
      variants: input.variants ?? {},
    }
  }

  function buildSeedModels(providerID: ProviderID, seed: ProviderSeed) {
    return Object.fromEntries(
      seed.models.map((model) => [model.id, buildModel(providerID, seed, model)]),
    ) as Record<string, Model>
  }

  function applyConfiguredModels(
    providerID: ProviderID,
    seed: ProviderSeed,
    provider: Info,
    configuredProvider: ProviderConfig | undefined,
  ) {
    for (const [modelID, raw] of Object.entries(configuredProvider?.models ?? {})) {
      const existing = provider.models[modelID]
      provider.models[modelID] = buildModel(providerID, seed, {
        id: modelID,
        name: raw?.name ?? existing?.name ?? modelID,
        family: raw?.family ?? existing?.family,
        reasoning: raw?.reasoning ?? existing?.capabilities.reasoning ?? false,
        attachment: raw?.attachment ?? existing?.capabilities.attachment ?? false,
        toolCall: raw?.tool_call ?? existing?.capabilities.toolcall ?? true,
        status: raw?.status ?? existing?.status ?? "active",
        input: {
          text: raw?.modalities?.input?.includes("text") ?? existing?.capabilities.input.text ?? true,
          audio: raw?.modalities?.input?.includes("audio") ?? existing?.capabilities.input.audio ?? false,
          image: raw?.modalities?.input?.includes("image") ?? existing?.capabilities.input.image ?? false,
          video: raw?.modalities?.input?.includes("video") ?? existing?.capabilities.input.video ?? false,
          pdf: raw?.modalities?.input?.includes("pdf") ?? existing?.capabilities.input.pdf ?? false,
        },
        output: {
          text: raw?.modalities?.output?.includes("text") ?? existing?.capabilities.output.text ?? true,
          audio: raw?.modalities?.output?.includes("audio") ?? existing?.capabilities.output.audio ?? false,
          image: raw?.modalities?.output?.includes("image") ?? existing?.capabilities.output.image ?? false,
          video: raw?.modalities?.output?.includes("video") ?? existing?.capabilities.output.video ?? false,
          pdf: raw?.modalities?.output?.includes("pdf") ?? existing?.capabilities.output.pdf ?? false,
        },
        limit: {
          context: raw?.limit?.context ?? existing?.limit.context,
          input: raw?.limit?.input ?? existing?.limit.input,
          output: raw?.limit?.output ?? existing?.limit.output,
        },
        cost: {
          input: raw?.cost?.input ?? existing?.cost.input,
          output: raw?.cost?.output ?? existing?.cost.output,
          cache: {
            read: raw?.cost?.cache_read ?? existing?.cost.cache.read,
            write: raw?.cost?.cache_write ?? existing?.cost.cache.write,
          },
        },
        headers: {
          ...existing?.headers,
          ...cloneRecord(raw?.headers),
        },
        options: {
          ...existing?.options,
          ...cloneRecord(raw?.options),
        },
        api: {
          id: raw?.id ?? existing?.api.id ?? modelID,
          url: raw?.provider?.api ?? configuredProvider?.api ?? existing?.api.url ?? "",
          npm: raw?.provider?.npm ?? configuredProvider?.npm ?? existing?.api.npm ?? seed.npm,
        },
        releaseDate: raw?.release_date ?? existing?.release_date,
        variants: raw?.variants ?? existing?.variants,
      })
    }
  }

  async function loadProviders() {
    const cfg = await Config.get()
    const configured = (cfg.provider ?? {}) as Record<string, ProviderConfig>
    const providers: Record<string, Info> = {}

    for (const [providerName, seed] of Object.entries(PROVIDER_SEEDS)) {
      const providerID = ProviderID.make(providerName)
      const configuredProvider = configured[providerName]
      const configuredModel = typeof cfg.model === "string" && cfg.model.startsWith(providerName + "/")
      const key = firstEnv(configuredProvider?.env ?? seed.env) ?? configuredProvider?.options?.apiKey

      if (!configuredProvider && !configuredModel && !key) {
        continue
      }

      const info: Info = {
        id: providerID,
        name: configuredProvider?.name ?? seed.name,
        source: configuredProvider ? "config" : key ? "env" : "custom",
        env: configuredProvider?.env ?? seed.env,
        ...(typeof key === "string" && key.length > 0 ? { key } : {}),
        options: {
          ...cloneRecord(configuredProvider?.options),
        },
        models: buildSeedModels(providerID, seed),
      }

      applyConfiguredModels(providerID, seed, info, configuredProvider)
      providers[providerID] = info
    }

    if (Object.keys(providers).length === 0) {
      const fallback = PROVIDER_SEEDS.anthropic
      providers[ProviderID.anthropic] = {
        id: ProviderID.anthropic,
        name: fallback.name,
        source: "custom",
        env: fallback.env,
        options: {},
        models: buildSeedModels(ProviderID.anthropic, fallback),
      }
    }

    return providers as Record<ProviderID, Info>
  }

  function modelKey(providerID: ProviderID, modelID: ModelID) {
    return String(providerID) + "/" + String(modelID)
  }

  export async function list() {
    return loadProviders()
  }

  export async function getProvider(providerID: ProviderID) {
    const providers = await loadProviders()
    const provider = providers[providerID]
    if (!provider) {
      throw new InitError({ providerID })
    }
    return provider
  }

  export async function getModel(providerID: ProviderID, modelID: ModelID) {
    const provider = await getProvider(providerID)
    const model = provider.models[modelID]
    if (model) return model

    const suggestions = Object.keys(provider.models).filter(
      (candidate) => candidate.includes(String(modelID)) || String(modelID).includes(candidate),
    )
    throw new ModelNotFoundError({
      providerID,
      modelID,
      ...(suggestions.length ? { suggestions: suggestions.slice(0, 3) } : {}),
    })
  }

  async function getSdk(provider: Info, model: Model) {
    const cacheKey = JSON.stringify({
      providerID: provider.id,
      apiId: model.api.id,
      baseURL: provider.options?.baseURL,
      headers: provider.options?.headers,
      key: provider.key,
    })
    if (providerCache.has(cacheKey)) {
      return providerCache.get(cacheKey)
    }

    const options = {
      ...cloneRecord(provider.options),
      ...(provider.key ? { apiKey: provider.key } : {}),
      headers: {
        ...cloneRecord(provider.options?.headers),
        ...cloneRecord(model.headers),
      },
    }

    let sdk: any
    switch (provider.id) {
      case ProviderID.anthropic:
        sdk = createAnthropic(options)
        break
      case ProviderID.openai:
        sdk = createOpenAI(options)
        break
      default:
        throw new InitError(
          { providerID: provider.id },
          { cause: new Error("Unsupported provider in ACP VM build: " + provider.id) },
        )
    }

    providerCache.set(cacheKey, sdk)
    return sdk
  }

  export async function getLanguage(model: Model) {
    const key = modelKey(model.providerID, model.id)
    const cached = languageCache.get(key)
    if (cached) return cached

    try {
      const provider = await getProvider(model.providerID)
      const sdk = await getSdk(provider, model)
      const language =
        model.providerID === ProviderID.openai && typeof sdk.responses === "function"
          ? sdk.responses(model.api.id)
          : sdk.languageModel(model.api.id)
      languageCache.set(key, language)
      return language
    } catch (cause) {
      throw new InitError({ providerID: model.providerID }, { cause })
    }
  }

  export async function closest(providerID: ProviderID, query: string[]) {
    const provider = await getProvider(providerID).catch(() => undefined)
    if (!provider) return undefined
    for (const item of query) {
      const match = Object.keys(provider.models).find((modelID) => modelID.includes(item))
      if (match) return { providerID, modelID: ModelID.make(match) }
    }
    return undefined
  }

  export async function getSmallModel(providerID: ProviderID) {
    const provider = await getProvider(providerID).catch(() => undefined)
    if (!provider) return undefined

    const preferred =
      providerID === ProviderID.anthropic
        ? ["haiku", "mini", "nano"]
        : ["mini", "nano", "haiku"]

    for (const token of preferred) {
      const match = Object.values(provider.models).find((model) => model.id.includes(token))
      if (match) return match
    }

    return undefined
  }

  export async function defaultModel() {
    const cfg = await Config.get()
    if (cfg.model) {
      const parsed = parseModel(cfg.model)
      const model = await getModel(parsed.providerID, parsed.modelID).catch(() => undefined)
      if (model) {
        return {
          providerID: parsed.providerID,
          modelID: parsed.modelID,
        }
      }
    }

    const providers = await loadProviders()
    for (const provider of Object.values(providers)) {
      const [model] = sort(Object.values(provider.models))
      if (model) {
        return {
          providerID: provider.id,
          modelID: model.id,
        }
      }
    }

    return {
      providerID: ProviderID.anthropic,
      modelID: ModelID.make("claude-sonnet-4-20250514"),
    }
  }

  export function sort<T extends { id: string }>(models: T[]) {
    return [...models].sort((a, b) => {
      const aPriority = priority.findIndex((item) => a.id.includes(item))
      const bPriority = priority.findIndex((item) => b.id.includes(item))
      const aRank = aPriority === -1 ? Number.MAX_SAFE_INTEGER : aPriority
      const bRank = bPriority === -1 ? Number.MAX_SAFE_INTEGER : bPriority
      if (aRank !== bRank) return aRank - bRank

      const aLatest = a.id.includes("latest") ? 1 : 0
      const bLatest = b.id.includes("latest") ? 1 : 0
      if (aLatest !== bLatest) return aLatest - bLatest

      return a.id.localeCompare(b.id)
    })
  }

  export function parseModel(model: string) {
    const [providerID, ...rest] = model.split("/")
    return {
      providerID: ProviderID.make(providerID),
      modelID: ModelID.make(rest.join("/")),
    }
  }

  export const ModelNotFoundError = NamedError.create(
    "ProviderModelNotFoundError",
    z.object({
      providerID: ProviderID.zod,
      modelID: ModelID.zod,
      suggestions: z.array(z.string()).optional(),
    }),
  )

  export const InitError = NamedError.create(
    "ProviderInitError",
    z.object({
      providerID: ProviderID.zod,
    }),
  )
}
`,
	);

	await writeFile(
		resolve(sourceRoot, "packages/opencode/src/plugin/index.ts"),
		`import { Effect, Layer, ServiceMap } from "effect"

export namespace Plugin {
  export interface Interface {
    readonly trigger: <Name extends string, Input, Output>(
      name: Name,
      input: Input,
      output: Output,
    ) => Effect.Effect<Output>
    readonly list: () => Effect.Effect<any[]>
    readonly init: () => Effect.Effect<void>
  }

  export class Service extends ServiceMap.Service<Service, Interface>()("@opencode/Plugin") {}

  const noop = Service.of({
    trigger: <Name extends string, Input, Output>(_name: Name, _input: Input, output: Output) =>
      Effect.succeed(output),
    list: () => Effect.succeed([]),
    init: () => Effect.void,
  })

  export const layer = Layer.succeed(Service, noop)
  export const defaultLayer = layer

  export async function trigger<Name extends string, Input, Output>(
    _name: Name,
    _input: Input,
    output: Output,
  ): Promise<Output> {
    return output
  }

  export async function list(): Promise<any[]> {
    return []
  }

  export async function init(): Promise<void> {}
}
`,
	);

	await writeFile(
		resolve(sourceRoot, "packages/opencode/src/project/bootstrap.ts"),
		`import { Instance } from "./instance"
import { Log } from "@/util/log"

export async function InstanceBootstrap() {
  Log.Default.info("bootstrapping", { directory: Instance.directory })
  Log.Default.info("bootstrap step", { step: "minimal:init" })
}
`,
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/project/instance.ts",
		(contents) =>
			contents
				.replace(
					`function boot(input: { directory: string; init?: () => Promise<any>; project?: Project.Info; worktree?: string }) {
  return iife(async () => {
`,
					`function boot(input: { directory: string; init?: () => Promise<any>; project?: Project.Info; worktree?: string }) {
  return iife(async () => {
    Log.Default.info("instance boot:start", { directory: input.directory })
`,
				)
				.replace(
					`    await context.provide(ctx, async () => {
      await input.init?.()
    })
    return ctx
`,
					`    Log.Default.info("instance boot:ctx", {
      directory: ctx.directory,
      worktree: ctx.worktree,
      projectID: ctx.project.id,
    })
    await context.provide(ctx, async () => {
      Log.Default.info("instance boot:init:start", { directory: ctx.directory })
      await input.init?.()
      Log.Default.info("instance boot:init:done", { directory: ctx.directory })
    })
    Log.Default.info("instance boot:done", { directory: ctx.directory })
    return ctx
`,
				)
				.replace(
					`    const ctx = await existing
    return context.provide(ctx, async () => {
      return input.fn()
    })
`,
					`    Log.Default.info("instance provide:await:start", { directory })
    const ctx = await existing
    Log.Default.info("instance provide:await:done", { directory })
    return context.provide(ctx, async () => {
      Log.Default.info("instance provide:fn:start", { directory })
      const result = await input.fn()
      Log.Default.info("instance provide:fn:done", { directory })
      return result
    })
`,
				),
	);

	await rewriteSourceFile(
		sourceRoot,
		"packages/opencode/src/project/project.ts",
		(contents) =>
			contents.includes('log.info("phase2 select project"')
				? contents
				: contents
				.replace(
					`        // Phase 2: upsert
        const row = yield* db((d) => d.select().from(ProjectTable).where(eq(ProjectTable.id, data.id)).get())
`,
					`        // Phase 2: upsert
        log.info("phase2 select project", { projectID: data.id })
        const row = yield* db((d) => d.select().from(ProjectTable).where(eq(ProjectTable.id, data.id)).get())
        log.info("phase2 select project done", { projectID: data.id, found: !!row })
`,
				)
				.replace(
					`        yield* db((d) =>
          d
            .insert(ProjectTable)
            .values({
`,
					`        log.info("phase2 upsert project", {
          projectID: result.id,
          sandboxes: result.sandboxes.length,
        })
        yield* db((d) =>
          d
            .insert(ProjectTable)
            .values({
`,
				)
				.replace(
					`        if (data.id !== ProjectID.global) {
`,
					`        log.info("phase2 upsert project done", { projectID: result.id })
        if (data.id !== ProjectID.global) {
`,
				),
	);

	await writeFile(
		resolve(sourceRoot, "packages/opencode/src/share/share-next.ts"),
		`export namespace ShareNext {
  const EMPTY_API = {
    create: "",
    sync: () => "",
    remove: () => "",
    data: () => "",
  }

  export async function url() {
    return ""
  }

  export async function request() {
    return {
      headers: {},
      api: EMPTY_API,
      baseUrl: "",
    }
  }

  export async function init() {}

  export async function create(_sessionID: string) {
    return { id: "", url: "", secret: "" }
  }

  export async function remove(_sessionID: string) {}
}
`,
	);

	await writeFile(
		resolve(sourceRoot, "packages/opencode/src/cli/cmd/tui/win32.ts"),
		`export function win32DisableProcessedInput() {}
export function win32FlushInputBuffer() {}
export function win32InstallCtrlCGuard() {
  return
}
`,
	);
}

async function assertPreparedSource(sourceRoot) {
	const instanceSource = await readFile(
		resolve(sourceRoot, "packages/opencode/src/server/instance.ts"),
		"utf8",
	);
	if (
		instanceSource.includes('import { PtyRoutes } from "./routes/pty"') ||
		instanceSource.includes('import { TuiRoutes } from "./routes/tui"') ||
		instanceSource.includes('.route("/pty", PtyRoutes())') ||
		instanceSource.includes('.route("/tui", TuiRoutes())')
	) {
		throw new Error("Prepared OpenCode source still exposes PTY/TUI routes in the ACP build");
	}

	const shellSource = await readFile(
		resolve(sourceRoot, "packages/opencode/src/shell/shell.ts"),
		"utf8",
	);
	if (shellSource.includes("Bun.which(")) {
		throw new Error("Prepared OpenCode source still references Bun.which in shell.ts");
	}

	const win32Source = await readFile(
		resolve(sourceRoot, "packages/opencode/src/cli/cmd/tui/win32.ts"),
		"utf8",
	);
	if (win32Source.includes("bun:ffi")) {
		throw new Error("Prepared OpenCode source still references bun:ffi in the Win32 TUI shim");
	}

	const dbNodeSource = await readFile(
		resolve(sourceRoot, "packages/opencode/src/storage/db.node.ts"),
		"utf8",
	);
	if (
		!dbNodeSource.includes('from "drizzle-orm/node-sqlite"') ||
		!dbNodeSource.includes('from "node:sqlite"')
	) {
		throw new Error("Prepared OpenCode source does not use the native node:sqlite database path");
	}
}

async function assertBundleClean(bundlePath) {
	const bundle = await readFile(bundlePath, "utf8");
	for (const pattern of [
		"bun:ffi",
		"bun-pty",
		"hono/bun",
		'.route("/pty", PtyRoutes())',
		'.route("/tui", TuiRoutes())',
		"Bun.which(",
		"bun:sqlite",
	]) {
		if (bundle.includes(pattern)) {
			throw new Error(
				`OpenCode ACP bundle still contains forbidden runtime dependency: ${pattern}`,
			);
		}
	}
}

async function main() {
	if (!existsSync(bunBin)) {
		throw new Error(
			`bun is not installed for @rivet-dev/agent-os-opencode (expected ${bunBin}). Run pnpm install first.`,
		);
	}

	mkdirSync(distDir, { recursive: true });
	mkdirSync(cacheDir, { recursive: true });
	rmSync(bundleDir, { recursive: true, force: true });

	const patch = readFileSync(patchPath, "utf-8");
	const buildScript = readFileSync(fileURLToPath(import.meta.url), "utf-8");
	const patchHash = createHash("sha256")
		.update(`${SOURCE_VERSION}\n${patch}\n${buildScript}`)
		.digest("hex")
		.slice(0, 16);
	const sourceRoot = join(cacheDir, `source-v${SOURCE_VERSION}-${patchHash}`);
	const preparedMarker = join(sourceRoot, ".agent-os-prepared.json");
	const tarballPath = join(cacheDir, `opencode-v${SOURCE_VERSION}.tar.gz`);

	if (!existsSync(preparedMarker)) {
		rmSync(sourceRoot, { recursive: true, force: true });
		mkdirSync(sourceRoot, { recursive: true });

		if (!existsSync(tarballPath)) {
			process.stdout.write(`Downloading OpenCode v${SOURCE_VERSION} source...\n`);
			await downloadFile(SOURCE_TARBALL_URL, tarballPath);
		}

		run("tar", ["-xzf", tarballPath, "--strip-components=1", "-C", sourceRoot]);
		run(bunBin, ["install", "--frozen-lockfile"], { cwd: sourceRoot });
		await ensureNodeAcpPatch(sourceRoot, tarballPath);
		await applyNodeAcpRuntimeTweaks(sourceRoot);
		await assertPreparedSource(sourceRoot);

		writeFileSync(
			preparedMarker,
			JSON.stringify(
				{
					sourceVersion: SOURCE_VERSION,
					sourceRepository: SOURCE_REPOSITORY,
					patchHash,
				},
				null,
				2,
				) + "\n",
		);
	}

	await ensureNodeAcpPatch(sourceRoot, tarballPath);
	await applyNodeAcpRuntimeTweaks(sourceRoot);
	await assertPreparedSource(sourceRoot);

	const migrations = await readMigrations(sourceRoot);
	const buildHelperDir = await mkdtemp(join(tmpdir(), "agentos-opencode-build-"));
	const buildHelperPath = join(buildHelperDir, "build-opencode-acp.mjs");
	const bunVersion =
		spawnSync(bunBin, ["--version"], { encoding: "utf-8" }).stdout?.trim() ??
		"unknown";

	try {
		await writeFile(
			buildHelperPath,
			`
import { mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";

const outdir = process.env.OUTDIR;
if (!outdir) {
	throw new Error("OUTDIR is required");
}

const result = await Bun.build({
	target: "node",
	format: "esm",
	outdir,
	entrypoints: ["./packages/opencode/src/cli/cmd/acp.ts"],
	define: {
		OPENCODE_MIGRATIONS: ${JSON.stringify(JSON.stringify(migrations))},
		OPENCODE_LIBC: ${JSON.stringify(JSON.stringify("glibc"))},
	},
});
if (!result.success) {
	for (const log of result.logs) {
		console.error(log);
	}
	throw new Error("OpenCode ACP bundle build failed");
}
for (const output of result.outputs) {
	const filePath = join(outdir, output.path);
	await mkdir(dirname(filePath), { recursive: true });
	await Bun.write(filePath, output);
}
`,
		);

		run(bunBin, [buildHelperPath], {
			cwd: sourceRoot,
			env: {
				...process.env,
				OUTDIR: bundleDir,
			},
		});
	} finally {
		rmSync(buildHelperDir, { recursive: true, force: true });
	}

	await assertBundleClean(join(bundleDir, "acp.js"));

	writeFileSync(
		manifestPath,
		JSON.stringify(
			{
				source: {
					repository: SOURCE_REPOSITORY,
					version: SOURCE_VERSION,
					tarballUrl: SOURCE_TARBALL_URL,
				},
				build: {
					bunVersion,
					patchHash,
					externalDependencies: [],
					entry: "./opencode-acp/acp.js",
				},
			},
			null,
			2,
		) + "\n",
	);
}

void main().catch((error) => {
	process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
	process.exitCode = 1;
});
