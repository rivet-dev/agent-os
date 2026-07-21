import { execFileSync } from "node:child_process";
import {
	existsSync,
	mkdtempSync,
	mkdirSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { resolveNodeRuntimeCommandsDir } from "@rivet-dev/agentos-runtime-core";
import type { CommandBenchmarkOp } from "../lib/layers.js";
import { nowMs } from "../lib/perf-utils.js";
import type { BenchVm } from "../lib/vm.js";

const ECOSYSTEM_SAMPLE_CAP = {
	maxIterations: 5,
	maxWarmup: 1,
};
const HEAVY_ECOSYSTEM_SAMPLE_CAP = {
	maxIterations: 1,
	maxWarmup: 0,
};
const SMALL_TEXT_BYTES = 4 * 1024;
const BIG_TEXT_BYTES = 1024 * 1024;
const BIG_GZIP_BYTES = 64 * 1024;
const SMALL_TREE_FILES = 32;
const BIG_TREE_FILES = 1000;
const BIG_ARCHIVE_FILES = 128;
const VM_ROOT = "/tmp/ecosystem";
const REQUIRED_WASM_COMMANDS = [
	"sh",
	"cat",
	"ls",
	"echo",
	"grep",
	"sed",
	"find",
	"tar",
	"gzip",
	"jq",
	"git",
	"diff",
	"wc",
	"cp",
	"rm",
	"mkdir",
] as const;

export function ecosystemWasmCommandDirs(): string[] {
	const commandsDir = resolveNodeRuntimeCommandsDir();
	assertWasmCommandsPresent(commandsDir, requiredWasmCommands());
	return [commandsDir];
}

export const ecosystemFamily: CommandBenchmarkOp[] = [
	ls100Op(),
	shPipelineOp(),
	cdTmpPwdOp(),
	echoHelloOp(),
	catOp("cat_small", SMALL_TEXT_BYTES),
	catOp("cat_big", BIG_TEXT_BYTES),
	grepTreeOp("grep_small", SMALL_TREE_FILES),
	grepTreeOp("grep_big", BIG_TREE_FILES),
	sedSubstitutionOp(),
	findTreeOp(),
	tarTreeOp("tar_small", SMALL_TREE_FILES),
	tarTreeOp("tar_big", BIG_ARCHIVE_FILES),
	gzipRoundTripOp("gzip_small", SMALL_TEXT_BYTES),
	gzipRoundTripOp("gzip_big", BIG_GZIP_BYTES),
	jqExtractOp(),
	gitInitCommitOp(),
];

function ls100Op(): CommandBenchmarkOp {
	return {
		family: "ecosystem",
		name: "ls_100",
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:46",
		reproducer: "ls -1 over a pre-created 100-file directory",
		runHostCmd: (iters, warmup) => {
			const dir = createHostDirWithFiles("agentos-ecosystem-ls-", 100);
			try {
				return sampleSyncCommand(iters, warmup, () => {
					const stdout = execFileSync("ls", ["-1", dir], { encoding: "utf8" });
					assertLineCount(stdout, 100, "host ls_100");
				});
			} finally {
				rmSync(dir, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const dir = `${VM_ROOT}/ls-100`;
			await setupVmDirWithFiles(vm, dir, 100);
			return sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand("ls", ["-1", dir]);
				assertExit(result, "vm ls_100");
				assertLineCount(result.stdout, 100, "vm ls_100");
			});
		},
	};
}

function shPipelineOp(): CommandBenchmarkOp {
	return {
		family: "ecosystem",
		name: "sh_pipeline",
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:78",
		reproducer: 'sh -c "ls -1 | grep -c ." over a 100-file directory',
		runHostCmd: (iters, warmup) => {
			const dir = createHostDirWithFiles(
				"agentos-ecosystem-pipeline-",
				100,
			);
			try {
				return sampleSyncCommand(iters, warmup, () => {
					const stdout = execFileSync("sh", ["-c", "ls -1 | grep -c ."], {
						cwd: dir,
						encoding: "utf8",
					});
					assertCount(stdout, 100, "host sh_pipeline");
				});
			} finally {
				rmSync(dir, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const dir = `${VM_ROOT}/sh-pipeline`;
			await setupVmDirWithFiles(vm, dir, 100);
			return sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand(
					"sh",
					["-c", "ls -1 | grep -c ."],
					{ cwd: dir },
				);
				assertExit(result, "vm sh_pipeline");
				assertCount(result.stdout, 100, "vm sh_pipeline");
			});
		},
	};
}

function cdTmpPwdOp(): CommandBenchmarkOp {
	return {
		family: "ecosystem",
		name: "cd_tmp_pwd",
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:114",
		reproducer: 'sh -c "cd /tmp && pwd"',
		runHostCmd: (iters, warmup) =>
			sampleSyncCommand(iters, warmup, () => {
				const stdout = execFileSync("sh", ["-c", "cd /tmp && pwd"], {
					encoding: "utf8",
				});
				assertExact(stdout.trim(), "/tmp", "host cd_tmp_pwd");
			}),
		runVmCmd: async (vm, iters, warmup) =>
			sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand("sh", ["-c", "cd /tmp && pwd"]);
				assertExit(result, "vm cd_tmp_pwd");
				assertExact(result.stdout.trim(), "/tmp", "vm cd_tmp_pwd");
			}),
	};
}

