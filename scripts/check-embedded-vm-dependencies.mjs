import { execFileSync } from "node:child_process";

const tree = execFileSync(
	"cargo",
	[
		"tree",
		"-p",
		"agentos-example-embedded-vm",
		"--edges",
		"normal,build",
		"--prefix",
		"none",
	],
	{ encoding: "utf8" },
);

const packages = new Set(
	tree
		.split("\n")
		.map((line) => line.trim().split(/\s+v\d/)[0])
		.filter(Boolean),
);

const forbidden = [
	"agentos-driver-tokio",
	"agentos-executor-contract",
	"agentos-executor-v8-runtime",
	"agentos-executor-wasm-abi",
	"agentos-rivetkit-ars-client",
	"agentos-sidecar-protocol",
	"agentos-vfs-storage",
	"agentos-vm-config",
	"aes",
	"aes-gcm",
	"aws-config",
	"aws-credential-types",
	"aws-sdk-s3",
	"ctr",
	"hmac",
	"jsonwebtoken",
	"md-5",
	"memmap2",
	"openssl",
	"oxc_allocator",
	"oxc_ast",
	"oxc_codegen",
	"oxc_parser",
	"oxc_semantic",
	"oxc_span",
	"oxc_transformer",
	"pbkdf2",
	"rusqlite",
	"rivet-vbare-compiler",
	"rivet-vbare-gen",
	"rustls",
	"rustls-pemfile",
	"scrypt",
	"sha1",
	"sha2",
	"tar",
	"tokio",
	"tokio-rustls",
	"ureq",
	"vbare",
	"wasmparser",
	"wasmtime",
];

const violations = forbidden.filter((name) => packages.has(name));
if (violations.length > 0) {
	throw new Error(
		`executor-free embedded VM pulled forbidden dependencies:\n${violations
			.map((name) => `- ${name}`)
			.join("\n")}`,
	);
}

console.log(
	`embedded VM dependency boundary: OK (${packages.size} packages, ${forbidden.length} forbidden packages absent)`,
);
