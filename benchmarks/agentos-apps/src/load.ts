import { performance } from "node:perf_hooks";
import { pathToFileURL } from "node:url";

export interface LoadConfig {
	target: string;
	concurrency: number;
	durationSeconds: number;
	timeoutMs: number;
	maxRequests: number;
	maxSamples: number;
	maxResponseBytes: number;
	maxReplicaSeries: number;
	maxP95Ms?: number;
	minSuccessRate?: number;
}

export interface LatencySummary {
	p50: number;
	p89: number;
	p95: number;
	p99: number;
	max: number;
}

export interface LoadResult {
	target: string;
	concurrency: number;
	durationSeconds: number;
	elapsedSeconds: number;
	completed: number;
	requestsPerSecond: number;
	successRate: number;
	latencyMs: LatencySummary;
	coldLatencyMs: LatencySummary;
	warmLatencyMs: LatencySummary;
	statuses: Record<string, number>;
	replicas: Record<string, number>;
	coldStarts: number;
	warmRequests: number;
	unclassifiedRequests: number;
	warmHitRate: number;
	replicaHeaderCoverage: number;
	maximumReplicaCount: number;
	queueDelayMs: Pick<LatencySummary, "p50" | "p95" | "max">;
	sampledRequests: number;
	droppedLatencySamples: number;
	droppedQueueDelaySamples: number;
	droppedReplicaSeries: number;
	stoppedBy: "duration" | "request-limit";
}

export function readLoadConfig(
	env: NodeJS.ProcessEnv = process.env,
): LoadConfig {
	return {
		target:
			env.LOAD_TEST_URL ?? "http://127.0.0.1:3000/apps/hello-world",
		concurrency: integerEnv(env, "LOAD_TEST_CONCURRENCY", 16, 1, 1_000),
		durationSeconds: integerEnv(
			env,
			"LOAD_TEST_DURATION_SECONDS",
			10,
			1,
			3_600,
		),
		timeoutMs: integerEnv(env, "LOAD_TEST_TIMEOUT_MS", 10_000, 1, 60_000),
		maxRequests: integerEnv(
			env,
			"LOAD_TEST_MAX_REQUESTS",
			100_000,
			1,
			10_000_000,
		),
		maxSamples: integerEnv(
			env,
			"LOAD_TEST_MAX_SAMPLES",
			100_000,
			1,
			1_000_000,
		),
		maxResponseBytes: integerEnv(
			env,
			"LOAD_TEST_MAX_RESPONSE_BYTES",
			1_048_576,
			0,
			134_217_728,
		),
		maxReplicaSeries: integerEnv(
			env,
			"LOAD_TEST_MAX_REPLICA_SERIES",
			1_024,
			1,
			10_000,
		),
		maxP95Ms: optionalNumberEnv(env, "LOAD_TEST_MAX_P95_MS"),
		minSuccessRate: optionalNumberEnv(
			env,
			"LOAD_TEST_MIN_SUCCESS_RATE",
			1,
		),
	};
}

