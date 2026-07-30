/**
 * Focused TCP loopback event-floor benchmark.
 *
 * Broad net rows mix connect/accept, polling cadence, write count, payload
 * bytes, read delivery, and close behavior. This benchmark exposes those axes
 * while preserving the same localhost server/client shape.
 */

import net from "node:net";
import type { SidecarProcess } from "@rivet-dev/agentos-core";
import { getHardware, printTable, round, stats } from "../lib/perf-utils.js";
import { type BenchVm, createBenchSidecar, createBenchVm } from "../lib/vm.js";

type Workload =
	| "connectClose"
	| "echoOnce"
	| "burstWritesEchoOnce"
	| "pingPong"
	| "concurrentConnect";
type ReplyMode = "none" | "echoOnceAfterAllBytes" | "echoEachWrite";
type WriteCadence = "none" | "burstSameTick" | "awaitReplyEachWrite";
type CostAxis =
	| "broad"
	| "connect_accept"
	| "payload_copy"
	| "write_count"
	| "event_cadence"
	| "concurrency";
type CompletionSemantics = "client_close" | "listener_close_after_accept";
type BroadEquivalent =
	| "tcp_echo_small"
	| "tcp_tiny_writes_16"
	| "tcp_echo_big"
	| "tcp_concurrent_4"
	| "tcp_connect_close";
type PayloadKind = "buffer" | "string";

interface NetTcpRow {
	id: string;
	workload: Workload;
	connections: number;
	writeCount: number;
	bytesPerWrite: number;
	replyMode: ReplyMode;
	writeCadence: WriteCadence;
	costAxis: CostAxis;
	compareAgainst?: string;
	completionSemantics: CompletionSemantics;
	broadMatrixEquivalent?: BroadEquivalent;
	payloadKind?: PayloadKind;
}

interface NetTcpRowResult extends NetTcpRow {
	totalBytes: number;
	expectedOps: {
		connectCalls: number;
		acceptEvents: number;
		writeCalls: number;
		expectedReadEvents: number;
		closeEvents: number;
	};
	node: ReturnType<typeof stats>;
	guest: ReturnType<typeof stats>;
	guestVsNodeRatio: number;
	guestMinusNodeMs: number;
	guestMsPerConnection: number;
	guestMsPerWrite?: number;
	guestMsPerKiB?: number;
	incrementalGuestMsFromSingleWrite?: number;
	compareAgainstGuestDeltaMs?: number;
	guestBridgeTrace?: Record<string, number>;
	sidecarNetTrace?: Record<string, number>;
	derivedGuestBridgeTrace?: {
		rawPollSentinels: number;
		readRawPerDataEvent?: number;
		acceptRawPerConnection?: number;
		writeRawPerUserWrite?: number;
		avgReadRawElapsedUs?: number;
		avgWriteRawElapsedUs?: number;
		avgReadBase64DecodeUs?: number;
		avgWriteBase64EncodeUs?: number;
		readBase64DecodeUsPerKiB?: number;
		writeBase64EncodeUsPerKiB?: number;
		avgReadPayloadMaterializeUs?: number;
		readPayloadMaterializeUsPerKiB?: number;
	};
	derivedSidecarNetTrace?: {
		kernelPollEmptyRatio?: number;
		socketReadDataHitRatio?: number;
		acceptConnectionHitRatio?: number;
		avgKernelPollElapsedUs?: number;
		avgKernelPollWaitUs?: number;
		avgSocketReadKernelUs?: number;
		avgSocketWriteKernelUs?: number;
		avgSocketReadRecordCloneUs?: number;
		avgSocketReadRecvCopyUs?: number;
		avgSocketReadRecvChunks?: number;
		socketReadKernelUsPerKiB?: number;
		socketWriteKernelUsPerKiB?: number;
		socketReadRecvCopyUsPerKiB?: number;
	};
	raw: {
		nodeMs: number[];
		guestMs: number[];
	};
}

