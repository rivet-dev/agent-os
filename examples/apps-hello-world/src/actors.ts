import { setup } from "@rivet-dev/agentos";
import { setupApps } from "@rivet-dev/agentos-apps";

const { appsActors } = setupApps();

export const registry = setup({
	use: {
		// These actors manage app deployments and scaling.
		...appsActors,
	},
});