function echoHelloOp(): CommandBenchmarkOp {
	return {
		family: "ecosystem",
		name: "echo_hello",
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:140",
		reproducer: "echo agentos-ecosystem",
		runHostCmd: (iters, warmup) =>
			sampleSyncCommand(iters, warmup, () => {
				const stdout = execFileSync("echo", ["agentos-ecosystem"], {
					encoding: "utf8",
				});
				assertExact(stdout, "agentos-ecosystem\n", "host echo_hello");
			}),
		runVmCmd: async (vm, iters, warmup) =>
			sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand("echo", [
					"agentos-ecosystem",
				]);
				assertExit(result, "vm echo_hello");
				assertExact(result.stdout, "agentos-ecosystem\n", "vm echo_hello");
			}),
	};
}

function catOp(name: string, sizeBytes: number): CommandBenchmarkOp {
	const expected = textPayload(sizeBytes);
	return {
		family: "ecosystem",
		name,
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:165",
		reproducer: `cat a pre-created ${sizeBytes} byte text fixture and verify exact stdout`,
		runHostCmd: (iters, warmup) => {
			const dir = mkdtempSync(join(tmpdir(), `agentos-ecosystem-${name}-`));
			const file = join(dir, "fixture.txt");
			writeFileSync(file, expected);
			try {
				return sampleSyncCommand(iters, warmup, () => {
					const stdout = execFileSync("cat", [file], {
						encoding: "utf8",
						maxBuffer: sizeBytes + 1024,
					});
					assertExact(stdout, expected, `host ${name}`);
				});
			} finally {
				rmSync(dir, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const file = `${VM_ROOT}/${name}.txt`;
			await ensureVmRoot(vm);
			await vm.writeFile(file, expected);
			return sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand("cat", [file]);
				assertExit(result, `vm ${name}`);
				assertExact(result.stdout, expected, `vm ${name}`);
			});
		},
	};
}

function grepTreeOp(name: string, fileCount: number): CommandBenchmarkOp {
	return {
		family: "ecosystem",
		name,
		...(fileCount >= BIG_TREE_FILES
			? HEAVY_ECOSYSTEM_SAMPLE_CAP
			: ECOSYSTEM_SAMPLE_CAP),
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:204",
		reproducer: `grep -l needle over ${fileCount} files and verify match count`,
		runHostCmd: (iters, warmup) => {
			const dir = createHostDirWithFiles(
				`agentos-ecosystem-${name}-`,
				fileCount,
				(i) => grepFileContent(i),
			);
			const files = numberedTextFiles(dir, fileCount);
			try {
				return sampleSyncCommand(iters, warmup, () => {
					const stdout = execFileSync("grep", ["-l", "needle", ...files], {
						encoding: "utf8",
						maxBuffer: fileCount * 256,
					});
					assertLineCount(stdout, fileCount, `host ${name}`);
				});
			} finally {
				rmSync(dir, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const dir = `${VM_ROOT}/${name}`;
			await setupVmDirWithFiles(vm, dir, fileCount, (i) => grepFileContent(i));
			const files = numberedTextFiles(dir, fileCount);
			return sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand("grep", [
					"-l",
					"needle",
					...files,
				]);
				assertExit(result, `vm ${name}`);
				assertLineCount(result.stdout, fileCount, `vm ${name}`);
			});
		},
	};
}

