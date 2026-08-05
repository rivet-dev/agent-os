export function bigIntToSafeNumber(value: bigint, context: string): number {
	const max = BigInt(Number.MAX_SAFE_INTEGER);
	const min = BigInt(Number.MIN_SAFE_INTEGER);
	if (value > max || value < min) {
		throw new Error(`${context} exceeds JavaScript safe integer range`);
	}
	return Number(value);
}