function defaultRows(): NetTcpRow[] {
	return [
		{
			id: "connect_close_1",
			workload: "connectClose",
			connections: 1,
			writeCount: 0,
			bytesPerWrite: 0,
			replyMode: "none",
			writeCadence: "none",
			costAxis: "broad",
			completionSemantics: "client_close",
			broadMatrixEquivalent: "tcp_connect_close",
		},
		{
			id: "connect_close_4",
			workload: "connectClose",
			connections: 4,
			writeCount: 0,
			bytesPerWrite: 0,
			replyMode: "none",
			writeCadence: "none",
			costAxis: "connect_accept",
			compareAgainst: "connect_close_1",
			completionSemantics: "client_close",
		},
		{
			id: "connect_close_8",
			workload: "connectClose",
			connections: 8,
			writeCount: 0,
			bytesPerWrite: 0,
			replyMode: "none",
			writeCadence: "none",
			costAxis: "connect_accept",
			compareAgainst: "connect_close_1",
			completionSemantics: "client_close",
		},
		{
			id: "echo_1x5",
			workload: "echoOnce",
			connections: 1,
			writeCount: 1,
			bytesPerWrite: 5,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "broad",
			completionSemantics: "client_close",
			broadMatrixEquivalent: "tcp_echo_small",
		},
		{
			id: "echo_1x5_string",
			workload: "echoOnce",
			connections: 1,
			writeCount: 1,
			bytesPerWrite: 5,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "payload_copy",
			compareAgainst: "echo_1x5",
			completionSemantics: "client_close",
			broadMatrixEquivalent: "tcp_echo_small",
			payloadKind: "string",
		},
		{
			id: "echo_1x1",
			workload: "echoOnce",
			connections: 1,
			writeCount: 1,
			bytesPerWrite: 1,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "payload_copy",
			completionSemantics: "client_close",
		},
		{
			id: "echo_1x4k",
			workload: "echoOnce",
			connections: 1,
			writeCount: 1,
			bytesPerWrite: 4096,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "payload_copy",
			compareAgainst: "echo_1x1",
			completionSemantics: "client_close",
		},
		{
			id: "echo_1x64k",
			workload: "echoOnce",
			connections: 1,
			writeCount: 1,
			bytesPerWrite: 65_536,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "broad",
			compareAgainst: "echo_1x1",
			completionSemantics: "client_close",
			broadMatrixEquivalent: "tcp_echo_big",
		},
		{
			id: "echo_1x256k",
			workload: "echoOnce",
			connections: 1,
			writeCount: 1,
			bytesPerWrite: 262_144,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "payload_copy",
			compareAgainst: "echo_1x64k",
			completionSemantics: "client_close",
		},
		{
			id: "echo_1x1m",
			workload: "echoOnce",
			connections: 1,
			writeCount: 1,
			bytesPerWrite: 1_048_576,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "payload_copy",
			compareAgainst: "echo_1x256k",
			completionSemantics: "client_close",
		},
		{
			id: "burst_4x1_echo_once",
			workload: "burstWritesEchoOnce",
			connections: 1,
			writeCount: 4,
			bytesPerWrite: 1,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "write_count",
			compareAgainst: "echo_1x1",
			completionSemantics: "client_close",
		},
		{
			id: "burst_16x1_echo_once",
			workload: "burstWritesEchoOnce",
			connections: 1,
			writeCount: 16,
			bytesPerWrite: 1,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "broad",
			compareAgainst: "echo_1x1",
			completionSemantics: "client_close",
			broadMatrixEquivalent: "tcp_tiny_writes_16",
		},
		{
			id: "burst_16x1_string_echo_once",
			workload: "burstWritesEchoOnce",
			connections: 1,
			writeCount: 16,
			bytesPerWrite: 1,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "payload_copy",
			compareAgainst: "burst_16x1_echo_once",
			completionSemantics: "client_close",
			broadMatrixEquivalent: "tcp_tiny_writes_16",
			payloadKind: "string",
		},
		{
			id: "burst_64x1_echo_once",
			workload: "burstWritesEchoOnce",
			connections: 1,
			writeCount: 64,
			bytesPerWrite: 1,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "write_count",
			compareAgainst: "echo_1x1",
			completionSemantics: "client_close",
		},
		{
			id: "burst_16x4096_echo_once",
			workload: "burstWritesEchoOnce",
			connections: 1,
			writeCount: 16,
			bytesPerWrite: 4096,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "write_count",
			compareAgainst: "echo_1x64k",
			completionSemantics: "client_close",
		},
		{
			id: "burst_64x1024_echo_once",
			workload: "burstWritesEchoOnce",
			connections: 1,
			writeCount: 64,
			bytesPerWrite: 1024,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "write_count",
			compareAgainst: "echo_1x64k",
			completionSemantics: "client_close",
		},
		{
			id: "burst_256x256_echo_once",
			workload: "burstWritesEchoOnce",
			connections: 1,
			writeCount: 256,
			bytesPerWrite: 256,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "write_count",
			compareAgainst: "echo_1x64k",
			completionSemantics: "client_close",
		},
		{
			id: "pingpong_1x1",
			workload: "pingPong",
			connections: 1,
			writeCount: 1,
			bytesPerWrite: 1,
			replyMode: "echoEachWrite",
			writeCadence: "awaitReplyEachWrite",
			costAxis: "event_cadence",
			compareAgainst: "echo_1x1",
			completionSemantics: "client_close",
		},
		{
			id: "pingpong_4x1",
			workload: "pingPong",
			connections: 1,
			writeCount: 4,
			bytesPerWrite: 1,
			replyMode: "echoEachWrite",
			writeCadence: "awaitReplyEachWrite",
			costAxis: "event_cadence",
			compareAgainst: "pingpong_1x1",
			completionSemantics: "client_close",
		},
		{
			id: "pingpong_8x1",
			workload: "pingPong",
			connections: 1,
			writeCount: 8,
			bytesPerWrite: 1,
			replyMode: "echoEachWrite",
			writeCadence: "awaitReplyEachWrite",
			costAxis: "event_cadence",
			compareAgainst: "pingpong_4x1",
			completionSemantics: "client_close",
		},
		{
			id: "pingpong_16x1",
			workload: "pingPong",
			connections: 1,
			writeCount: 16,
			bytesPerWrite: 1,
			replyMode: "echoEachWrite",
			writeCadence: "awaitReplyEachWrite",
			costAxis: "event_cadence",
			compareAgainst: "pingpong_4x1",
			completionSemantics: "client_close",
		},
		{
			id: "pingpong_32x1",
			workload: "pingPong",
			connections: 1,
			writeCount: 32,
			bytesPerWrite: 1,
			replyMode: "echoEachWrite",
			writeCadence: "awaitReplyEachWrite",
			costAxis: "event_cadence",
			compareAgainst: "pingpong_16x1",
			completionSemantics: "client_close",
		},
		{
			id: "concurrent_2x1",
			workload: "concurrentConnect",
			connections: 2,
			writeCount: 1,
			bytesPerWrite: 1,
			replyMode: "none",
			writeCadence: "burstSameTick",
			costAxis: "concurrency",
			compareAgainst: "connect_close_1",
			completionSemantics: "listener_close_after_accept",
		},
		{
			id: "concurrent_4x1",
			workload: "concurrentConnect",
			connections: 4,
			writeCount: 1,
			bytesPerWrite: 1,
			replyMode: "none",
			writeCadence: "burstSameTick",
			costAxis: "broad",
			compareAgainst: "concurrent_2x1",
			completionSemantics: "listener_close_after_accept",
			broadMatrixEquivalent: "tcp_concurrent_4",
		},
		{
			id: "concurrent_8x1",
			workload: "concurrentConnect",
			connections: 8,
			writeCount: 1,
			bytesPerWrite: 1,
			replyMode: "none",
			writeCadence: "burstSameTick",
			costAxis: "concurrency",
			compareAgainst: "concurrent_4x1",
			completionSemantics: "listener_close_after_accept",
		},
		{
			id: "echo_4x1",
			workload: "echoOnce",
			connections: 4,
			writeCount: 1,
			bytesPerWrite: 1,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "concurrency",
			compareAgainst: "echo_1x1",
			completionSemantics: "client_close",
		},
		{
			id: "echo_8x1",
			workload: "echoOnce",
			connections: 8,
			writeCount: 1,
			bytesPerWrite: 1,
			replyMode: "echoOnceAfterAllBytes",
			writeCadence: "burstSameTick",
			costAxis: "concurrency",
			compareAgainst: "echo_4x1",
			completionSemantics: "client_close",
		},
	];
}

function parseArgs(): {
	iterations: number;
	warmup: number;
	rows: NetTcpRow[];
	netBridgeTrace: boolean;
} {
	const value = (name: string) =>
		process.argv.find((arg) => arg.startsWith(`--${name}=`))?.split("=")[1];
	const iterations = Number(value("iterations") ?? 20);
	const warmup = Number(value("warmup") ?? 5);
	const netBridgeTrace =
		process.argv.includes("--net-bridge-trace") ||
		value("net-bridge-trace") === "1" ||
		value("net-bridge-trace") === "true";
	const selected = value("rows") ?? value("cases");
	const rows = selected
		? selected
				.split(",")
				.map((id) => id.trim())
				.filter(Boolean)
				.map(rowForId)
		: defaultRows();
	if (iterations < 1 || warmup < 0 || rows.length === 0) {
		throw new Error(
			"invalid args; expected --iterations>=1 --warmup>=0 --rows=<row ids>",
		);
	}
	return { iterations, warmup, rows, netBridgeTrace };
}

function rowForId(id: string): NetTcpRow {
	const row = defaultRows().find((candidate) => candidate.id === id);
	if (row) return row;
	const match = /^custom_([a-zA-Z]+)_c(\d+)_w(\d+)_b(\d+)$/.exec(id);
	if (!match) throw new Error(`unknown net-tcp-event-floor row: ${id}`);
	const workload = match[1] as Workload;
	const connections = Number(match[2]);
	const writeCount = Number(match[3]);
	const bytesPerWrite = Number(match[4]);
	return {
		id,
		workload,
		connections,
		writeCount,
		bytesPerWrite,
		replyMode:
			workload === "pingPong"
				? "echoEachWrite"
				: writeCount === 0
					? "none"
					: "echoOnceAfterAllBytes",
		writeCadence:
			workload === "pingPong"
				? "awaitReplyEachWrite"
				: writeCount === 0
					? "none"
					: "burstSameTick",
		costAxis: inferCostAxis(workload, connections, writeCount, bytesPerWrite),
		completionSemantics:
			workload === "concurrentConnect"
				? "listener_close_after_accept"
				: "client_close",
	};
}

function inferCostAxis(
	workload: Workload,
	connections: number,
	writeCount: number,
	bytesPerWrite: number,
): CostAxis {
	if (workload === "connectClose")
		return connections === 1 ? "broad" : "connect_accept";
	if (workload === "concurrentConnect")
		return connections === 4 ? "broad" : "concurrency";
	if (workload === "pingPong") return "event_cadence";
	if (connections > 1) return "concurrency";
	if (writeCount > 1) return "write_count";
	return bytesPerWrite === 5 || bytesPerWrite === 65_536
		? "broad"
		: "payload_copy";
}

