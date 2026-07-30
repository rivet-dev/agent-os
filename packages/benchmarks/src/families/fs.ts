import type { BenchmarkOp } from "../lib/layers.js";

function fsWriteOp(name: string, sizeBytes: number): BenchmarkOp {
	return {
		family: "fs",
		name,
		nativeOp: "fs_write",
		nativeArgs: ["--size-bytes", String(sizeBytes)],
		fileLine: "crates/vm-kernel/src/kernel.rs:1930",
		reproducer: `node fs.writeFileSync('/tmp/fuzz-perf-write.txt', ${sizeBytes} byte payload)`,
	program: `async (i) => {
  const fs = await import("node:fs");
  fs.writeFileSync("/tmp/fuzz-perf-write.txt", Buffer.alloc(${sizeBytes}, i & 255));
}`,
	};
}

function fsReadOp(name: string, sizeBytes: number): BenchmarkOp {
	return {
		family: "fs",
		name,
		nativeOp: "fs_read",
		nativeArgs: ["--size-bytes", String(sizeBytes)],
		fileLine: "crates/vm-kernel/src/mount_table.rs:814",
		reproducer: `node fs.readFileSync('/tmp/fuzz-perf-read-${sizeBytes}.bin')`,
		program: `async () => {
  const fs = await import("node:fs");
  const path = "/tmp/fuzz-perf-read-${sizeBytes}.bin";
  if (!fs.existsSync(path)) fs.writeFileSync(path, Buffer.alloc(${sizeBytes}, 7));
  const data = fs.readFileSync(path);
  if (data.length !== ${sizeBytes}) throw new Error("bad read: " + data.length);
}`,
	};
}

function readdirOp(name: string, entryCount: number): BenchmarkOp {
	return {
		family: "fs",
		name,
		nativeOp: "fs_readdir",
		nativeArgs: ["--entry-count", String(entryCount)],
		fileLine: "crates/vm-kernel/src/mount_table.rs:814",
		reproducer: `readdirSync over a ${entryCount}-entry VM directory`,
		setup: `async () => {
  const fs = await import("node:fs");
  const dir = "/tmp/fuzz-perf-readdir-${entryCount}";
  if (!fs.existsSync(dir)) fs.mkdirSync(dir);
  for (let i = 0; i < ${entryCount}; i++) {
    const path = dir + "/" + i + ".txt";
    if (!fs.existsSync(path)) fs.writeFileSync(path, "hi");
  }
}`,
		program: `async () => {
  const fs = await import("node:fs");
  const dir = "/tmp/fuzz-perf-readdir-${entryCount}";
  const entries = fs.readdirSync(dir);
  if (entries.length < ${entryCount}) throw new Error("short readdir: " + entries.length);
}`,
	};
}

function streamCopyOp(name: string, sizeBytes: number): BenchmarkOp {
	return {
		family: "fs",
		name,
		nativeOp: "stream_copy",
		nativeArgs: ["--size-bytes", String(sizeBytes)],
		fileLine: "crates/vm-kernel/src/mount_table.rs:814",
		reproducer: `stream pipeline copies one ${sizeBytes} byte file inside VM`,
		setup: `async () => {
  const fs = await import("node:fs");
  const src = "/tmp/fuzz-perf-stream-copy-src-${sizeBytes}.bin";
  if (!fs.existsSync(src)) fs.writeFileSync(src, Buffer.alloc(${sizeBytes}, 7));
}`,
		program: `async (i) => {
  const fs = await import("node:fs");
  const { pipeline } = await import("node:stream/promises");
  const src = "/tmp/fuzz-perf-stream-copy-src-${sizeBytes}.bin";
  const dst = "/tmp/fuzz-perf-stream-copy-dst-${sizeBytes}-" + i + ".bin";
  await pipeline(fs.createReadStream(src), fs.createWriteStream(dst));
  const stat = fs.statSync(dst);
  fs.unlinkSync(dst);
  if (stat.size !== ${sizeBytes}) throw new Error("bad stream copy: " + stat.size);
}`,
	};
}

