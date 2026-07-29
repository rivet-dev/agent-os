import type { RegistryIconName } from "./registry-icons";

export interface RegistryEntryBase {
	slug: string;
	title: string;
	description: string;
	category?: string;
	types: (
		| "file-system"
		| "binding"
		| "agent"
		| "sandbox-extension"
		| "software"
		| "browser"
		| "deploy"
	)[];
	featured?: boolean;
	// Marks an entry as in beta — renders a "Beta" pill on the registry card
	// and detail page.
	beta?: boolean;
	// Lucide icon name, resolved via REGISTRY_ICONS. Used when no `image` is
	// provided. Must be a serializable string so it survives the Astro island
	// prop boundary.
	icon?: RegistryIconName;
	image?: string;
	// Documentation page for this entry. Always set for agents (generated as
	// /docs/agents/<agentId>); overrides the per-type docs fallback on the
	// detail page.
	docsHref?: string;
	// The session agent id, set for all generated agent entries.
	agentId?: string;
}

export interface RegistryEntryAvailable extends RegistryEntryBase {
	status: "available";
	package: string;
}

export interface RegistryEntryComingSoon extends RegistryEntryBase {
	status: "coming-soon";
}

// An entry that isn't a separately installable package — e.g. a built-in agent
// adapter. The registry page links straight to its documentation instead of
// rendering an install command.
export interface RegistryEntryDocs extends RegistryEntryBase {
	status: "docs";
	// Link to the docs page documenting this entry.
	docsHref: string;
	// For built-in agent adapters: the npm software package that provides the
	// adapter and the session agent id. Used to render a usage example on the
	// registry page before linking out to the docs.
	package?: string;
	agentId?: string;
}

// A built-in capability configured inline (e.g. a filesystem mount plugin),
// not a separately installable package. Renders a configuration snippet and a
// link to the relevant docs instead of an `npm install` command.
export interface RegistryEntryConfig extends RegistryEntryBase {
	status: "config";
	// Short label for how it's enabled, e.g. `mounts: [{ plugin: { id: "s3" } }]`.
	configId: string;
	// TypeScript snippet showing how to configure it.
	configExample: string;
	// Link to the docs page that documents this configuration.
	docsHref: string;
}

// An entry whose card links straight to an external page (e.g. a deployment
// guide on rivet.dev). No detail page is generated for it.
export interface RegistryEntryExternal extends RegistryEntryBase {
	status: "external";
	href: string;
}

export type RegistryEntry =
	| RegistryEntryAvailable
	| RegistryEntryComingSoon
	| RegistryEntryDocs
	| RegistryEntryConfig
	| RegistryEntryExternal;

// Agent and software entries are generated from the monorepo's software/ catalog
// by scripts/gen-registry.mjs — a package is listed iff its
// agentos-package.json has a `registry` block with title + description. Run
// `pnpm dev`/`pnpm build` (or the script directly) to refresh after editing a
// package. Only non-package capabilities (file systems, bindings, sandbox
// mounting) are curated by hand below.
import generated from "../generated/registry.json";
import { DEPLOY_TARGETS } from "./deploy-targets";

// Featured is a website decision, not package metadata: generated entries
// with a slug in this set get the featured treatment on the registry page.
const FEATURED_GENERATED_SLUGS = new Set(["browserbase", "git", "duckdb"]);

const generatedRegistry: RegistryEntry[] = generated.entries.map(
	({ priority: _priority, ...entry }) =>
		({
			...entry,
			featured: FEATURED_GENERATED_SLUGS.has(entry.slug) || undefined,
		}) as RegistryEntry,
);

