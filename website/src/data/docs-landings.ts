import {
	faForwardFast,
	faLightbulb,
	faScaleBalanced,
	faRobot,
	faMessages,
	faCheck,
	faKey,
	faDownload,
	faFloppyDisk,
	faWrench,
	faTerminal,
	faGlobe,
	faClock,
	faHardDrive,
	faNodeJs,
	faGauge,
	faTowerBroadcast,
	faArrowsLeftRight,
	faDiagramNext,
	faBook,
	faCircleInfo,
} from "@rivet-gg/icons";
import type { DocsLandingData } from "@rivet-dev/docs-theme/components/docs/DocsLanding";

/**
 * Section-overview landings (the icon-grid element, like rivet.dev's docs
 * landings). Keyed by route; [...slug].astro renders <DocsLanding> for a
 * matching path instead of the prose article.
 */
export const docsLandings: Record<string, DocsLandingData> = {
	"/docs": {
		title: "Documentation",
		subtitle:
			"agentOS runs coding agents inside isolated VMs with full filesystem, process, and network control — a lightweight VM in your own process with bindings, permissions, and orchestration built in.",
		sections: [
			{
				title: "Get Started",
				items: [
					{ title: "Quickstart", href: "/docs/quickstart", icon: faForwardFast, description: "Boot a VM and run your first coding agent." },
					{ title: "Crash Course", href: "/docs/crash-course", icon: faLightbulb, description: "Learn the core agentOS concepts end to end." },
					{ title: "agentOS vs Sandbox", href: "/docs/versus-sandbox", icon: faScaleBalanced, description: "How agentOS compares to a plain sandbox." },
				],
			},
		],
	},
	"/cookbooks": {
		title: "Cookbooks",
		subtitle:
			"Runnable examples for every agentOS capability. Each page mirrors an example in the repo — follow the source link to run it.",
		sections: [
			{
				title: "Quickstart",
				items: [
					{ title: "Quickstart App", href: "/cookbooks/quickstart-app", icon: faForwardFast, description: "A complete starter app." },
					{ title: "Crash Course", href: "/cookbooks/crash-course", icon: faLightbulb, description: "The core concepts, hands on." },
				],
			},
			{
				title: "Agents",
				items: [
					{ title: "Pi Agent", href: "/cookbooks/pi", icon: faRobot, description: "Run the Pi coding agent." },
					{ title: "Claude Agent", href: "/cookbooks/claude", icon: faRobot, description: "Run Claude Code." },
					{ title: "Codex Agent", href: "/cookbooks/codex", icon: faRobot, description: "Run Codex." },
					{ title: "OpenCode Agent", href: "/cookbooks/opencode", icon: faRobot, description: "Run OpenCode." },
					{ title: "Agent to Agent", href: "/cookbooks/agent-to-agent", icon: faArrowsLeftRight, description: "Agents calling agents." },
				],
			},
			{
				title: "Code Execution",
				items: [
					{ title: "AI Agent Code Exec", href: "/cookbooks/js-ai-agent-code-exec", icon: faRobot, description: "Execute generated code with explicit limits." },
					{ title: "Code Mode", href: "/cookbooks/js-code-mode", icon: faTerminal, description: "Let generated code orchestrate narrow bindings." },
					{ title: "Dev Servers", href: "/cookbooks/js-dev-servers", icon: faGlobe, description: "Run and drive a long-lived guest server." },
					{ title: "Plugin Systems", href: "/cookbooks/js-plugin-systems", icon: faWrench, description: "Evaluate plugins inside a restricted VM." },
				],
			},
			{
				title: "Sessions & Permissions",
				items: [
					{ title: "Sessions", href: "/cookbooks/sessions", icon: faMessages, description: "Session lifecycle." },
					{ title: "Permissions", href: "/cookbooks/permissions", icon: faKey, description: "Permission policies." },
					{ title: "Approvals", href: "/cookbooks/approvals", icon: faCheck, description: "Approval gating." },
					{ title: "Authentication", href: "/cookbooks/authentication", icon: faKey, description: "Authenticating callers." },
					{ title: "Multiplayer", href: "/cookbooks/multiplayer", icon: faTowerBroadcast, description: "Realtime multiplayer." },
					{ title: "Persistence", href: "/cookbooks/persistence", icon: faHardDrive, description: "Persist and resume." },
				],
			},
			{
				title: "Orchestration",
				items: [
					{ title: "Cron", href: "/cookbooks/cron", icon: faClock, description: "Scheduled jobs." },
					{ title: "Workflows", href: "/cookbooks/workflows", icon: faDiagramNext, description: "Durable workflows." },
				],
			},
			{
				title: "Reference",
				items: [
					{ title: "Core", href: "/cookbooks/core", icon: faBook, description: "Core SDK usage." },
					{ title: "Software", href: "/cookbooks/software", icon: faDownload, description: "Custom software." },
					{ title: "Bindings", href: "/cookbooks/bindings", icon: faWrench, description: "Host bindings." },
					{ title: "Resource Limits", href: "/cookbooks/resource-limits", icon: faGauge, description: "Limits in practice." },
					{ title: "Sandbox", href: "/cookbooks/sandbox", icon: faHardDrive, description: "Sandbox mounting." },
				],
			},
		],
	},
};
