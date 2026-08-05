import type { FuzzProgram } from "./generator.js";

export function shrinkProgram(program: FuzzProgram): FuzzProgram {
	return {
		...program,
		count: Math.max(1, Math.ceil(program.count / 2)),
		concurrency: Math.max(1, Math.ceil(program.concurrency / 2)),
		payloadBytes: program.payloadBytes > 0 ? Math.max(1, program.payloadBytes) : 0,
		chunkBytes: program.chunkBytes > 0 ? Math.max(1, program.chunkBytes) : 0,
	};
}