const curatedRegistry: RegistryEntry[] = [
	// Agents
	{
		slug: "custom-agent",
		title: "Custom Agent",
		status: "docs",
		docsHref: "/docs/agents/custom",
		description:
			"Bring your own coding agent to agentOS by speaking the Agent Client Protocol (ACP) inside the VM.",
		types: ["agent"],
		icon: "Wrench",
	},

	// File Systems
	{
		slug: "host-dir",
		title: "Host Directory",
		status: "config",
		configId: 'plugin: { id: "host_dir" }',
		docsHref: "/docs/filesystem",
		description:
			"Project a real host directory into the VM, Docker-style. The guest sees only the mounted subtree, never the wider host filesystem.",
		types: ["file-system"],
		icon: "HardDrive",
		configExample: `import { agentOS, setup } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";

const vm = agentOS({
  software: [pi],
  mounts: [
    {
      path: "/home/agentos/repo",
      plugin: { id: "host_dir", config: { hostPath: "/path/to/repo" } },
      readOnly: true,
    },
  ],
});

export const registry = setup({ use: { vm } });`,
	},
	{
		slug: "s3",
		title: "S3",
		status: "config",
		configId: 'plugin: { id: "s3" }',
		docsHref: "/docs/filesystem",
		description:
			"Mount an S3-compatible bucket as a filesystem. File contents are chunked into S3 objects, keeping large files, partial reads/writes, and snapshots efficient.",
		types: ["file-system"],
		featured: true,
		image: "/images/registry/s3.svg",
		configExample: `import { agentOS, setup } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";

const vm = agentOS({
  software: [pi],
  mounts: [
    {
      path: "/home/agentos/data",
      plugin: {
        id: "s3",
        config: { bucket: "my-bucket", prefix: "agent-data/", region: "us-east-1" },
      },
    },
  ],
});

export const registry = setup({ use: { vm } });`,
	},
	{
		slug: "google-drive",
		title: "Google Drive",
		status: "config",
		configId: 'plugin: { id: "google_drive" }',
		docsHref: "/docs/filesystem",
		description:
			"Mount a Google Drive folder as a filesystem for reading and writing documents and files.",
		types: ["file-system"],
		featured: true,
		image: "/images/registry/google-drive.svg",
		configExample: `import { agentOS, setup } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";

const vm = agentOS({
  software: [pi],
  mounts: [
    {
      path: "/home/agentos/drive",
      plugin: {
        id: "google_drive",
        config: {
          credentials: {
            clientEmail: process.env.GOOGLE_DRIVE_CLIENT_EMAIL!,
            privateKey: process.env.GOOGLE_DRIVE_PRIVATE_KEY!,
          },
          folderId: process.env.GOOGLE_DRIVE_FOLDER_ID!,
        },
      },
    },
  ],
});

export const registry = setup({ use: { vm } });`,
	},
	{
		slug: "memory",
		title: "In-Memory",
		status: "config",
		configId: 'plugin: { id: "memory" }',
		docsHref: "/docs/filesystem",
		description:
			"Mount an ephemeral in-memory directory. Fast scratch space that is discarded when the VM is destroyed.",
		types: ["file-system"],
		icon: "HardDrive",
		configExample: `import { agentOS, setup } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";

const vm = agentOS({
  software: [pi],
  mounts: [
    // Ephemeral scratch space for large intermediate artifacts (build
    // output, downloads, caches). Lives in memory only: it never touches
    // the VM's persisted filesystem, so it stays out of snapshots and is
    // discarded when the VM is disposed.
    { path: "/tmp/scratch", plugin: { id: "memory", config: {} } },
  ],
});

export const registry = setup({ use: { vm } });`,
	},
	// Bindings
	{
		slug: "sandbox",
		title: "Sandbox",
		status: "available",
		package: "@rivet-dev/agentos-sandbox",
		description:
			"Mount a sandbox filesystem and expose process management bindings. Works with any Sandbox Agent provider.",
		types: ["binding", "file-system"],
		icon: "Monitor",
		docsHref: "/docs/sandboxes",
	},
	// Sandbox Mounting
	{
		slug: "local",
		title: "Local",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes directly on the local machine for development and testing.",
		beta: true,
		types: ["sandbox-extension"],
		icon: "Monitor",
	},
	{
		slug: "docker",
		title: "Docker",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes in Docker containers for isolated local execution.",
		beta: true,
		types: ["sandbox-extension"],
		image: "/images/registry/docker.svg",
	},
	{
		slug: "e2b",
		title: "E2B",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes on E2B's cloud infrastructure for secure, ephemeral environments.",
		beta: true,
		types: ["sandbox-extension"],
		featured: true,
		image: "/images/registry/e2b.svg",
	},
	{
		slug: "daytona",
		title: "Daytona",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes on Daytona's managed development environments.",
		beta: true,
		types: ["sandbox-extension"],
		image: "/images/registry/daytona.svg",
	},
	{
		slug: "modal",
		title: "Modal",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes on Modal's serverless cloud infrastructure.",
		beta: true,
		types: ["sandbox-extension"],
		image: "/images/registry/modal.svg",
	},
	{
		slug: "cloudflare",
		title: "Cloudflare",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes on Cloudflare's global network with Sandbox SDK containers.",
		beta: true,
		types: ["sandbox-extension"],
		image: "/images/registry/cloudflare.svg",
	},
	{
		slug: "vercel",
		title: "Vercel",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes on Vercel's edge and serverless platform.",
		beta: true,
		types: ["sandbox-extension"],
		image: "/images/registry/vercel.svg",
	},
	{
		slug: "computesdk",
		title: "ComputeSDK",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes using the ComputeSDK compute provider.",
		beta: true,
		types: ["sandbox-extension"],
		image: "/images/registry/computesdk.svg",
	},
	{
		slug: "sprites",
		title: "Sprites",
		status: "available",
		package: "sandbox-agent",
		description:
			"Run sandboxes on Sprites' cloud sandbox infrastructure.",
		beta: true,
		types: ["sandbox-extension"],
		image: "/images/registry/sprites.svg",
	},
];

// Deploy targets come from the shared data module (also rendered by the docs
// <DeployTargets /> component). Their cards link straight to the external
// deployment guides; the "deploy-" slug prefix avoids colliding with
// same-named sandbox providers (e.g. vercel).
const deployRegistry: RegistryEntry[] = DEPLOY_TARGETS.map((target) => ({
	slug: `deploy-${target.slug}`,
	title: target.title,
	status: "external",
	href: target.href,
	description: target.description,
	types: ["deploy"],
	image: target.image,
}));

export const registry: RegistryEntry[] = [
	...generatedRegistry,
	...curatedRegistry,
	...deployRegistry,
];

// A generated slug colliding with a curated one would silently shadow a
// detail page; fail the build instead.
{
	const seen = new Set<string>();
	for (const entry of registry) {
		if (seen.has(entry.slug)) {
			throw new Error(`registry: duplicate slug "${entry.slug}"`);
		}
		seen.add(entry.slug);
	}
}