export async function runLoadTest(
	config: LoadConfig,
	fetchImpl: typeof fetch = fetch,
): Promise<LoadResult> {
	const loadStartedAt = performance.now();
	const deadline = loadStartedAt + config.durationSeconds * 1_000;
	const latencies: number[] = [];
	const coldLatencies: number[] = [];
	const warmLatencies: number[] = [];
	const statuses = new Map<string, number>();
	const replicas = new Map<string, number>();
	const queueDelays: number[] = [];
	let started = 0;
	let completed = 0;
	let successful = 0;
	let coldStarts = 0;
	let warmRequests = 0;
	let replicaHeaders = 0;
	let maximumReplicaCount = 0;
	let droppedLatencySamples = 0;
	let droppedQueueDelaySamples = 0;
	let droppedReplicaSeries = 0;

	await Promise.all(
		Array.from({ length: config.concurrency }, async () => {
			while (
				performance.now() < deadline &&
				started < config.maxRequests
			) {
				started += 1;
				const startedAt = performance.now();
				let status = "error";
				let temperature: "cold" | "warm" | undefined;
				try {
					const response = await fetchImpl(config.target, {
						signal: AbortSignal.timeout(config.timeoutMs),
						headers: { "user-agent": "agentos-apps-load-test" },
					});
					await consumeResponseBody(response, config.maxResponseBytes);
					status = String(response.status);
					if (response.ok) successful += 1;

					const replica = response.headers.get("x-agentos-app-replica");
					if (replica) {
						replicaHeaders += 1;
						if (
							replicas.has(replica) ||
							replicas.size < config.maxReplicaSeries
						) {
							replicas.set(replica, (replicas.get(replica) ?? 0) + 1);
						} else {
							droppedReplicaSeries += 1;
						}
					}

					const coldStart = response.headers.get(
						"x-agentos-app-cold-start",
					);
					if (coldStart === "1") {
						coldStarts += 1;
						temperature = "cold";
					} else if (coldStart === "0") {
						warmRequests += 1;
						temperature = "warm";
					}

					const queueDelay = headerNumber(
						response,
						"x-agentos-app-queue-delay-ms",
					);
					if (queueDelay !== undefined) {
						if (queueDelays.length < config.maxSamples) {
							queueDelays.push(queueDelay);
						} else {
							droppedQueueDelaySamples += 1;
						}
					}

					const replicaCount = headerNumber(
						response,
						"x-agentos-app-replica-count",
					);
					if (replicaCount !== undefined) {
						maximumReplicaCount = Math.max(
							maximumReplicaCount,
							replicaCount,
						);
					}
				} catch (error) {
					status = error instanceof Error ? error.name : "error";
				}

				const latency = performance.now() - startedAt;
				if (latencies.length < config.maxSamples) {
					latencies.push(latency);
				} else {
					droppedLatencySamples += 1;
				}
				if (
					temperature === "cold" &&
					coldLatencies.length < config.maxSamples
				) {
					coldLatencies.push(latency);
				}
				if (
					temperature === "warm" &&
					warmLatencies.length < config.maxSamples
				) {
					warmLatencies.push(latency);
				}
				statuses.set(status, (statuses.get(status) ?? 0) + 1);
				completed += 1;
			}
		}),
	);

	const elapsedSeconds = (performance.now() - loadStartedAt) / 1_000;
	const successRate = completed === 0 ? 0 : successful / completed;
	const classifiedRequests = coldStarts + warmRequests;
	return {
		target: config.target,
		concurrency: config.concurrency,
		durationSeconds: config.durationSeconds,
		elapsedSeconds: round(elapsedSeconds),
		completed,
		requestsPerSecond: round(completed / elapsedSeconds),
		successRate,
		latencyMs: latencySummary(latencies),
		coldLatencyMs: latencySummary(coldLatencies),
		warmLatencyMs: latencySummary(warmLatencies),
		statuses: Object.fromEntries([...statuses.entries()].sort()),
		replicas: Object.fromEntries([...replicas.entries()].sort()),
		coldStarts,
		warmRequests,
		unclassifiedRequests: completed - classifiedRequests,
		warmHitRate:
			classifiedRequests === 0 ? 0 : warmRequests / classifiedRequests,
		replicaHeaderCoverage:
			completed === 0 ? 0 : replicaHeaders / completed,
		maximumReplicaCount,
		queueDelayMs: {
			p50: round(percentile(queueDelays, 0.5)),
			p95: round(percentile(queueDelays, 0.95)),
			max: round(Math.max(...queueDelays, 0)),
		},
		sampledRequests: latencies.length,
		droppedLatencySamples,
		droppedQueueDelaySamples,
		droppedReplicaSeries,
		stoppedBy:
			started >= config.maxRequests ? "request-limit" : "duration",
	};
}

