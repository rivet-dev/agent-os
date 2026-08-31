declare module "long-timeout" {
	export interface LongTimeout {}

	export function setTimeout(
		callback: (...args: unknown[]) => void,
		delay: number,
		...args: unknown[]
	): LongTimeout;

	export function clearTimeout(timeout: LongTimeout): void;
}
