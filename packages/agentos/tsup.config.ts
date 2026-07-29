import { defineConfig } from "tsup";

export default defineConfig({
	format: ["esm"],
	dts: true,
	sourcemap: true,
	clean: true,
	// Keep RivetKit and React out of the bundle so subpath conditions resolve in
	// the consumer's environment.
	external: ["rivetkit", "@rivetkit/react", "react", "react-dom"],
});
