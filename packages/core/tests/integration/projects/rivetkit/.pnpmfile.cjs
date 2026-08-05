const PINNED_RIVETKIT_VERSION = "0.0.0-sqlite-uds.4e59a38";

module.exports = {
	hooks: {
		readPackage(pkg) {
			if (pkg.name === "rivetkit" && pkg.version === PINNED_RIVETKIT_VERSION) {
				delete pkg.dependencies?.["@rivet-dev/agent-os-core"];
			}
			return pkg;
		},
	},
};