function expectedOps(row: NetTcpRow): NetTcpRowResult["expectedOps"] {
	const writeCalls = row.connections * row.writeCount;
	return {
		connectCalls: row.connections,
		acceptEvents: row.connections,
		writeCalls,
		expectedReadEvents:
			row.replyMode === "none"
				? 0
				: row.replyMode === "echoEachWrite"
					? writeCalls
					: row.connections,
		closeEvents: row.connections,
	};
}

function rowPayload(row: NetTcpRow): Buffer | string {
	if (row.payloadKind === "string") return "x".repeat(row.bytesPerWrite);
	return Buffer.alloc(row.bytesPerWrite, 88);
}

async function runOneRow(row: NetTcpRow): Promise<void> {
	if (row.workload === "connectClose") {
		return runConnectClose(row);
	}
	if (row.workload === "concurrentConnect") {
		return runConcurrentConnect(row);
	}
	return runEcho(row);
}

function runConnectClose(row: NetTcpRow): Promise<void> {
	return new Promise((resolve, reject) => {
		let closed = 0;
		const server = net.createServer((socket) => socket.end());
		server.on("error", reject);
		server.listen(0, "127.0.0.1", () => {
			const address = server.address();
			if (!address || typeof address === "string")
				return reject(new Error("missing address"));
			for (let i = 0; i < row.connections; i++) {
				const socket = net.connect(address.port, "127.0.0.1");
				socket.on("error", reject);
				socket.on("close", () => {
					closed += 1;
					if (closed === row.connections) {
						server.close((error) => (error ? reject(error) : resolve()));
					}
				});
				socket.end();
			}
		});
	});
}

function runConcurrentConnect(row: NetTcpRow): Promise<void> {
	return new Promise((resolve, reject) => {
		let accepted = 0;
		const server = net.createServer((socket) => {
			socket.on("data", () => socket.end());
			if (++accepted === row.connections) {
				server.close((error) => (error ? reject(error) : resolve()));
			}
		});
		server.on("error", reject);
		server.listen(0, "127.0.0.1", () => {
			const address = server.address();
			if (!address || typeof address === "string")
				return reject(new Error("missing address"));
			const payload = rowPayload(row);
			for (let i = 0; i < row.connections; i++) {
				const socket = net.connect(address.port, "127.0.0.1");
				socket.on("error", reject);
				socket.write(payload);
			}
		});
	});
}

function runEcho(row: NetTcpRow): Promise<void> {
	const payload = rowPayload(row);
	const expectedBytes = row.writeCount * row.bytesPerWrite;
	return new Promise((resolve, reject) => {
		let completed = 0;
		const finishClient = () => {
			completed += 1;
			if (completed === row.connections) {
				server.close((error) => (error ? reject(error) : resolve()));
			}
		};
		const server = net.createServer((socket) => {
			const chunks: Buffer[] = [];
			let received = 0;
			let replies = 0;
			socket.on("data", (chunk) => {
				if (row.replyMode === "echoEachWrite") {
					socket.write(chunk);
					replies += 1;
					if (replies >= row.writeCount) socket.end();
					return;
				}
				chunks.push(chunk);
				received += chunk.length;
				if (received >= expectedBytes) socket.end(Buffer.concat(chunks));
			});
			socket.on("error", reject);
		});
		server.on("error", reject);
		server.listen(0, "127.0.0.1", () => {
			const address = server.address();
			if (!address || typeof address === "string")
				return reject(new Error("missing address"));
			for (let i = 0; i < row.connections; i++) {
				const socket = net.connect(address.port, "127.0.0.1");
				let replies = 0;
				let received = 0;
				socket.on("connect", async () => {
					try {
						for (let write = 0; write < row.writeCount; write++) {
							socket.write(payload);
							if (row.writeCadence === "awaitReplyEachWrite") {
								await new Promise<void>((replyResolve) => {
									const onData = (data: Buffer) => {
										received += data.length;
										replies += 1;
										socket.off("data", onData);
										replyResolve();
									};
									socket.on("data", onData);
								});
							}
						}
					} catch (error) {
						reject(error);
					}
				});
				if (row.writeCadence !== "awaitReplyEachWrite") {
					socket.on("data", (data) => {
						received += data.length;
						replies += 1;
					});
				}
				socket.on("error", reject);
				socket.on("close", () => {
					if (row.replyMode !== "none" && received !== expectedBytes) {
						reject(
							new Error(
								`short echo: expected ${expectedBytes}, got ${received}`,
							),
						);
						return;
					}
					if (row.replyMode === "echoEachWrite" && replies < row.writeCount) {
						reject(
							new Error(
								`short pingpong: expected ${row.writeCount}, got ${replies}`,
							),
						);
						return;
					}
					finishClient();
				});
			}
		});
	});
}

