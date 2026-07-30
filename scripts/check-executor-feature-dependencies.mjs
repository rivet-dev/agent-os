import { execFileSync } from "node:child_process";

function packagesFor(feature) {
	const args = [
		"tree",
		"-p",
		"agentos-sidecar",
		"--no-default-features",
		"--edges",
		"normal",
		"--prefix",
		"none",
	];
	if (feature) args.push("--features", feature);
	const tree = execFileSync("cargo", args, { encoding: "utf8" });
	return new Set(
		tree
			.split("\n")
			.map((line) => line.trim().split(/\s+v\d/)[0])
			.filter(Boolean),
	);
}

function assertGraph(name, packages, { present = [], absent = [] }) {
	const missing = present.filter((dependency) => !packages.has(dependency));
	const unexpected = absent.filter((dependency) => packages.has(dependency));
	if (missing.length || unexpected.length) {
		throw new Error(
			[
				`${name} dependency graph is incorrect`,
				...missing.map((dependency) => `- missing: ${dependency}`),
				...unexpected.map((dependency) => `- unexpected: ${dependency}`),
			].join("\n"),
		);
	}
}

const concreteExecutors = [
	"agentos-executor-node-v8",
	"agentos-executor-python-v8-pyodide",
	"agentos-executor-wasm-v8",
	"agentos-executor-wasm-wasmtime",
];

assertGraph("no-executor sidecar", packagesFor(), {
	absent: [
		...concreteExecutors,
		"agentos-executor-wasm-abi",
		"oxc_parser",
	],
});
assertGraph("node-v8", packagesFor("node-v8"), {
	present: ["agentos-executor-node-v8", "oxc_parser"],
	absent: [
		"agentos-executor-python-v8-pyodide",
		"agentos-executor-wasm-abi",
		"agentos-executor-wasm-v8",
		"agentos-executor-wasm-wasmtime",
	],
});
assertGraph("python-v8-pyodide", packagesFor("python-v8-pyodide"), {
	present: ["agentos-executor-python-v8-pyodide"],
	absent: [
		"agentos-executor-node-v8",
		"agentos-executor-wasm-abi",
		"agentos-executor-wasm-v8",
		"agentos-executor-wasm-wasmtime",
		"oxc_parser",
	],
});
assertGraph("wasm-v8", packagesFor("wasm-v8"), {
	present: ["agentos-executor-wasm-abi", "agentos-executor-wasm-v8"],
	absent: [
		"agentos-executor-node-v8",
		"agentos-executor-python-v8-pyodide",
		"agentos-executor-wasm-wasmtime",
		"oxc_parser",
	],
});
assertGraph("wasm-wasmtime", packagesFor("wasm-wasmtime"), {
	present: ["agentos-executor-wasm-abi", "agentos-executor-wasm-wasmtime"],
	absent: [
		"agentos-executor-node-v8",
		"agentos-executor-python-v8-pyodide",
		"agentos-executor-v8-runtime",
		"agentos-executor-wasm-v8",
		"oxc_parser",
	],
});

console.log("executor feature dependency matrix: OK");
