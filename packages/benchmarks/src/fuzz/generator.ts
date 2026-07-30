export interface FuzzProgram {
	id: string;
	family: "process" | "fs" | "net" | "dns" | "pipes";
	count: number;
	payloadBytes: number;
	concurrency: number;
	chunkBytes: number;
	interleaving: "serial" | "fanout";
}

export function generateSeedCorpus(): FuzzProgram[] {
	return [
		{
			id: "known-spawn-tax",
			family: "process",
			count: 6,
			payloadBytes: 0,
			concurrency: 1,
			chunkBytes: 0,
			interleaving: "serial",
		},
		{
			id: "fanout-spawn-storm",
			family: "process",
			count: 12,
			payloadBytes: 0,
			concurrency: 3,
			chunkBytes: 0,
			interleaving: "fanout",
		},
		{
			id: "fanout-stdout-storm",
			family: "process",
			count: 6,
			payloadBytes: 1024,
			concurrency: 3,
			chunkBytes: 1024,
			interleaving: "fanout",
		},
		{
			id: "small-fs-write-churn",
			family: "fs",
			count: 16,
			payloadBytes: 64,
			concurrency: 1,
			chunkBytes: 64,
			interleaving: "serial",
		},
	];
}
