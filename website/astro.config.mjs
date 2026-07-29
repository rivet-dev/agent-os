import { defineConfig } from "astro/config";
import tailwind from "@astrojs/tailwind";
import sitemap from "@astrojs/sitemap";
import { docsTheme } from "@rivet-dev/docs-theme";
import { siteConfig } from "./docs.config.mjs";

// https://astro.build/config
export default defineConfig({
	site: "https://agentos-sdk.dev",
	output: "static",
	integrations: [
		// The shared Rivet docs framework (copied 1:1 from rivet.dev, no Starlight).
		// docsTheme() provides react + mdx + the remark/rehype/Shiki pipeline +
		// route generation + the virtual config. docs.config.mjs maps agentOS's
		// identity/nav/sitemap onto it.
		...docsTheme(siteConfig),
		tailwind({ applyBaseStyles: false }),
		sitemap(),
	],
	vite: {
		server: {
			// Amp orbs can expose the dev server through an E2B portal hostname.
			allowedHosts: [".e2b.app"],
		},
		resolve: {
			dedupe: ["react", "react-dom", "react/jsx-runtime", "react/jsx-dev-runtime"],
		},
		optimizeDeps: {
			// Force EVERY React island dependency into one optimize pass at
			// startup. Otherwise Vite discovers a dep lazily on first request
			// (seen: "new dependencies optimized: @rivet-gg/components ...
			// reloading"), reloads mid-render, and react-dom ends up referencing a
			// React instance whose hook dispatcher was reset → "Invalid hook call"
			// → islands never hydrate. The split is in the optimizer, not on disk.
			// Ref: withastro/astro#16766. Keep this list in sync with the libs the
			// theme + vendored components actually import in island code.
			include: [
				"react",
				"react-dom",
				"react-dom/client",
				"react/jsx-runtime",
				"react/jsx-dev-runtime",
				"@rivet-gg/components",
				"@rivet-gg/icons",
				"framer-motion",
				"lucide-react",
				"posthog-js",
				"posthog-js/react",
				"@sentry/react",
				"@headlessui/react",
				"@floating-ui/react",
				"react-hook-form",
				"react-markdown",
				"react-day-picker",
				"react-resizable-panels",
				// CJS island deps that fail ESM interop unless pre-bundled (the
				// actual hydration blocker — "does not provide an export"). They're
				// imported from INSIDE the theme package, so use Vite's
				// `linked-pkg > nested-dep` syntax to force them through optimize.
				"typesense",
				"use-sync-external-store",
				"use-sync-external-store/shim",
				"use-sync-external-store/with-selector",
				"@rivet-dev/docs-theme > typesense",
				"@rivet-dev/docs-theme > use-sync-external-store",
				"@rivet-dev/docs-theme > use-sync-external-store/with-selector",
				"@rivet-gg/components > use-sync-external-store",
				"@rivet-gg/components > use-sync-external-store/with-selector",
				"@radix-ui/react-accordion",
				"@radix-ui/react-avatar",
				"@radix-ui/react-checkbox",
				"@radix-ui/react-context-menu",
				"@radix-ui/react-dialog",
				"@radix-ui/react-dropdown-menu",
				"@radix-ui/react-label",
				"@radix-ui/react-popover",
				"@radix-ui/react-progress",
				"@radix-ui/react-radio-group",
				"@radix-ui/react-scroll-area",
				"@radix-ui/react-select",
				"@radix-ui/react-separator",
				"@radix-ui/react-slider",
				"@radix-ui/react-slot",
				"@radix-ui/react-switch",
				"@radix-ui/react-tabs",
				"@radix-ui/react-toggle",
				"@radix-ui/react-toggle-group",
				"@radix-ui/react-tooltip",
				"@radix-ui/react-visually-hidden",
			],
		},
		ssr: {
			// The theme/components ship .tsx, so they must be bundled for SSR —
			// but keep React external so the bundled theme and react-dom/server
			// share one instance (clears the SSR-side "Invalid hook call" noise).
			external: ["react", "react-dom", "react/jsx-runtime", "react/jsx-dev-runtime"],
			noExternal: [
				"@rivet-dev/docs-theme",
				"@rivet-gg/components",
				"@rivet-gg/icons",
			],
		},
	},
});