function sedSubstitutionOp(): CommandBenchmarkOp {
	const input = "alpha beta alpha\nplain alpha\n";
	const expected = "omega beta omega\nplain omega\n";
	return {
		family: "ecosystem",
		name: "sed_substitution",
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:248",
		reproducer: "sed replaces every alpha token in a known fixture",
		runHostCmd: (iters, warmup) => {
			const dir = mkdtempSync(join(tmpdir(), "agentos-ecosystem-sed-"));
			const file = join(dir, "fixture.txt");
			writeFileSync(file, input);
			try {
				return sampleSyncCommand(iters, warmup, () => {
					const stdout = execFileSync("sed", ["s/alpha/omega/g", file], {
						encoding: "utf8",
					});
					assertExact(stdout, expected, "host sed_substitution");
				});
			} finally {
				rmSync(dir, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const file = `${VM_ROOT}/sed-fixture.txt`;
			await ensureVmRoot(vm);
			await vm.writeFile(file, input);
			return sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand("sed", [
					"s/alpha/omega/g",
					file,
				]);
				assertExit(result, "vm sed_substitution");
				assertExact(result.stdout, expected, "vm sed_substitution");
			});
		},
	};
}

function findTreeOp(): CommandBenchmarkOp {
	return {
		family: "ecosystem",
		name: "find_1000",
		...HEAVY_ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:286",
		reproducer: "find a 1000-file tree and verify the file count",
		runHostCmd: (iters, warmup) => {
			const dir = createHostDirWithFiles(
				"agentos-ecosystem-find-",
				BIG_TREE_FILES,
			);
			try {
				return sampleSyncCommand(iters, warmup, () => {
					const stdout = execFileSync(
						"find",
						[dir, "-type", "f", "-name", "*.txt"],
						{
							encoding: "utf8",
							maxBuffer: BIG_TREE_FILES * 256,
						},
					);
					assertLineCount(stdout, BIG_TREE_FILES, "host find_1000");
				});
			} finally {
				rmSync(dir, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const dir = `${VM_ROOT}/find-1000`;
			await setupVmDirWithFiles(vm, dir, BIG_TREE_FILES);
			return sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand("find", [
					dir,
					"-type",
					"f",
					"-name",
					"*.txt",
				]);
				assertExit(result, "vm find_1000");
				assertLineCount(result.stdout, BIG_TREE_FILES, "vm find_1000");
			});
		},
	};
}

