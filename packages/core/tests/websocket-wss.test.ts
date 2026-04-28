import { createServer } from "node:https";
import { resolve } from "node:path";
import type { AddressInfo } from "node:net";
import { afterEach, describe, expect, test } from "vitest";
import { WebSocketServer } from "ws";
import { AgentOs } from "../src/index.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

const TLS_TEST_KEY_PEM = `-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQClvETzHfSyd1Y+
sjCfGkuyGxFMzwQlYjUrE0iwdMF774LYHFdpvtEo3sLOW6/b1xfXS/55jq+aggxS
v+vgtjrhGf/y33XzdrjxcVBRWIsgAtxMHsNKO4EQ/uA1g6zlbaSIu+ZWX3bkDuTi
K45VW69M0XSVyv8XFGYOcf8LTI87gTtXHuT92iej77IM2lHqLXCzQVr+NQ9yvXld
9yHlA2ZfYqhkSTLdDablqfgirrQIzZzLypSGQwZUU06nCtZ+dg6SNV4TGL4NqekD
jXR3BvmZu5l4sGAsNfFVjLx6hxsLt8uqn65sCAwBDdfucR+39+pHA+esj6NAWAFO
J9CB94sfAgMBAAECggEABQTA772x+a98aJSbvU2eCiwgp3tDTGB/bKj+U/2NGFQl
2aZuDTEugzbPnlEPb7BBNA9EiujDr4GNnvnZyimqecOASRn0J+Wp7wG35Waxe8wq
YJGz5y0LGPkmz+gHVcEusMdDz8y/PGOpEaIxAquukLxs89Y8SDYhawGPsAdm9O3F
4a+aosyQwS26mkZ/1WZOTsOVd4A1/1pxBvsANURj+pq7ed/1WqgrZBN/BG1TX5Xm
DZeYy01kTCMWtcAb4f8PxGpbkSGMvBb+Mj5XtZByvfQeC+Cs5ECXhmJtVaYVUHhT
vI0oTMGvit9ffoYNds0qTeZpEeineaDH3sD16D037QKBgQDX5b65KfIVH0/WvcbJ
Gx2Wh7knXdDBky40wdq4buKK+ImzPPRxOsQ+xEMgEaZs8gb7LBapbB0cZ+YsKBOt
4FY86XQU5V5ju2ntldIIIaugIGgvGS0jdRMH3ux6iEjPZE6Fm7/s8bjIgqB7keWh
1rcZwDrwMzqwAUoBTJX58OY/fQKBgQDEhT5U7TqgEFVSspYh8c8yVRV9udiphPH3
3XIbo9iV3xzNFdwtNHC+2eLM+4J3WKjhB0UvzrlIegSqKPIsy+0nD1uzaU+O72gg
7+NKSh0RT61UDolk+P4s/2+5tnZqSNYO7Sd/svE/rkwIEtDEI5tb1nqq75h/HDEW
k56GHAxvywKBgGmGmTdmIjZizKJYti4b+9VU15I/T8ceCmqtChw1zrNAkgWy2IPz
xnIreefV2LPNhM4GGbmL55q3yhBxMlU9nsk9DokcJ4u10ivXnAZvdrTYwjOrKZ34
HmotcwbdUEFWdO7nVuMYr0oKVyivAj+ddHe4ttYrJBddOe/yoCe/sLr9AoGBAKHL
IVpCRXXqfJStOzWPI4rIyfzMuTg3oA71XjCrYHFjUw715GPDPN+j+znQB8XCVKeP
mMKXa6vj6Vs+gsOm0QTLfC/lj/6Z1Bzp4zMSeYP7GTSPE0bySDE7y/wV4L/4X2PC
lDZqWHyZPzeWZhJVTl754dxBjkd4KmHv/x9ikEqpAoGBAJNA0u0fKhdWDz32+a2F
+plJ18kQvGuwKFWIIVHBDc0wCxLKWKr5wgkhdcAEpy4mgosiZ09DzV/OpQBBHVWZ
v/Cn/DwZyoiXIi5onf7AqWIhw+aem+oMbugbSIYqDwYkwnN79tsza0KC1ScphIuf
vKoOAdY4xOcG9BEZZoKVOa8R
-----END PRIVATE KEY-----
`;