async function runNode(
	row: NetTcpRow,
	iterations: number,
	warmup: number,
): Promise<number[]> {
	const samples: number[] = [];
	for (let i = 0; i < warmup + iterations; i++) {
		const start = performance.now();
		await runOneRow(row);
		const ms = performance.now() - start;
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

async function createVm(sidecar: SidecarProcess): Promise<BenchVm> {
	return createBenchVm({
		sidecar,
	});
}

async function runGuest(
	vm: BenchVm,
	row: NetTcpRow,
	iterations: number,
	warmup: number,
	netBridgeTrace: boolean,
): Promise<{
	samples: number[];
	bridgeTrace?: Record<string, number>;
	sidecarNetTrace?: Record<string, number>;
}> {
	const scriptPath = `/tmp/net-tcp-event-floor-${row.id}-${Date.now()}-${Math.random()
		.toString(16)
		.slice(2)}.mjs`;
	const source = `
import net from "node:net";
const row = ${JSON.stringify(row)};
const iterations = Number(process.env.BENCH_ITERATIONS || 20);
const warmup = Number(process.env.BENCH_WARMUP || 5);
const samples = [];
const bridgeTrace = ${netBridgeTrace ? "globalThis.__agentOSNetBridgeMetrics" : "undefined"};
bridgeTrace?.enable?.();
const __name = (target, _name) => target;
${rowPayload.toString()}
${runConnectClose.toString()}
${runConcurrentConnect.toString()}
${runEcho.toString()}
${runOneRow.toString()}
bridgeTrace?.reset?.();
for (let i = 0; i < warmup + iterations; i++) {
  if (i === warmup) bridgeTrace?.reset?.();
  const start = Number(process.hrtime.bigint()) / 1e6;
  await runOneRow(row);
  const ms = Number(process.hrtime.bigint()) / 1e6 - start;
  if (i >= warmup) samples.push(ms);
}
const bridgeSnapshot = bridgeTrace?.snapshot?.();
const sidecarNetTrace = bridgeSnapshot?.sidecarNetTrace;
if (bridgeSnapshot && Object.prototype.hasOwnProperty.call(bridgeSnapshot, "sidecarNetTrace")) {
  delete bridgeSnapshot.sidecarNetTrace;
}
process.stdout.write(JSON.stringify({ samples, bridgeTrace: bridgeSnapshot, sidecarNetTrace }));
`;
	await vm.writeFile(scriptPath, source);
	let stdout = "";
	let stderr = "";
	const proc = vm.spawn("node", [scriptPath], {
		env: {
			BENCH_ITERATIONS: String(iterations),
			BENCH_WARMUP: String(warmup),
			...(netBridgeTrace ? { AGENTOS_NET_BRIDGE_TRACE: "1" } : {}),
			...(process.env.BENCH_NET_RETAIN_OWNED_WRITE_BUFFER === "0"
				? { AGENTOS_NET_RETAIN_OWNED_WRITE_BUFFER: "0" }
				: {}),
		},
		onStdout: (data) => {
			stdout += Buffer.from(data).toString("utf8");
		},
		onStderr: (data) => {
			stderr += Buffer.from(data).toString("utf8");
		},
	});
	const code = await vm.waitProcess(proc.pid);
	if (code !== 0)
		throw new Error(
			`guest net-tcp-event-floor ${row.id} exited ${code}\n${stderr}`,
		);
	return JSON.parse(stdout);
}

function classifySummary(rows: NetTcpRowResult[]) {
	const byId = new Map(rows.map((row) => [row.id, row]));
	const evidence: string[] = [];
	const echo1 = byId.get("echo_1x1");
	const echo64k = byId.get("echo_1x64k");
	const burst16 = byId.get("burst_16x1_echo_once");
	const burst64 = byId.get("burst_64x1_echo_once");
	const ping16 = byId.get("pingpong_16x1");
	const concurrent4 = byId.get("concurrent_4x1");
	if (echo1 && echo64k) {
		evidence.push(
			`payload delta echo_1x64k-echo_1x1=${round(echo64k.guest.p50 - echo1.guest.p50)}ms`,
		);
	}
	if (echo64k && burst64) {
		evidence.push(
			`write-count delta burst_64x1_echo_once-echo_1x64k=${round(burst64.guest.p50 - echo64k.guest.p50)}ms`,
		);
	}
	if (burst16 && ping16) {
		evidence.push(
			`cadence delta pingpong_16x1-burst_16x1_echo_once=${round(ping16.guest.p50 - burst16.guest.p50)}ms`,
		);
	}
	if (concurrent4)
		evidence.push(`concurrent_4x1 guest p50=${concurrent4.guest.p50}ms`);
	const likelyDominantCost =
		ping16 && burst16 && ping16.guest.p50 > burst16.guest.p50 * 1.5
			? "poll/event-cadence"
			: echo64k && echo1 && echo64k.guest.p50 > echo1.guest.p50 * 1.5
				? "payload-copy"
				: burst64 && echo64k && burst64.guest.p50 > echo64k.guest.p50 * 1.5
					? "poll/event-cadence"
					: "mixed";
	return {
		likelyDominantCost,
		evidence,
		reproducesBroadRows: [
			"connect_close_1",
			"echo_1x5",
			"echo_1x64k",
			"burst_16x1_echo_once",
			"concurrent_4x1",
		].every((id) => byId.has(id)),
	};
}

function deriveGuestBridgeTrace(
	trace: Record<string, number> | undefined,
	measuredIterations = 1,
) {
	if (!trace) return undefined;
	const iterationCount = Math.max(1, measuredIterations);
	const rawPollSentinels =
		(trace.readTimeoutSentinels ?? 0) + (trace.acceptTimeoutSentinels ?? 0);
	const readRawCalls = trace.readRawCalls ?? 0;
	const writeRawCalls = trace.writeRawCalls ?? 0;
	const readBase64DecodeCalls = trace.readBase64DecodeCalls ?? 0;
	const writeBase64EncodeCalls = trace.writeBase64EncodeCalls ?? 0;
	const readBase64DecodeBytes = trace.readBase64DecodeBytes ?? 0;
	const writeBase64EncodeBytes = trace.writeBase64EncodeBytes ?? 0;
	const readPayloadMaterializeCalls = trace.readPayloadMaterializeCalls ?? 0;
	const readPayloadMaterializeBytes = trace.readPayloadMaterializeBytes ?? 0;
	const peerWakeScans = trace.peerWakeScans ?? 0;
	const peerWakeFound = trace.peerWakeFound ?? 0;
	const readWakeAttempts = trace.readWakeAttempts ?? 0;
	const readWakeNoTimer = trace.readWakeNoTimer ?? 0;
	const readFirstPumpAfterNoTimerWakeCalls =
		trace.readFirstPumpAfterNoTimerWakeCalls ?? 0;
	const readFirstPumpResults =
		(trace.readFirstPumpResultData ?? 0) +
		(trace.readFirstPumpResultEnd ?? 0) +
		(trace.readFirstPumpResultTimeout ?? 0);
	const readFirstPumpScheduleCandidates =
		trace.readFirstPumpScheduleCandidates ?? 0;
	const readFirstPumpScheduleQueued = trace.readFirstPumpScheduleQueued ?? 0;
	const readFirstPumpScheduleRuns = trace.readFirstPumpScheduleRuns ?? 0;
	const readFirstPumpSchedulePumpCalls =
		trace.readFirstPumpSchedulePumpCalls ?? 0;
	const readFirstPumpScheduleResults =
		(trace.readFirstPumpScheduleResultData ?? 0) +
		(trace.readFirstPumpScheduleResultTimeout ?? 0) +
		(trace.readFirstPumpScheduleResultEnd ?? 0);
	const acceptWakeSocketScans = trace.acceptWakeSocketScans ?? 0;
	const acceptWakeAttempts = trace.acceptWakeAttempts ?? 0;
	const acceptWakeNoTimer = trace.acceptWakeNoTimer ?? 0;
	const acceptFirstPumpAfterNoTimerWakeCalls =
		trace.acceptFirstPumpAfterNoTimerWakeCalls ?? 0;
	const acceptFirstPumpResults =
		(trace.acceptFirstPumpResultConnection ?? 0) +
		(trace.acceptFirstPumpResultTimeout ?? 0) +
		(trace.acceptFirstPumpResultEmpty ?? 0);
	const acceptRawCalls = trace.acceptRawCalls ?? 0;
	const acceptConnections = trace.acceptConnections ?? 0;
	const readPollTimerFires = trace.readPollTimerFires ?? 0;
	const acceptPollTimerFires = trace.acceptPollTimerFires ?? 0;
	const readMacrotaskYields = trace.readMacrotaskYields ?? 0;
	const queueReadablePayloads = trace.queueReadablePayloads ?? 0;
	const queueReadableImmediateReadCalls =
		trace.queueReadableImmediateReadCalls ?? 0;
	const socketReadableEmits = trace.socketReadableEmits ?? 0;
	const socketDataEmits = trace.socketDataEmits ?? 0;
	const postDeliveryProbeCalls =
		trace.readPostDeliveryProbeCalls ?? trace.readPostDeliveryNextRawCalls ?? 0;
	const userWriteDuringDataEmitCalls = trace.userWriteDuringDataEmitCalls ?? 0;
	const flushCalls = trace.flushCalls ?? 0;
	const readEventWakeups = trace.readEventWakeups ?? 0;
	const acceptEventWakeups = trace.acceptEventWakeups ?? 0;
	return {
		rawPollSentinels,
		readRawPerDataEvent:
			trace.readDataEvents > 0
				? round(readRawCalls / trace.readDataEvents)
				: undefined,
		acceptRawPerConnection:
			trace.acceptConnections > 0
				? round((trace.acceptRawCalls ?? 0) / trace.acceptConnections)
				: undefined,
		writeRawPerUserWrite:
			trace.userWriteCalls > 0
				? round(writeRawCalls / trace.userWriteCalls)
				: undefined,
		avgReadRawElapsedUs:
			readRawCalls > 0
				? round((trace.readRawElapsedUs ?? 0) / readRawCalls)
				: undefined,
		avgWriteRawElapsedUs:
			writeRawCalls > 0
				? round((trace.writeRawElapsedUs ?? 0) / writeRawCalls)
				: undefined,
		avgReadBase64DecodeUs:
			readBase64DecodeCalls > 0
				? round((trace.readBase64DecodeUs ?? 0) / readBase64DecodeCalls)
				: undefined,
		avgWriteBase64EncodeUs:
			writeBase64EncodeCalls > 0
				? round((trace.writeBase64EncodeUs ?? 0) / writeBase64EncodeCalls)
				: undefined,
		readBase64DecodeUsPerKiB:
			readBase64DecodeBytes > 0
				? round(
						(trace.readBase64DecodeUs ?? 0) / (readBase64DecodeBytes / 1024),
					)
				: undefined,
		writeBase64EncodeUsPerKiB:
			writeBase64EncodeBytes > 0
				? round(
						(trace.writeBase64EncodeUs ?? 0) / (writeBase64EncodeBytes / 1024),
					)
				: undefined,
		avgReadPayloadMaterializeUs:
			readPayloadMaterializeCalls > 0
				? round(
						(trace.readPayloadMaterializeUs ?? 0) / readPayloadMaterializeCalls,
					)
				: undefined,
		readPayloadMaterializeUsPerKiB:
			readPayloadMaterializeBytes > 0
				? round(
						(trace.readPayloadMaterializeUs ?? 0) /
							(readPayloadMaterializeBytes / 1024),
					)
				: undefined,
		peerWakeFoundRatio:
			peerWakeScans > 0 ? round(peerWakeFound / peerWakeScans) : undefined,
		peerWakeMissRatio:
			peerWakeScans > 0
				? round((trace.peerWakeMiss ?? 0) / peerWakeScans)
				: undefined,
		readWakeWakeupRatio:
			readWakeAttempts > 0
				? round((trace.readEventWakeups ?? 0) / readWakeAttempts)
				: undefined,
		readWakeNoTimerRatio:
			readWakeAttempts > 0
				? round(readWakeNoTimer / readWakeAttempts)
				: undefined,
		readWakeAlreadyRunningRatio:
			readWakeAttempts > 0
				? round((trace.readWakeAlreadyRunning ?? 0) / readWakeAttempts)
				: undefined,
		readWakeNoTimerBeforeFirstPumpRatio:
			readWakeNoTimer > 0
				? round((trace.readWakeNoTimerBeforeFirstPump ?? 0) / readWakeNoTimer)
				: undefined,
		readWakeNoTimerAfterFirstPumpRatio:
			readWakeNoTimer > 0
				? round((trace.readWakeNoTimerAfterFirstPump ?? 0) / readWakeNoTimer)
				: undefined,
		readWakeNoTimerConnectedRatio:
			readWakeNoTimer > 0
				? round((trace.readWakeNoTimerConnected ?? 0) / readWakeNoTimer)
				: undefined,
		readWakeNoTimerConnectingRatio:
			readWakeNoTimer > 0
				? round((trace.readWakeNoTimerConnecting ?? 0) / readWakeNoTimer)
				: undefined,
		readWakeNoTimerRefedRatio:
			readWakeNoTimer > 0
				? round((trace.readWakeNoTimerRefed ?? 0) / readWakeNoTimer)
				: undefined,
		readWakeNoTimerDataListenerRatio:
			readWakeNoTimer > 0
				? round((trace.readWakeNoTimerHasDataListener ?? 0) / readWakeNoTimer)
				: undefined,
		readWakeNoTimerReadableListenerRatio:
			readWakeNoTimer > 0
				? round(
						(trace.readWakeNoTimerHasReadableListener ?? 0) / readWakeNoTimer,
					)
				: undefined,
		readWakeNoTimerPendingWriteFlushRatio:
			readWakeNoTimer > 0
				? round((trace.readWakeNoTimerPendingWriteFlush ?? 0) / readWakeNoTimer)
				: undefined,
		readWakeNoTimerPendingWriteBytesPerIteration:
			readWakeNoTimer > 0
				? round((trace.readWakeNoTimerPendingWriteBytes ?? 0) / iterationCount)
				: undefined,
		avgReadFirstPumpAfterNoTimerWakeUs:
			readFirstPumpAfterNoTimerWakeCalls > 0
				? round(
						(trace.readFirstPumpAfterNoTimerWakeUs ?? 0) /
							readFirstPumpAfterNoTimerWakeCalls,
					)
				: undefined,
		readFirstPumpAfterNoTimerWakeMsPerIteration:
			readFirstPumpAfterNoTimerWakeCalls > 0
				? round(
						(trace.readFirstPumpAfterNoTimerWakeUs ?? 0) /
							1000 /
							iterationCount,
					)
				: undefined,
		readFirstPumpOriginConnectWaitRatio:
			trace.readPumpRuns > 0
				? round(
						(trace.readFirstPumpOriginConnectWait ?? 0) / trace.readPumpRuns,
					)
				: undefined,
		readFirstPumpOriginAcceptedHandleRatio:
			trace.readPumpRuns > 0
				? round(
						(trace.readFirstPumpOriginAcceptedHandle ?? 0) / trace.readPumpRuns,
					)
				: undefined,
		readFirstPumpOriginEventWakeRatio:
			trace.readPumpRuns > 0
				? round((trace.readFirstPumpOriginEventWake ?? 0) / trace.readPumpRuns)
				: undefined,
		readFirstPumpOriginTimerRatio:
			trace.readPumpRuns > 0
				? round((trace.readFirstPumpOriginTimer ?? 0) / trace.readPumpRuns)
				: undefined,
		readFirstPumpOriginRefRatio:
			trace.readPumpRuns > 0
				? round((trace.readFirstPumpOriginRef ?? 0) / trace.readPumpRuns)
				: undefined,
		readFirstPumpOriginTlsRatio:
			trace.readPumpRuns > 0
				? round((trace.readFirstPumpOriginTls ?? 0) / trace.readPumpRuns)
				: undefined,
		readFirstPumpOriginUnknownRatio:
			trace.readPumpRuns > 0
				? round((trace.readFirstPumpOriginUnknown ?? 0) / trace.readPumpRuns)
				: undefined,
		readFirstPumpResultDataRatio:
			readFirstPumpResults > 0
				? round((trace.readFirstPumpResultData ?? 0) / readFirstPumpResults)
				: undefined,
		readFirstPumpResultEndRatio:
			readFirstPumpResults > 0
				? round((trace.readFirstPumpResultEnd ?? 0) / readFirstPumpResults)
				: undefined,
		readFirstPumpResultTimeoutRatio:
			readFirstPumpResults > 0
				? round((trace.readFirstPumpResultTimeout ?? 0) / readFirstPumpResults)
				: undefined,
		readFirstPumpScheduleQueueRatio:
			readFirstPumpScheduleCandidates > 0
				? round(readFirstPumpScheduleQueued / readFirstPumpScheduleCandidates)
				: undefined,
		readFirstPumpScheduleRunRatio:
			readFirstPumpScheduleQueued > 0
				? round(readFirstPumpScheduleRuns / readFirstPumpScheduleQueued)
				: undefined,
		readFirstPumpSchedulePumpCallRatio:
			readFirstPumpScheduleRuns > 0
				? round(readFirstPumpSchedulePumpCalls / readFirstPumpScheduleRuns)
				: undefined,
		readFirstPumpScheduleSkipPumpStartedRatio:
			readFirstPumpScheduleRuns > 0
				? round(
						(trace.readFirstPumpScheduleSkipPumpStarted ?? 0) /
							readFirstPumpScheduleRuns,
					)
				: undefined,
		avgReadFirstPumpScheduleQueuedToRunUs:
			readFirstPumpScheduleRuns > 0
				? round(
						(trace.readFirstPumpScheduleQueuedToRunUs ?? 0) /
							readFirstPumpScheduleRuns,
					)
				: undefined,
		readFirstPumpScheduleQueuedToRunMsPerIteration:
			readFirstPumpScheduleRuns > 0
				? round(
						(trace.readFirstPumpScheduleQueuedToRunUs ?? 0) /
							1000 /
							iterationCount,
					)
				: undefined,
		avgReadFirstPumpScheduleQueuedToPumpStartUs:
			readFirstPumpSchedulePumpCalls > 0
				? round(
						(trace.readFirstPumpScheduleQueuedToPumpStartUs ?? 0) /
							readFirstPumpSchedulePumpCalls,
					)
				: undefined,
		readFirstPumpScheduleQueuedToPumpStartMsPerIteration:
			readFirstPumpSchedulePumpCalls > 0
				? round(
						(trace.readFirstPumpScheduleQueuedToPumpStartUs ?? 0) /
							1000 /
							iterationCount,
					)
				: undefined,
		readFirstPumpScheduleResultDataRatio:
			readFirstPumpScheduleResults > 0
				? round(
						(trace.readFirstPumpScheduleResultData ?? 0) /
							readFirstPumpScheduleResults,
					)
				: undefined,
		readFirstPumpScheduleResultTimeoutRatio:
			readFirstPumpScheduleResults > 0
				? round(
						(trace.readFirstPumpScheduleResultTimeout ?? 0) /
							readFirstPumpScheduleResults,
					)
				: undefined,
		acceptWakeSocketFoundRatio:
			acceptWakeSocketScans > 0
				? round((trace.acceptWakeSocketFound ?? 0) / acceptWakeSocketScans)
				: undefined,
		acceptWakeWakeupRatio:
			acceptWakeAttempts > 0
				? round((trace.acceptEventWakeups ?? 0) / acceptWakeAttempts)
				: undefined,
		acceptWakeNoTimerRatio:
			acceptWakeAttempts > 0
				? round(acceptWakeNoTimer / acceptWakeAttempts)
				: undefined,
		acceptWakeNoTimerBeforeFirstPumpRatio:
			acceptWakeNoTimer > 0
				? round(
						(trace.acceptWakeNoTimerBeforeFirstPump ?? 0) / acceptWakeNoTimer,
					)
				: undefined,
		acceptWakeNoTimerAfterFirstPumpRatio:
			acceptWakeNoTimer > 0
				? round(
						(trace.acceptWakeNoTimerAfterFirstPump ?? 0) / acceptWakeNoTimer,
					)
				: undefined,
		acceptWakeNoTimerLoopRunningRatio:
			acceptWakeNoTimer > 0
				? round((trace.acceptWakeNoTimerLoopRunning ?? 0) / acceptWakeNoTimer)
				: undefined,
		acceptWakeNoTimerLoopActiveRatio:
			acceptWakeNoTimer > 0
				? round((trace.acceptWakeNoTimerLoopActive ?? 0) / acceptWakeNoTimer)
				: undefined,
		acceptWakeNoTimerRefedRatio:
			acceptWakeNoTimer > 0
				? round((trace.acceptWakeNoTimerRefed ?? 0) / acceptWakeNoTimer)
				: undefined,
		acceptWakeNoTimerConnectionsPerWake:
			acceptWakeNoTimer > 0
				? round((trace.acceptWakeNoTimerConnections ?? 0) / acceptWakeNoTimer)
				: undefined,
		avgAcceptFirstPumpAfterNoTimerWakeUs:
			acceptFirstPumpAfterNoTimerWakeCalls > 0
				? round(
						(trace.acceptFirstPumpAfterNoTimerWakeUs ?? 0) /
							acceptFirstPumpAfterNoTimerWakeCalls,
					)
				: undefined,
		acceptFirstPumpAfterNoTimerWakeMsPerIteration:
			acceptFirstPumpAfterNoTimerWakeCalls > 0
				? round(
						(trace.acceptFirstPumpAfterNoTimerWakeUs ?? 0) /
							1000 /
							iterationCount,
					)
				: undefined,
		acceptFirstPumpOriginListenRatio:
			trace.acceptPumpRuns > 0
				? round((trace.acceptFirstPumpOriginListen ?? 0) / trace.acceptPumpRuns)
				: undefined,
		acceptFirstPumpOriginEventWakeRatio:
			trace.acceptPumpRuns > 0
				? round(
						(trace.acceptFirstPumpOriginEventWake ?? 0) / trace.acceptPumpRuns,
					)
				: undefined,
		acceptFirstPumpOriginTimerRatio:
			trace.acceptPumpRuns > 0
				? round((trace.acceptFirstPumpOriginTimer ?? 0) / trace.acceptPumpRuns)
				: undefined,
		acceptFirstPumpOriginRefRatio:
			trace.acceptPumpRuns > 0
				? round((trace.acceptFirstPumpOriginRef ?? 0) / trace.acceptPumpRuns)
				: undefined,
		acceptFirstPumpOriginUnknownRatio:
			trace.acceptPumpRuns > 0
				? round(
						(trace.acceptFirstPumpOriginUnknown ?? 0) / trace.acceptPumpRuns,
					)
				: undefined,
		acceptFirstPumpResultConnectionRatio:
			acceptFirstPumpResults > 0
				? round(
						(trace.acceptFirstPumpResultConnection ?? 0) /
							acceptFirstPumpResults,
					)
				: undefined,
		acceptFirstPumpResultTimeoutRatio:
			acceptFirstPumpResults > 0
				? round(
						(trace.acceptFirstPumpResultTimeout ?? 0) / acceptFirstPumpResults,
					)
				: undefined,
		acceptFirstPumpResultEmptyRatio:
			acceptFirstPumpResults > 0
				? round(
						(trace.acceptFirstPumpResultEmpty ?? 0) / acceptFirstPumpResults,
					)
				: undefined,
		avgAcceptRawElapsedUs:
			acceptRawCalls > 0
				? round((trace.acceptRawElapsedUs ?? 0) / acceptRawCalls)
				: undefined,
		avgAcceptJsonParseUs:
			acceptConnections > 0
				? round((trace.acceptJsonParseUs ?? 0) / acceptConnections)
				: undefined,
		avgAcceptOnConnectionUs:
			acceptConnections > 0
				? round((trace.acceptOnConnectionUs ?? 0) / acceptConnections)
				: undefined,
		avgReadMacrotaskYieldUs:
			readMacrotaskYields > 0
				? round((trace.readMacrotaskYieldElapsedUs ?? 0) / readMacrotaskYields)
				: undefined,
		readMacrotaskYieldMsPerIteration:
			readMacrotaskYields > 0
				? round(
						(trace.readMacrotaskYieldElapsedUs ?? 0) / 1000 / iterationCount,
					)
				: undefined,
		avgReadPollTimerFireLagUs:
			readPollTimerFires > 0
				? round((trace.readPollTimerFireLagUs ?? 0) / readPollTimerFires)
				: undefined,
		readPollTimerFireLagMsPerIteration:
			readPollTimerFires > 0
				? round((trace.readPollTimerFireLagUs ?? 0) / 1000 / iterationCount)
				: undefined,
		avgAcceptPollTimerFireLagUs:
			acceptPollTimerFires > 0
				? round((trace.acceptPollTimerFireLagUs ?? 0) / acceptPollTimerFires)
				: undefined,
		acceptPollTimerFireLagMsPerIteration:
			acceptPollTimerFires > 0
				? round((trace.acceptPollTimerFireLagUs ?? 0) / 1000 / iterationCount)
				: undefined,
		avgQueueReadablePayloadUs:
			queueReadablePayloads > 0
				? round(
						(trace.queueReadablePayloadElapsedUs ?? 0) / queueReadablePayloads,
					)
				: undefined,
		queueReadablePayloadMsPerIteration:
			queueReadablePayloads > 0
				? round(
						(trace.queueReadablePayloadElapsedUs ?? 0) / 1000 / iterationCount,
					)
				: undefined,
		avgQueueReadableImmediateReadUs:
			queueReadableImmediateReadCalls > 0
				? round(
						(trace.queueReadableImmediateReadUs ?? 0) /
							queueReadableImmediateReadCalls,
					)
				: undefined,
		avgSocketReadableEmitUs:
			socketReadableEmits > 0
				? round((trace.socketReadableEmitUs ?? 0) / socketReadableEmits)
				: undefined,
		socketReadableEmitMsPerIteration:
			socketReadableEmits > 0
				? round((trace.socketReadableEmitUs ?? 0) / 1000 / iterationCount)
				: undefined,
		avgSocketDataEmitUs:
			socketDataEmits > 0
				? round((trace.socketDataEmitUs ?? 0) / socketDataEmits)
				: undefined,
		socketDataEmitMsPerIteration:
			socketDataEmits > 0
				? round((trace.socketDataEmitUs ?? 0) / 1000 / iterationCount)
				: undefined,
		postDeliveryProbeSentinelRatio:
			postDeliveryProbeCalls > 0
				? round(
						(trace.readPostDeliveryProbeTimeoutSentinels ??
							trace.readPostDeliveryNextRawTimeoutSentinels ??
							0) / postDeliveryProbeCalls,
					)
				: undefined,
		postDeliveryProbeDataRatio:
			postDeliveryProbeCalls > 0
				? round(
						(trace.readPostDeliveryProbeDataEvents ??
							trace.readPostDeliveryNextRawDataEvents ??
							0) / postDeliveryProbeCalls,
					)
				: undefined,
		avgPostDeliveryToProbeStartUs:
			postDeliveryProbeCalls > 0
				? round(
						(trace.readPostDeliveryToProbeStartUs ?? 0) /
							postDeliveryProbeCalls,
					)
				: undefined,
		postDeliveryProbeMsPerIteration:
			postDeliveryProbeCalls > 0
				? round(
						(trace.readPostDeliveryProbeElapsedUs ?? 0) / 1000 / iterationCount,
					)
				: undefined,
		postDeliveryPendingWriteFlushRatio:
			postDeliveryProbeCalls > 0
				? round(
						(trace.readPostDeliveryPendingWriteFlushes ?? 0) /
							postDeliveryProbeCalls,
					)
				: undefined,
		postDeliveryPendingWriteBytesPerIteration:
			postDeliveryProbeCalls > 0
				? round((trace.readPostDeliveryPendingWriteBytes ?? 0) / iterationCount)
				: undefined,
		avgDataEmitStartToUserWriteUs:
			userWriteDuringDataEmitCalls > 0
				? round(
						(trace.dataEmitStartToUserWriteUs ?? 0) /
							userWriteDuringDataEmitCalls,
					)
				: undefined,
		dataEmitStartToUserWriteMsPerIteration:
			userWriteDuringDataEmitCalls > 0
				? round((trace.dataEmitStartToUserWriteUs ?? 0) / 1000 / iterationCount)
				: undefined,
		avgDataEmitEndToUserWriteUs:
			trace.dataEmitEndToUserWriteUs > 0 &&
			trace.userWriteCalls > userWriteDuringDataEmitCalls
				? round(
						(trace.dataEmitEndToUserWriteUs ?? 0) /
							(trace.userWriteCalls - userWriteDuringDataEmitCalls),
					)
				: undefined,
		dataEmitEndToUserWriteMsPerIteration:
			trace.dataEmitEndToUserWriteUs > 0
				? round((trace.dataEmitEndToUserWriteUs ?? 0) / 1000 / iterationCount)
				: undefined,
		avgWriteQueuedToFlushStartUs:
			flushCalls > 0
				? round((trace.writeQueuedToFlushStartUs ?? 0) / flushCalls)
				: undefined,
		writeQueuedToFlushStartMsPerIteration:
			flushCalls > 0
				? round((trace.writeQueuedToFlushStartUs ?? 0) / 1000 / iterationCount)
				: undefined,
		avgWriteFlushQueuedToRawUs:
			flushCalls > 0
				? round((trace.writeFlushQueuedToRawUs ?? 0) / flushCalls)
				: undefined,
		writeFlushQueuedToRawMsPerIteration:
			flushCalls > 0
				? round((trace.writeFlushQueuedToRawUs ?? 0) / 1000 / iterationCount)
				: undefined,
		avgReadWakeQueuedToPumpStartUs:
			readEventWakeups > 0
				? round((trace.readWakeQueuedToPumpStartUs ?? 0) / readEventWakeups)
				: undefined,
		readWakeQueuedToPumpStartMsPerIteration:
			readEventWakeups > 0
				? round(
						(trace.readWakeQueuedToPumpStartUs ?? 0) / 1000 / iterationCount,
					)
				: undefined,
		avgAcceptWakeQueuedToPumpStartUs:
			acceptEventWakeups > 0
				? round((trace.acceptWakeQueuedToPumpStartUs ?? 0) / acceptEventWakeups)
				: undefined,
		acceptWakeQueuedToPumpStartMsPerIteration:
			acceptEventWakeups > 0
				? round(
						(trace.acceptWakeQueuedToPumpStartUs ?? 0) / 1000 / iterationCount,
					)
				: undefined,
	};
}

function deriveSidecarNetTrace(trace: Record<string, number> | undefined) {
	if (!trace) return undefined;
	const kernelPollTargets = trace.kernelPollTargets ?? 0;
	const socketReadCalls = trace.socketReadCalls ?? 0;
	const serverAcceptCalls = trace.serverAcceptCalls ?? 0;
	const socketReadDataEvents = trace.socketReadDataEvents ?? 0;
	const socketReadBytes = trace.socketReadBytes ?? 0;
	const socketReadRecordCloneCalls = trace.socketReadRecordCloneCalls ?? 0;
	const socketReadRecvCalls = trace.socketReadRecvCalls ?? 0;
	const socketReadRecvBytes = trace.socketReadRecvBytes ?? 0;
	const socketWriteCalls = trace.socketWriteCalls ?? 0;
	const socketWriteBytes = trace.socketWriteBytes ?? 0;
	return {
		kernelPollEmptyRatio:
			kernelPollTargets > 0
				? round((trace.kernelPollEmpty ?? 0) / kernelPollTargets)
				: undefined,
		socketReadDataHitRatio:
			socketReadCalls > 0
				? round(socketReadDataEvents / socketReadCalls)
				: undefined,
		acceptConnectionHitRatio:
			serverAcceptCalls > 0
				? round((trace.serverAcceptConnections ?? 0) / serverAcceptCalls)
				: undefined,
		avgKernelPollElapsedUs:
			kernelPollTargets > 0
				? round((trace.kernelPollElapsedUs ?? 0) / kernelPollTargets)
				: undefined,
		avgKernelPollWaitUs:
			kernelPollTargets > 0
				? round((trace.kernelPollWaitUs ?? 0) / kernelPollTargets)
				: undefined,
		avgSocketReadKernelUs:
			socketReadDataEvents > 0
				? round((trace.socketReadKernelUs ?? 0) / socketReadDataEvents)
				: undefined,
		avgSocketWriteKernelUs:
			socketWriteCalls > 0
				? round((trace.socketWriteKernelUs ?? 0) / socketWriteCalls)
				: undefined,
		avgSocketReadRecordCloneUs:
			socketReadRecordCloneCalls > 0
				? round(
						(trace.socketReadRecordCloneUs ?? 0) / socketReadRecordCloneCalls,
					)
				: undefined,
		avgSocketReadRecvCopyUs:
			socketReadRecvCalls > 0
				? round((trace.socketReadRecvCopyUs ?? 0) / socketReadRecvCalls)
				: undefined,
		avgSocketReadRecvChunks:
			socketReadRecvCalls > 0
				? round((trace.socketReadRecvChunks ?? 0) / socketReadRecvCalls)
				: undefined,
		socketReadKernelUsPerKiB:
			socketReadBytes > 0
				? round((trace.socketReadKernelUs ?? 0) / (socketReadBytes / 1024))
				: undefined,
		socketWriteKernelUsPerKiB:
			socketWriteBytes > 0
				? round((trace.socketWriteKernelUs ?? 0) / (socketWriteBytes / 1024))
				: undefined,
		socketReadRecvCopyUsPerKiB:
			socketReadRecvBytes > 0
				? round(
						(trace.socketReadRecvCopyUs ?? 0) / (socketReadRecvBytes / 1024),
					)
				: undefined,
	};
}

async function runCase(
	row: NetTcpRow,
	iterations: number,
	warmup: number,
	sidecar: SidecarProcess,
	netBridgeTrace: boolean,
): Promise<NetTcpRowResult> {
	const nodeMs = await runNode(row, iterations, warmup);
	const vm = await createVm(sidecar);
	try {
		const guestRun = await runGuest(
			vm,
			row,
			iterations,
			warmup,
			netBridgeTrace,
		);
		const guestMs = guestRun.samples;
		const node = stats(nodeMs);
		const guest = stats(guestMs);
		const totalBytes = row.connections * row.writeCount * row.bytesPerWrite;
		const ops = expectedOps(row);
		const singleWrite = row.writeCount > 1 ? undefined : undefined;
		return {
			...row,
			totalBytes,
			expectedOps: ops,
			node,
			guest,
			guestVsNodeRatio: round(guest.p50 / node.p50),
			guestMinusNodeMs: round(guest.p50 - node.p50),
			guestMsPerConnection: round(guest.p50 / row.connections),
			guestMsPerWrite:
				ops.writeCalls > 0 ? round(guest.p50 / ops.writeCalls) : undefined,
			guestMsPerKiB:
				totalBytes > 0 ? round(guest.p50 / (totalBytes / 1024)) : undefined,
			incrementalGuestMsFromSingleWrite: singleWrite,
			guestBridgeTrace: guestRun.bridgeTrace,
			sidecarNetTrace: guestRun.sidecarNetTrace,
			derivedGuestBridgeTrace: deriveGuestBridgeTrace(
				guestRun.bridgeTrace,
				guestMs.length,
			),
			derivedSidecarNetTrace: deriveSidecarNetTrace(guestRun.sidecarNetTrace),
			raw: { nodeMs, guestMs },
		};
	} finally {
		await vm.dispose();
	}
}

function addIncrementalFields(rows: NetTcpRowResult[]): void {
	const single = rows.find((row) => row.id === "echo_1x1");
	const byId = new Map(rows.map((row) => [row.id, row]));
	for (const row of rows) {
		if (
			single &&
			row.bytesPerWrite === 1 &&
			row.writeCount > 1 &&
			row.connections === 1
		) {
			row.incrementalGuestMsFromSingleWrite = round(
				row.guest.p50 - single.guest.p50,
			);
		}
		if (row.compareAgainst) {
			const baseline = byId.get(row.compareAgainst);
			if (baseline) {
				row.compareAgainstGuestDeltaMs = round(
					row.guest.p50 - baseline.guest.p50,
				);
			}
		}
	}
}

async function main(): Promise<void> {
	const { iterations, warmup, rows, netBridgeTrace } = parseArgs();
	const hardware = getHardware();
	console.error("=== Net TCP Event Floor Benchmark ===");
	console.error(`CPU: ${hardware.cpu}`);
	console.error(`RAM: ${hardware.ram} | Node: ${hardware.node}`);
	console.error(
		`Iterations: ${iterations} (+ ${warmup} warmup), rows: ${rows.map((r) => r.id).join(",")}`,
	);
	if (netBridgeTrace) console.error("Guest net bridge tracing: enabled");
	const sidecar = await createBenchSidecar();
	try {
		const results: NetTcpRowResult[] = [];
		for (const row of rows) {
			const result = await runCase(
				row,
				iterations,
				warmup,
				sidecar,
				netBridgeTrace,
			);
			results.push(result);
			console.error(
				`  row=${row.id}: node.p50=${result.node.p50}ms guest.p50=${result.guest.p50}ms ratio=${result.guestVsNodeRatio}x`,
			);
		}
		addIncrementalFields(results);
		const summary = classifySummary(results);
		printTable(
			[
				"row",
				"workload",
				"conn",
				"writes",
				"bytes/write",
				"node p50",
				"guest p50",
				"guest/node",
				"guest ms/write",
				...(netBridgeTrace
					? [
							"read sentinels",
							"accept sentinels",
							"kernel empty",
							"raw writes",
							"copy bytes",
							"retain bytes",
							"b64 decode us",
							"b64 encode us",
							"kernel rw us",
						]
					: []),
			],
			results.map((row) => [
				row.id,
				row.workload,
				row.connections,
				row.writeCount,
				row.bytesPerWrite,
				`${row.node.p50}ms`,
				`${row.guest.p50}ms`,
				`${row.guestVsNodeRatio}x`,
				row.guestMsPerWrite === undefined ? "n/a" : `${row.guestMsPerWrite}ms`,
				...(netBridgeTrace
					? [
							row.guestBridgeTrace?.readTimeoutSentinels ?? 0,
							row.guestBridgeTrace?.acceptTimeoutSentinels ?? 0,
							row.sidecarNetTrace?.kernelPollEmpty ?? 0,
							row.guestBridgeTrace?.writeRawCalls ?? 0,
							row.guestBridgeTrace?.queuedWriteCopiedBytes ?? 0,
							row.guestBridgeTrace?.queuedWriteRetainedBytes ?? 0,
							row.derivedGuestBridgeTrace?.avgReadBase64DecodeUs ?? 0,
							row.derivedGuestBridgeTrace?.avgWriteBase64EncodeUs ?? 0,
							`${row.derivedSidecarNetTrace?.avgSocketReadKernelUs ?? 0}/${row.derivedSidecarNetTrace?.avgSocketWriteKernelUs ?? 0}`,
						]
					: []),
			]),
		);
		console.log(
			JSON.stringify(
				{
					benchmark: netBridgeTrace
						? "net-tcp-cadence-trace"
						: "net-tcp-event-floor",
					generatedAt: new Date().toISOString(),
					hardware,
					iterations,
					warmup,
					netBridgeTrace,
					rows: results,
					summary,
				},
				null,
				2,
			),
		);
	} finally {
		await sidecar.dispose();
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