function tarTreeOp(name: string, fileCount: number): CommandBenchmarkOp {
	return {
		family: "ecosystem",
		name,
		...(fileCount > SMALL_TREE_FILES
			? HEAVY_ECOSYSTEM_SAMPLE_CAP
			: ECOSYSTEM_SAMPLE_CAP),
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:327",
		reproducer: `tar create/extract/list a ${fileCount}-file tree and diff a sentinel file`,
		runHostCmd: (iters, warmup) => {
			const src = createHostDirWithFiles(
				`agentos-ecosystem-${name}-`,
				fileCount,
			);
			writeFileSync(join(src, "sentinel.txt"), sentinelContent(fileCount));
			const work = mkdtempSync(
				join(tmpdir(), `agentos-ecosystem-${name}-work-`),
			);
			try {
				return sampleSyncCommand(iters, warmup, (i) => {
					const archive = join(work, `archive-${i}.tar`);
					const out = join(work, `out-${i}`);
					const stdout = execFileSync(
						"sh",
						["-c", tarRoundTripScript(src, archive, out)],
						{
							encoding: "utf8",
							maxBuffer: (fileCount + 8) * 128,
						},
					);
					assertTarListing(stdout, fileCount + 1, `host ${name}`);
				});
			} finally {
				rmSync(src, { recursive: true, force: true });
				rmSync(work, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const src = `${VM_ROOT}/${name}-src`;
			await setupVmDirWithFiles(vm, src, fileCount);
			await vm.writeFile(`${src}/sentinel.txt`, sentinelContent(fileCount));
			return sampleAsyncCommand(iters, warmup, async (i) => {
				const archive = `${VM_ROOT}/${name}-${i}.tar`;
				const out = `${VM_ROOT}/${name}-out-${i}`;
				const result = await vm.execWasmCommand("sh", [
					"-c",
					tarRoundTripScript(src, archive, out),
				]);
				assertExit(result, `vm ${name}`);
				assertTarListing(result.stdout, fileCount + 1, `vm ${name}`);
			});
		},
	};
}

function gzipRoundTripOp(name: string, sizeBytes: number): CommandBenchmarkOp {
	const content = textPayload(sizeBytes);
	return {
		family: "ecosystem",
		name,
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:381",
		reproducer: `gzip then gunzip a ${sizeBytes} byte fixture and diff the result`,
		runHostCmd: (iters, warmup) => {
			const dir = mkdtempSync(join(tmpdir(), `agentos-ecosystem-${name}-`));
			const src = join(dir, "fixture.txt");
			writeFileSync(src, content);
			try {
				return sampleSyncCommand(iters, warmup, (i) => {
					const stdout = execFileSync(
						"sh",
						["-c", gzipRoundTripScript(src, join(dir, `work-${i}.txt`))],
						{ encoding: "utf8" },
					);
					assertCount(stdout, sizeBytes, `host ${name}`);
				});
			} finally {
				rmSync(dir, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const src = `${VM_ROOT}/${name}.txt`;
			await ensureVmRoot(vm);
			await vm.writeFile(src, content);
			return sampleAsyncCommand(iters, warmup, async (i) => {
				const result = await vm.execWasmCommand("sh", [
					"-c",
					gzipRoundTripScript(src, `${VM_ROOT}/${name}-work-${i}.txt`),
				]);
				assertExit(result, `vm ${name}`);
				assertCount(result.stdout, sizeBytes, `vm ${name}`);
			});
		},
	};
}

function jqExtractOp(): CommandBenchmarkOp {
	const input =
		JSON.stringify({
			items: [
				{ name: "control", value: 7 },
				{ name: "target", value: 42 },
			],
		}) + "\n";
	return {
		family: "ecosystem",
		name: "jq_extract",
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:428",
		reproducer: "jq extracts a known field from a JSON fixture",
		runHostCmd: (iters, warmup) => {
			const dir = mkdtempSync(join(tmpdir(), "agentos-ecosystem-jq-"));
			const file = join(dir, "fixture.json");
			writeFileSync(file, input);
			try {
				return sampleSyncCommand(iters, warmup, () => {
					const stdout = execFileSync("sh", ["-c", jqExtractScript(file)], {
						encoding: "utf8",
					});
					assertExact(stdout, "42\n", "host jq_extract");
				});
			} finally {
				rmSync(dir, { recursive: true, force: true });
			}
		},
		runVmCmd: async (vm, iters, warmup) => {
			const file = `${VM_ROOT}/jq-fixture.json`;
			await ensureVmRoot(vm);
			await vm.writeFile(file, input);
			return sampleAsyncCommand(iters, warmup, async () => {
				const result = await vm.execWasmCommand("sh", [
					"-c",
					jqExtractScript(file),
				]);
				assertExit(result, "vm jq_extract");
				assertExact(result.stdout, "42\n", "vm jq_extract");
			});
		},
	};
}

function gitInitCommitOp(): CommandBenchmarkOp {
	return {
		family: "ecosystem",
		name: "git_init_commit",
		...ECOSYSTEM_SAMPLE_CAP,
		fileLine: "packages/benchmarks/src/families/ecosystem.ts:469",
		reproducer: "git init, add ten files, commit, and rev-parse HEAD",
		runHostCmd: (iters, warmup) =>
			sampleSyncCommand(iters, warmup, () => {
				const dir = createHostDirWithFiles("agentos-ecosystem-git-", 10);
				try {
					const stdout = execFileSync("sh", ["-c", gitScript()], {
						cwd: dir,
						encoding: "utf8",
						env: gitEnv(),
					});
					assertSha(stdout, "host git_init_commit");
				} finally {
					rmSync(dir, { recursive: true, force: true });
				}
			}),
		runVmCmd: async (vm, iters, warmup) =>
			sampleAsyncCommand(iters, warmup, async (i) => {
				const dir = `${VM_ROOT}/git-init-commit-${i}`;
				await setupVmDirWithFiles(vm, dir, 10);
				const result = await vm.execWasmCommand("sh", ["-c", gitScript()], {
					cwd: dir,
					env: gitEnv(),
				});
				assertExit(result, "vm git_init_commit");
				assertSha(result.stdout, "vm git_init_commit");
			}),
	};
}

function requiredWasmCommands(): readonly string[] {
	const override = process.env.BENCH_REQUIRED_WASM_COMMANDS;
	if (override === undefined) return REQUIRED_WASM_COMMANDS;
	return override
		.split(",")
		.map((command) => command.trim())
		.filter(Boolean);
}

function assertWasmCommandsPresent(
	commandsDir: string,
	requiredCommands: readonly string[],
): void {
	const missing = requiredCommands.filter(
		(command) => !existsSync(join(commandsDir, command)),
	);
	if (missing.length > 0) {
		throw new Error(
			`ecosystem WASM command(s) missing from ${commandsDir}: ${missing.join(", ")}`,
		);
	}
}

function gitEnv(): Record<string, string> {
	const env: Record<string, string> = {};
	for (const [key, value] of Object.entries(process.env)) {
		if (value !== undefined) env[key] = value;
	}
	return {
		...env,
		GIT_AUTHOR_NAME: "Bench",
		GIT_AUTHOR_EMAIL: "bench@example.com",
		GIT_COMMITTER_NAME: "Bench",
		GIT_COMMITTER_EMAIL: "bench@example.com",
	};
}

function gitScript(): string {
	return [
		"git init -q",
		"git add .",
		"git commit -q -m init",
		"git rev-parse HEAD",
	].join(" && ");
}

function createHostDirWithFiles(
	prefix: string,
	fileCount: number,
	content: (index: number) => string = () => "x\n",
): string {
	const dir = mkdtempSync(join(tmpdir(), prefix));
	mkdirSync(dir, { recursive: true });
	for (let i = 0; i < fileCount; i++) {
		writeFileSync(
			join(dir, `file-${String(i).padStart(4, "0")}.txt`),
			content(i),
		);
	}
	return dir;
}

function numberedTextFiles(dir: string, fileCount: number): string[] {
	const files: string[] = [];
	for (let i = 0; i < fileCount; i++) {
		files.push(`${dir}/file-${String(i).padStart(4, "0")}.txt`);
	}
	return files;
}

async function setupVmDirWithFiles(
	vm: Pick<BenchVm, "exec" | "writeFile">,
	dir: string,
	fileCount: number,
	content: (index: number) => string = () => "x\n",
): Promise<void> {
	await vm.exec(`rm -rf ${dir} && mkdir -p ${dir}`);
	for (let i = 0; i < fileCount; i++) {
		await vm.writeFile(
			`${dir}/file-${String(i).padStart(4, "0")}.txt`,
			content(i),
		);
	}
}

async function ensureVmRoot(vm: Pick<BenchVm, "exec">): Promise<void> {
	await vm.exec(`mkdir -p ${VM_ROOT}`);
}

function textPayload(sizeBytes: number): string {
	const chunk = "agentos ecosystem payload line 0123456789abcdef\n";
	let output = "";
	while (output.length < sizeBytes) output += chunk;
	return output.slice(0, sizeBytes);
}

function grepFileContent(index: number): string {
	return (
		[
			`plain prefix ${index}`,
			`needle line ${index}`,
			`plain suffix ${index}`,
		].join("\n") + "\n"
	);
}

function sentinelContent(fileCount: number): string {
	return `agentos tar sentinel ${fileCount}\n`;
}

function tarRoundTripScript(src: string, archive: string, out: string): string {
	return [
		`rm -rf ${shellQuote(out)} ${shellQuote(archive)}`,
		`mkdir -p ${shellQuote(out)}`,
		`tar -cf ${shellQuote(archive)} -C ${shellQuote(src)} .`,
		`tar -xf ${shellQuote(archive)} -C ${shellQuote(out)}`,
		`diff ${shellQuote(join(src, "sentinel.txt"))} ${shellQuote(join(out, "sentinel.txt"))} >/dev/null`,
		`tar -tf ${shellQuote(archive)}`,
	].join(" && ");
}

function gzipRoundTripScript(src: string, work: string): string {
	return [
		`rm -f ${shellQuote(work)} ${shellQuote(`${work}.gz`)}`,
		`cp ${shellQuote(src)} ${shellQuote(work)}`,
		`gzip -f ${shellQuote(work)}`,
		`rm -f ${shellQuote(work)}`,
		`gzip -df ${shellQuote(`${work}.gz`)}`,
		`diff ${shellQuote(src)} ${shellQuote(work)} >/dev/null`,
		`wc -c < ${shellQuote(work)}`,
	].join(" && ");
}

function jqExtractScript(file: string): string {
	return `jq -r ${shellQuote('.items[] | select(.name == "target") | .value')} < ${shellQuote(file)}`;
}

function shellQuote(value: string): string {
	return `'${value.replace(/'/g, "'\\''")}'`;
}

function sampleSyncCommand(
	iters: number,
	warmup: number,
	fn: (index: number) => void,
): number[] {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		fn(i);
		const ms = nowMs(start);
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function sampleAsyncCommand(
	iters: number,
	warmup: number,
	fn: (index: number) => Promise<void>,
): Promise<number[]> {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		await fn(i);
		const ms = nowMs(start);
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

function assertExit(
	result: { exitCode: number; stderr: string },
	context: string,
): void {
	if (result.exitCode !== 0) {
		throw new Error(`${context} exited ${result.exitCode}\n${result.stderr}`);
	}
}

function assertLineCount(
	stdout: string,
	expected: number,
	context: string,
): void {
	const actual = stdout.trim().split("\n").filter(Boolean).length;
	if (actual !== expected) {
		throw new Error(
			`${context} returned ${actual} line(s), expected ${expected}`,
		);
	}
}

function assertTarListing(
	stdout: string,
	expectedFiles: number,
	context: string,
): void {
	const files = stdout
		.trim()
		.split("\n")
		.filter(
			(line) => line && line !== "." && line !== "./" && !line.endsWith("/"),
		);
	const hasSentinel = files.some(
		(line) => line === "./sentinel.txt" || line === "sentinel.txt",
	);
	if (!hasSentinel || files.length !== expectedFiles) {
		throw new Error(
			`${context} tar listing had ${files.length} file(s), sentinel=${hasSentinel}, expected ${expectedFiles}`,
		);
	}
}

function assertCount(stdout: string, expected: number, context: string): void {
	const actual = Number(stdout.trim());
	if (actual !== expected) {
		throw new Error(`${context} count ${actual}, expected ${expected}`);
	}
}

function assertExact(actual: string, expected: string, context: string): void {
	if (actual !== expected) {
		throw new Error(
			`${context} output ${JSON.stringify(actual)}, expected ${JSON.stringify(expected)}`,
		);
	}
}

function assertSha(stdout: string, context: string): void {
	if (!/^[0-9a-f]{40}$/i.test(stdout.trim())) {
		throw new Error(`${context} did not emit a commit sha: ${stdout}`);
	}
}
