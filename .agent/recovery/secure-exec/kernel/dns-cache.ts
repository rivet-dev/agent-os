/**
 * Kernel DNS cache shared across runtimes.
 *
 * Runtimes call kernel DNS cache before falling through to the host
 * adapter. Entries expire after their TTL.
 */

import type { DnsResult } from "./host-adapter.js";

export interface DnsCacheOptions {
	/** Default TTL in milliseconds when none is specified. Default: 30000 (30s). */
	defaultTtlMs?: number;
}

interface DnsCacheEntry {
	result: DnsResult;
	expiresAt: number;
}

export class DnsCache {
	private cache: Map<string, DnsCacheEntry> = new Map();
	private defaultTtlMs: number;

	constructor(options?: DnsCacheOptions) {
		this.defaultTtlMs = options?.defaultTtlMs ?? 30_000;
	}

	/**
	 * Look up a cached DNS result. Returns null on miss or expired entry.
	 */
	lookup(hostname: string, rrtype: string): DnsResult | null {
		const key = cacheKey(hostname, rrtype);
		const entry = this.cache.get(key);
		if (!entry) return null;

		// Expired — remove and return miss
		if (Date.now() >= entry.expiresAt) {
			this.cache.delete(key);
			return null;
		}

		return entry.result;
	}

	/**
	 * Store a DNS result with TTL.
	 * @param ttlMs TTL in milliseconds. Uses defaultTtlMs if not provided.
	 */
	store(hostname: string, rrtype: string, result: DnsResult, ttlMs?: number): void {
		const key = cacheKey(hostname, rrtype);
		const ttl = ttlMs ?? this.defaultTtlMs;
		this.cache.set(key, {
			result,
			expiresAt: Date.now() + ttl,
		});
	}

	/** Flush all cached entries. */
	flush(): void {
		this.cache.clear();
	}

	/** Number of entries (including possibly expired). */
	get size(): number {
		return this.cache.size;
	}
}

/** Canonical cache key: "hostname:rrtype" */
function cacheKey(hostname: string, rrtype: string): string {
	return `${hostname}:${rrtype}`;
}
