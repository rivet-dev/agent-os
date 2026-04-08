import * as http from 'node:http';
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { HOST_BRIDGE_GLOBAL_KEYS } from '../src/bridge-contract.ts';
import { buildNetworkBridgeHandlers, buildNetworkSocketBridgeHandlers } from '../src/bridge-handlers.ts';
import { createDefaultNetworkAdapter } from '../src/default-network-adapter.ts';
import { createBudgetState } from '../src/isolate-bootstrap.ts';

describe('legacy networking removal policy', () => {
	it('keeps driver and bridge sources free of the legacy networking maps', () => {
		const driverSource = readFileSync(
			new URL('../src/driver.ts', import.meta.url),
			'utf8',
		);
		const bridgeNetworkSource = readFileSync(
			new URL('../src/bridge/network.ts', import.meta.url),
			'utf8',
		);
		const bridgeHandlersSource = readFileSync(
			new URL('../src/bridge-handlers.ts', import.meta.url),
			'utf8',
		);

		expect(driverSource).not.toContain('ownedServerPorts');
		expect(driverSource).not.toContain('upgradeSockets');
		expect(driverSource).not.toContain('const servers = new Map');
		expect(driverSource).not.toContain('http.createServer(');
		expect(driverSource).not.toContain('net.connect(');

		expect(bridgeNetworkSource).not.toContain('activeNetSockets');
		expect(bridgeNetworkSource).toContain('NET_SOCKET_REGISTRY_PREFIX');
		expect(bridgeNetworkSource).not.toContain('const directLoopbackConnectServer =');
		expect(bridgeNetworkSource).not.toContain('const directLoopbackUpgradeServer =');
		expect(bridgeNetworkSource).not.toContain('const directLoopbackServer =');
		expect(bridgeHandlersSource).not.toContain('adapter.httpServerListen');
		expect(bridgeHandlersSource).not.toContain('adapter.httpServerClose');
	});

	it('requires kernel socket routing for net socket bridge handlers', () => {
		expect(() =>
			buildNetworkSocketBridgeHandlers({
				dispatch: () => {},
			}),
		).toThrow('buildNetworkSocketBridgeHandlers requires a kernel socketTable and pid');

		expect(HOST_BRIDGE_GLOBAL_KEYS.netSocketConnectRaw).toBe('_netSocketConnectRaw');
	});

	it('requires kernel socket routing for HTTP server bridge handlers', () => {
		expect(() =>
			buildNetworkBridgeHandlers({
				networkAdapter: {
					async fetch() {
						return { ok: true, status: 200, statusText: 'OK', headers: {}, body: '', url: '', redirected: false };
					},
					async dnsLookup() {
						return { address: '127.0.0.1', family: 4 as const };
					},
					async httpRequest() {
						return { status: 200, statusText: 'OK', headers: {}, body: '', url: '' };
					},
				},
				budgetState: createBudgetState(),
				isolateJsonPayloadLimitBytes: 1024,
				activeHttpServerIds: new Set(),
				activeHttpServerClosers: new Map(),
				sendStreamEvent: () => {},
			}),
		).toThrow('buildNetworkBridgeHandlers requires a kernel socketTable and pid');

		expect(HOST_BRIDGE_GLOBAL_KEYS.networkHttpServerListenRaw).toBe('_networkHttpServerListenRaw');
	});

	it('allows loopback fetch and httpRequest via the injected kernel loopback checker', async () => {
		const server = http.createServer((_req, res) => {
			res.writeHead(200, { 'content-type': 'text/plain' });
			res.end('kernel-loopback-ok');
		});

		await new Promise<void>((resolve, reject) => {
			server.once('error', reject);
			server.listen(0, '127.0.0.1', () => resolve());
		});

		const address = server.address();
		if (!address || typeof address === 'string') {
			throw new Error('expected an inet listener address');
		}

		const adapter = createDefaultNetworkAdapter() as {
			__setLoopbackPortChecker?: (checker: (hostname: string, port: number) => boolean) => void;
			fetch: typeof createDefaultNetworkAdapter extends (...args: any[]) => infer T
				? T['fetch']
				: never;
			httpRequest: typeof createDefaultNetworkAdapter extends (...args: any[]) => infer T
				? T['httpRequest']
				: never;
		};
		adapter.__setLoopbackPortChecker?.((_hostname, port) => port === address.port);

		try {
			const fetchResult = await adapter.fetch(`http://127.0.0.1:${address.port}/`, {});
			expect(fetchResult.status).toBe(200);
			expect(fetchResult.body).toBe('kernel-loopback-ok');

			const httpResult = await adapter.httpRequest(`http://127.0.0.1:${address.port}/`, {});
			expect(httpResult.status).toBe(200);
			expect(httpResult.body).toBe('kernel-loopback-ok');
		} finally {
			await new Promise<void>((resolve, reject) => {
				server.close((err) => {
					if (err) reject(err);
					else resolve();
				});
			});
		}
	});
});