const TLS_TEST_CERT_PEM = `-----BEGIN CERTIFICATE-----
MIIDCTCCAfGgAwIBAgIUJqRgTEIlpbfqbQnyo9hxLyIn3qYwDQYJKoZIhvcNAQEL
BQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDQwNTA3MTAwOVoXDTI2MDQw
NjA3MTAwOVowFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEF
AAOCAQ8AMIIBCgKCAQEApbxE8x30sndWPrIwnxpLshsRTM8EJWI1KxNIsHTBe++C
2BxXab7RKN7Czluv29cX10v+eY6vmoIMUr/r4LY64Rn/8t9183a48XFQUViLIALc
TB7DSjuBEP7gNYOs5W2kiLvmVl925A7k4iuOVVuvTNF0lcr/FxRmDnH/C0yPO4E7
Vx7k/dono++yDNpR6i1ws0Fa/jUPcr15Xfch5QNmX2KoZEky3Q2m5an4Iq60CM2c
y8qUhkMGVFNOpwrWfnYOkjVeExi+DanpA410dwb5mbuZeLBgLDXxVYy8eocbC7fL
qp+ubAgMAQ3X7nEft/fqRwPnrI+jQFgBTifQgfeLHwIDAQABo1MwUTAdBgNVHQ4E
FgQUwViZyKE6S2vgTAkexnZFccSwoPMwHwYDVR0jBBgwFoAUwViZyKE6S2vgTAke
xnZFccSwoPMwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAQEAadmK
3Ugrvep6glHAfgPP54um9cjJZQZDPn5I7yvgDr/Zp/u/UMW/OUKSfL1VNHlbAVLc
Yzq2RVTrJKObiTSoy99OzYkEdgfuEBBP7XBEQlqoOGYNRR+IZXBBiQ+m9CtajNwQ
G6mr9//zZtV1y2UUBgtxVpry5iOekpkr8iXyDLnGpS2gKL5dwXCzWCKVCO3qVotn
r6FBg4DCBMkwO6xOVN2yInPd6CPy/JAUPW50zWPnn4DKfeAAU0C+E75HN65jozdi
12yT4K772P8oSecGPInZhqJgOv1q0BDG8gccOxX1PA4sE00Enqlbvxz7sku9y4zp
ykAheWCsAteSEWVc0w==
-----END CERTIFICATE-----
`;

const GUEST_SCRIPT = String.raw`
import WebSocket from "ws";

const wsUrl = process.env.WS_URL;
if (!wsUrl) {
  throw new Error("missing WS_URL");
}

const reply = await new Promise((resolve, reject) => {
  const socket = new WebSocket(wsUrl, {
    rejectUnauthorized: false,
    headers: {
      "User-Agent": "agent-os-wss-test",
    },
  });
  const timer = setTimeout(() => {
    reject(new Error("timed out waiting for websocket reply"));
  }, 15_000);

  socket.once("open", () => {
    socket.send(JSON.stringify({ kind: "ping" }));
  });

  socket.once("message", (data) => {
    clearTimeout(timer);
    try {
      socket.close();
    } catch {}
    resolve(data.toString());
  });

  socket.once("error", (error) => {
    clearTimeout(timer);
    reject(error);
  });

  socket.once("close", (code, reason) => {
    console.log("WS_CLOSE:" + JSON.stringify({ code, reason: String(reason || "") }));
  });
});

console.log("WS_REPLY:" + reply);
`;

describe("guest websocket over wss", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		if (vm) {
			await vm.dispose();
			vm = null;
		}
	});

	test("connects to a host wss endpoint and exchanges a message", async () => {
		const server = createServer({
			key: TLS_TEST_KEY_PEM,
			cert: TLS_TEST_CERT_PEM,
		});
		const wss = new WebSocketServer({ noServer: true });

		wss.on("connection", (socket) => {
			socket.once("message", (data) => {
				socket.send(
					JSON.stringify({
						ok: true,
						payload: data.toString(),
					}),
				);
			});
		});

		server.on("upgrade", (request, socket, head) => {
			wss.handleUpgrade(request, socket, head, (client) => {
				wss.emit("connection", client, request);
			});
		});

		await new Promise<void>((resolvePromise) => {
			server.listen(0, "127.0.0.1", () => resolvePromise());
		});

		try {
			const port = (server.address() as AddressInfo).port;
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				loopbackExemptPorts: [port],
				permissions: {
					fs: "allow",
					childProcess: "allow",
					env: "allow",
					network: {
						default: "deny",
						rules: [
							{
								mode: "allow",
								patterns: [
									`dns://localhost`,
									`tcp://localhost:${port}`,
								],
							},
						],
					},
				},
			});
			await vm.writeFile("/tmp/websocket-wss-test.mjs", GUEST_SCRIPT);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn("node", ["/tmp/websocket-wss-test.mjs"], {
				env: {
					WS_URL: `wss://localhost:${port}`,
				},
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);

			const replyLine = stdout
				.split("\n")
				.find((line) => line.startsWith("WS_REPLY:"));
			expect(replyLine, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBeTruthy();
			expect(JSON.parse(replyLine!.slice("WS_REPLY:".length))).toEqual({
				ok: true,
				payload: JSON.stringify({ kind: "ping" }),
			});
		} finally {
			await new Promise<void>((resolvePromise, reject) => {
				wss.close((error) => {
					if (error) {
						reject(error);
						return;
					}
					resolvePromise();
				});
			});
			await new Promise<void>((resolvePromise, reject) => {
				server.close((error) => {
					if (error) {
						reject(error);
						return;
					}
					resolvePromise();
				});
			});
		}
	});
});
