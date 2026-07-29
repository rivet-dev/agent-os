const projectDir = __dirname;

require("./next-wasm-shim.cjs");

const { nextBuild } = require("next/dist/cli/next-build");

async function main() {
	await nextBuild(
		{
			debug: false,
			experimentalAppOnly: false,
			experimentalDebugMemoryUsage: false,
			experimentalTurbo: false,
			lint: true,
			mangling: true,
			profile: false,
		},
		projectDir,
	);
}

module.exports = main;

if (require.main === module) {
	main().catch((error) => {
		console.error(error);
		process.exitCode = 1;
	});
}