async function main(): Promise<void> {
	const config = readLoadConfig();
	const result = await runLoadTest(config);
	console.log(JSON.stringify(result, null, 2));

	if (
		result.droppedLatencySamples > 0 ||
		result.droppedQueueDelaySamples > 0
	) {
		console.warn(
			`sample limit ${config.maxSamples} reached; dropped ${result.droppedLatencySamples} latency and ${result.droppedQueueDelaySamples} queue-delay samples; raise LOAD_TEST_MAX_SAMPLES`,
		);
	}
	if (result.droppedReplicaSeries > 0) {
		console.warn(
			`replica series limit ${config.maxReplicaSeries} reached; dropped ${result.droppedReplicaSeries} replica observations; raise LOAD_TEST_MAX_REPLICA_SERIES`,
		);
	}
	if (
		config.maxP95Ms !== undefined &&
		result.latencyMs.p95 > config.maxP95Ms
	) {
		console.error(
			`p95 ${result.latencyMs.p95}ms exceeded ${config.maxP95Ms}ms`,
		);
		process.exitCode = 1;
	}
	if (
		config.minSuccessRate !== undefined &&
		result.successRate < config.minSuccessRate
	) {
		console.error(
			`success rate ${result.successRate} was below ${config.minSuccessRate}`,
		);
		process.exitCode = 1;
	}
}

async function consumeResponseBody(
	response: Response,
	maxResponseBytes: number,
): Promise<void> {
	if (!response.body) return;
	const reader = response.body.getReader();
	let received = 0;
	try {
		while (true) {
			const { done, value } = await reader.read();
			if (done) return;
			received += value.byteLength;
			if (received > maxResponseBytes) {
				await reader.cancel();
				throw new ResponseBodyLimitError(maxResponseBytes);
			}
		}
	} finally {
		reader.releaseLock();
	}
}

function headerNumber(
	response: Response,
	name: string,
): number | undefined {
	const header = response.headers.get(name);
	if (header === null) return undefined;
	const value = Number(header);
	return Number.isFinite(value) ? value : undefined;
}

class ResponseBodyLimitError extends Error {
	override name = "ResponseBodyLimitError";

	constructor(limit: number) {
		super(
			`response exceeded LOAD_TEST_MAX_RESPONSE_BYTES (${limit} bytes); raise the limit to read larger responses`,
		);
	}
}

function integerEnv(
	env: NodeJS.ProcessEnv,
	name: string,
	fallback: number,
	minimum: number,
	maximum: number,
): number {
	const value = Number(env[name] ?? fallback);
	if (!Number.isInteger(value) || value < minimum || value > maximum) {
		throw new Error(
			`${name} must be an integer between ${minimum} and ${maximum}`,
		);
	}
	return value;
}

function optionalNumberEnv(
	env: NodeJS.ProcessEnv,
	name: string,
	maximum = Number.POSITIVE_INFINITY,
): number | undefined {
	if (env[name] === undefined) return undefined;
	const value = Number(env[name]);
	if (!Number.isFinite(value) || value < 0 || value > maximum) {
		throw new Error(
			`${name} must be a number between 0 and ${maximum}`,
		);
	}
	return value;
}

function latencySummary(values: number[]): LatencySummary {
	values.sort((a, b) => a - b);
	return {
		p50: round(percentile(values, 0.5)),
		p89: round(percentile(values, 0.89)),
		p95: round(percentile(values, 0.95)),
		p99: round(percentile(values, 0.99)),
		max: round(values.at(-1) ?? 0),
	};
}

function percentile(sorted: number[], quantile: number): number {
	if (sorted.length === 0) return 0;
	return sorted[
		Math.min(sorted.length - 1, Math.ceil(sorted.length * quantile) - 1)
	]!;
}

function round(value: number): number {
	return Math.round(value * 100) / 100;
}

const entrypoint = process.argv[1];
if (entrypoint && import.meta.url === pathToFileURL(entrypoint).href) {
	await main();
}
