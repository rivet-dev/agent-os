const PINNED_RIVETKIT_VERSION = "0.0.0-sqlite-uds.4e59a38";

module.exports = {
	hooks: {
		readPackage(pkg) {
			if (pkg.name === "rivetkit" && pkg.version === PINNED_RIVETKIT_VERSION) {
				// This preview bundles its ./agent-os implementation but still declares
				// the retired external runtime. Keeping the declaration would restore the
				// sunset package graph even though no bundled RivetKit module imports it.
				delete pkg.dependencies?.["@rivet-dev/agent-os-core"];
			}
			return pkg;
		},
	},
};
