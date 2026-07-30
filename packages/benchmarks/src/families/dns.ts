import type { BenchmarkOp } from "../lib/layers.js";

export const dnsFamily: BenchmarkOp[] = [
	{
		family: "dns",
		name: "resolve_uncached_localhost",
		nativeOp: "dns_lookup",
		wasmUnsupportedReason: "DNS lookup is not supported in the native-baseline wasm lane",
		fileLine: "crates/vm-kernel/src/dns.rs:218",
		reproducer: "await dns.lookup('localhost') inside VM",
		program: `async () => {
  const dns = await import("node:dns/promises");
  const result = await dns.lookup("localhost");
  if (!result.address) throw new Error("no address");
}`,
	},
	{
		family: "dns",
		name: "resolve_cached_localhost",
		nativeOp: "dns_lookup_x2",
		wasmUnsupportedReason: "DNS lookup is not supported in the native-baseline wasm lane",
		fileLine: "crates/vm-kernel/src/dns.rs:218",
		reproducer: "two dns.lookup('localhost') calls in one VM process",
		program: `async () => {
  const dns = await import("node:dns/promises");
  const first = await dns.lookup("localhost");
  const second = await dns.lookup("localhost");
  if (!first.address || !second.address) throw new Error("no address");
}`,
	},
	{
		family: "dns",
		name: "resolve_concurrent_4",
		nativeOp: "dns_concurrent",
		wasmUnsupportedReason: "DNS lookup is not supported in the native-baseline wasm lane",
		fileLine: "crates/vm-kernel/src/dns.rs:218",
		reproducer: "four concurrent dns.lookup('localhost') calls inside VM",
		program: `async () => {
  const dns = await import("node:dns/promises");
  const results = await Promise.all(Array.from({ length: 4 }, () => dns.lookup("localhost")));
  if (results.some((result) => !result.address)) throw new Error("no address");
}`,
	},
];