export const fsFamily: BenchmarkOp[] = [
	{
		family: "fs",
		name: "open_close_churn",
		nativeOp: "fs_open_close",
		fileLine: "crates/vm-kernel/src/kernel.rs:1950",
		reproducer: "fs.openSync + fs.closeSync on a small fixture inside VM",
		program: `async () => {
  const fs = await import("node:fs");
  const path = "/tmp/fuzz-perf-open-close.txt";
  if (!fs.existsSync(path)) fs.writeFileSync(path, "hi");
  const fd = fs.openSync(path, "r");
  fs.closeSync(fd);
}`,
	},
	{
		family: "fs",
		name: "stat_storm",
		nativeOp: "fs_stat",
		fileLine: "crates/vm-kernel/src/kernel.rs:1950",
		reproducer: "node fs.statSync('/tmp/fuzz-perf-stat.txt') inside VM",
		program: `async (i) => {
  const fs = await import("node:fs");
  const path = "/tmp/fuzz-perf-stat.txt";
  if (!fs.existsSync(path)) fs.writeFileSync(path, "hi");
  fs.statSync(path);
}`,
	},
	fsWriteOp("fs_write_small", 4 * 1024),
	fsWriteOp("fs_write_big", 1024 * 1024),
	fsReadOp("fs_read_small", 4 * 1024),
	fsReadOp("fs_read_big", 1024 * 1024),
	{
		family: "fs",
		name: "mkdir_rmdir",
		nativeOp: "fs_mkdir_rmdir",
		fileLine: "crates/vm-kernel/src/mount_table.rs:814",
		reproducer: "fs.mkdirSync + fs.rmdirSync on a fresh VM path",
		program: `async (i) => {
  const fs = await import("node:fs");
  const path = "/tmp/fuzz-perf-dir-" + i + "-" + process.pid;
  fs.mkdirSync(path);
  fs.rmSync(path, { recursive: true });
}`,
	},
	{
		family: "fs",
		name: "rename_file",
		nativeOp: "fs_rename",
		fileLine: "crates/vm-kernel/src/mount_table.rs:814",
		reproducer: "write one file, rename it, then unlink",
		program: `async (i) => {
  const fs = await import("node:fs");
  const from = "/tmp/fuzz-perf-rename-" + i + ".a";
  const to = "/tmp/fuzz-perf-rename-" + i + ".b";
  fs.writeFileSync(from, "hi");
  fs.renameSync(from, to);
  fs.unlinkSync(to);
}`,
	},
	readdirOp("readdir_small", 32),
	readdirOp("readdir_big", 1000),
	{
		family: "fs",
		name: "fsync_small",
		nativeOp: "fs_fsync",
		fileLine: "crates/vm-kernel/src/kernel.rs:1930",
		reproducer: "fs.writeSync then fs.fsyncSync on a small file",
		program: `async () => {
  const fs = await import("node:fs");
  const fd = fs.openSync("/tmp/fuzz-perf-fsync.txt", "w");
  fs.writeSync(fd, "hello");
  fs.fsyncSync(fd);
  fs.closeSync(fd);
}`,
	},
	{
		family: "fs",
		name: "fs_promises_stat_x32",
		nativeOp: "fs_stat_x32",
		fileLine: "crates/vm-kernel/src/kernel.rs:1950",
		reproducer: "32 sequential fs.promises.stat calls on one VM file",
		setup: `async () => {
  const fs = await import("node:fs");
  const path = "/tmp/fuzz-perf-promises-stat.txt";
  if (!fs.existsSync(path)) fs.writeFileSync(path, "hi");
}`,
		program: `async () => {
  const fs = await import("node:fs");
  const path = "/tmp/fuzz-perf-promises-stat.txt";
  for (let k = 0; k < 32; k++) {
    await fs.promises.stat(path);
  }
}`,
	},
	streamCopyOp("stream_copy_small", 64 * 1024),
	streamCopyOp("stream_copy_big", 1024 * 1024),
];
