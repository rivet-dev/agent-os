// Nightly: requires a non-core registry command.
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { execSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import {
	existsSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import {
	createServer,
	type IncomingMessage,
	type Server,
	type ServerResponse,
} from "node:http";
import {
	createServer as createHttpsServer,
	type Server as HttpsServer,
} from "node:https";
import {
	createServer as createTlsServer,
	type Server as TlsServer,
} from "node:tls";
import {
	createServer as createNetServer,
	type Server as NetServer,
} from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { gzipSync } from "node:zlib";
import { createWasmVmRuntime } from "@rivet-dev/agentos-test-harness";
import {
	allowAll,
	C_BUILD_DIR,
	COMMANDS_DIR,
	createInMemoryFileSystem,
	createKernel,
	describeIf,
	itIf,
} from "@rivet-dev/agentos-test-harness";
import type { Kernel } from "@rivet-dev/agentos-test-harness";

const WGET_COMMAND_DIRS = [C_BUILD_DIR, COMMANDS_DIR].filter((dir) =>
	existsSync(dir),
);
const hasWgetBinary = WGET_COMMAND_DIRS.some((dir) =>
	existsSync(resolve(dir, "wget")),
);
const WGET_EXEC_TIMEOUT_MS = 10_000;

let hasOpenssl = false;
try {
	execSync("openssl version", { stdio: "pipe" });
	hasOpenssl = true;
} catch {
	hasOpenssl = false;
}

// A long, highly compressible payload so the gzipped body is clearly distinct
// from the plaintext (proving wget actually inflated it via zlib).
const COMPRESSION_PAYLOAD =
	"agentos-wget-compression " +
	"the quick brown fox jumps over the lazy dog. ".repeat(64);

// Build a real CA and a leaf cert signed by it, with a SAN covering the
// 127.0.0.1 loopback endpoint the tests connect to. This lets wget's mbedTLS
// backend perform genuine chain + hostname verification, exactly like Linux
// wget against a private CA.
function makeCaSignedCert(caCommonName: string): {
	caPem: string;
	serverKey: string;
	serverCert: string;
} {
	const dir = mkdtempSync(join(tmpdir(), "wget-ca-"));
	try {
		execSync(
			`openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "${dir}/ca.key" 2>/dev/null`,
		);
		execSync(
			`openssl req -x509 -new -key "${dir}/ca.key" -days 3650 -subj "/CN=${caCommonName}" -out "${dir}/ca.crt" 2>/dev/null`,
		);
		execSync(
			`openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "${dir}/srv.key" 2>/dev/null`,
		);
		execSync(
			`openssl req -new -key "${dir}/srv.key" -subj "/CN=localhost" -out "${dir}/srv.csr" 2>/dev/null`,
		);
		writeFileSync(`${dir}/ext.cnf`, "subjectAltName=DNS:localhost,IP:127.0.0.1\n");
		execSync(
			`openssl x509 -req -in "${dir}/srv.csr" -CA "${dir}/ca.crt" -CAkey "${dir}/ca.key" ` +
				`-CAcreateserial -days 3650 -extfile "${dir}/ext.cnf" -out "${dir}/srv.crt" 2>/dev/null`,
		);
		return {
			caPem: readFileSync(`${dir}/ca.crt`, "utf8"),
			serverKey: readFileSync(`${dir}/srv.key`, "utf8"),
			serverCert: readFileSync(`${dir}/srv.crt`, "utf8"),
		};
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

function makeMutualTlsCerts(caCommonName: string): {
	caPem: string;
	serverKey: string;
	serverCert: string;
	clientKey: string;
	clientCert: string;
} {
	const dir = mkdtempSync(join(tmpdir(), "wget-mtls-"));
	try {
		execSync(
			`openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "${dir}/ca.key" 2>/dev/null`,
		);
		execSync(
			`openssl req -x509 -new -key "${dir}/ca.key" -days 3650 -subj "/CN=${caCommonName}" -out "${dir}/ca.crt" 2>/dev/null`,
		);
		for (const [name, commonName, extensions] of [
			["server", "localhost", "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n"],
			["client", "wget-client", "extendedKeyUsage=clientAuth\n"],
		] as const) {
			execSync(
				`openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "${dir}/${name}.key" 2>/dev/null`,
			);
			execSync(
				`openssl req -new -key "${dir}/${name}.key" -subj "/CN=${commonName}" -out "${dir}/${name}.csr" 2>/dev/null`,
			);
			writeFileSync(`${dir}/${name}.cnf`, extensions);
			execSync(
				`openssl x509 -req -in "${dir}/${name}.csr" -CA "${dir}/ca.crt" -CAkey "${dir}/ca.key" ` +
					`-CAcreateserial -days 3650 -extfile "${dir}/${name}.cnf" -out "${dir}/${name}.crt" 2>/dev/null`,
			);
		}
		return {
			caPem: readFileSync(`${dir}/ca.crt`, "utf8"),
			serverKey: readFileSync(`${dir}/server.key`, "utf8"),
			serverCert: readFileSync(`${dir}/server.crt`, "utf8"),
			clientKey: readFileSync(`${dir}/client.key`, "utf8"),
			clientCert: readFileSync(`${dir}/client.crt`, "utf8"),
		};
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

function generateSelfSignedCert(): { key: string; cert: string } {
	const keyPath = join(tmpdir(), `wget-test-key-${process.pid}-${Date.now()}.pem`);
	try {
		const key = execSync(
			"openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 2>/dev/null",
			{ encoding: "utf8" },
		);
		writeFileSync(keyPath, key);
		const cert = execSync(
			`openssl req -new -x509 -key "${keyPath}" -days 1 -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" 2>/dev/null`,
			{ encoding: "utf8" },
		);
		return { key, cert };
	} finally {
		try {
			unlinkSync(keyPath);
		} catch {
			// Best effort cleanup for test temp files.
		}
	}
}

describeIf(hasWgetBinary, "wget command", () => {
	let kernel: Kernel;
	let server: Server;
	let selfSignedServer: HttpsServer;
	let validHttpsServer: HttpsServer;
	let caHttpsServer: HttpsServer;
	let mutualTlsServer: HttpsServer;
	let handshakeStallServer: NetServer;
	let readStallServer: TlsServer;
	let ftpsControlServer: TlsServer;
	let ftpsDataServer: TlsServer;
	let port: number;
	let selfSignedPort: number;
	let validHttpsPort: number;
	let caHttpsPort: number;
	let mutualTlsPort: number;
	let handshakeStallPort: number;
	let readStallPort: number;
	let ftpsControlPort: number;
	let ftpsDataPort: number;
	// CA (PEM) trusted by the seeded /etc/ssl/certs/ca-certificates.crt bundle;
	// it signs validHttpsServer's leaf. caOnlyPem signs caHttpsServer's leaf and
	// is deliberately NOT in the bundle, so it verifies only via
	// --ca-certificate.
	let seededCaPem = "";
	let caOnlyPem = "";
	let clientKeyPem = "";
	let clientCertPem = "";
	let mutualCaPem = "";
	let ftpsDataSessionReused = false;

	beforeAll(async () => {
		server = createServer((req: IncomingMessage, res: ServerResponse) => {
			const url = req.url ?? "/";

			if (url === "/file.txt") {
				res.writeHead(200, { "Content-Type": "text/plain" });
				res.end("downloaded content");
				return;
			}

			if (url === "/data.json") {
				res.writeHead(200, { "Content-Type": "application/json" });
				res.end(JSON.stringify({ status: "ok" }));
				return;
			}

			if (url === "/gzip") {
				const body = gzipSync(Buffer.from(COMPRESSION_PAYLOAD));
				res.writeHead(200, {
					"Content-Type": "text/plain",
					"Content-Encoding": "gzip",
					"Content-Length": String(body.length),
				});
				res.end(body);
				return;
			}

			if (url === "/redirect") {
				res.writeHead(302, {
					Location: `http://127.0.0.1:${port}/redirected`,
				});
				res.end();
				return;
			}

			if (url === "/redirected") {
				res.writeHead(200, { "Content-Type": "text/plain" });
				res.end("arrived after redirect");
				return;
			}

			res.writeHead(404, { "Content-Type": "text/plain" });
			res.end("not found");
		});

		await new Promise<void>((resolveListen) =>
			server.listen(0, "127.0.0.1", resolveListen),
		);
		port = (server.address() as import("node:net").AddressInfo).port;

		// Accept TCP but never send a TLS ServerHello. Native Wget applies its
		// read timeout while handshaking; the mbedTLS backend must do the same.
		handshakeStallServer = createNetServer((socket) => {
			setTimeout(() => socket.destroy(), 3_000);
		});
		await new Promise<void>((resolveListen) =>
			handshakeStallServer.listen(0, "127.0.0.1", resolveListen),
		);
		handshakeStallPort = (
			handshakeStallServer.address() as import("node:net").AddressInfo
		).port;

		if (hasOpenssl) {
			// Self-signed leaf: no chain to any trusted CA -> must fail verify.
			const selfSigned = generateSelfSignedCert();
			selfSignedServer = createHttpsServer(
				{ key: selfSigned.key, cert: selfSigned.cert },
				(req, res) => {
					res.writeHead(200, { "Content-Type": "text/plain" });
					res.end("self-signed secure content");
				},
			);
			await new Promise<void>((resolveListen) =>
				selfSignedServer.listen(0, "127.0.0.1", resolveListen),
			);
			selfSignedPort = (
				selfSignedServer.address() as import("node:net").AddressInfo
			).port;

			// Leaf chaining to a CA seeded into the guest's bundle -> verifies
			// with no --no-check-certificate / --ca-certificate.
			const trusted = makeCaSignedCert("AgentOS Wget Test Root CA");
			seededCaPem = trusted.caPem;
			validHttpsServer = createHttpsServer(
				{
					key: trusted.serverKey,
					cert: trusted.serverCert,
					minVersion: "TLSv1.3",
				},
				(req, res) => {
					res.writeHead(200, { "Content-Type": "text/plain" });
					res.end("verified https content");
				},
			);
			await new Promise<void>((resolveListen) =>
				validHttpsServer.listen(0, "127.0.0.1", resolveListen),
			);
			validHttpsPort = (
				validHttpsServer.address() as import("node:net").AddressInfo
			).port;

			// Complete TLS and send a deliberately incomplete fixed-length body.
			// A read timeout is an error, not a clean EOF/truncated success.
			readStallServer = createTlsServer(
				{ key: trusted.serverKey, cert: trusted.serverCert },
				(socket) => {
					socket.write(
						"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\npartial",
					);
					setTimeout(() => socket.destroy(), 3_000);
				},
			);
			await new Promise<void>((resolveListen) =>
				readStallServer.listen(0, "127.0.0.1", resolveListen),
			);
			readStallPort = (
				readStallServer.address() as import("node:net").AddressInfo
			).port;

			// Leaf whose CA is provided ONLY via --ca-certificate (not in bundle).
			const caOnly = makeCaSignedCert("AgentOS Wget Cacert-Only CA");
			caOnlyPem = caOnly.caPem;
			caHttpsServer = createHttpsServer(
				{
					key: caOnly.serverKey,
					cert: caOnly.serverCert,
					maxVersion: "TLSv1.2",
				},
				(req, res) => {
					res.writeHead(200, { "Content-Type": "text/plain" });
					res.end("cacert https content");
				},
			);
			await new Promise<void>((resolveListen) =>
				caHttpsServer.listen(0, "127.0.0.1", resolveListen),
			);
			caHttpsPort = (
				caHttpsServer.address() as import("node:net").AddressInfo
			).port;

			const mutual = makeMutualTlsCerts("AgentOS Wget Mutual TLS CA");
			mutualCaPem = mutual.caPem;
			clientKeyPem = mutual.clientKey;
			clientCertPem = mutual.clientCert;
			mutualTlsServer = createHttpsServer(
				{
					key: mutual.serverKey,
					cert: mutual.serverCert,
					ca: mutual.caPem,
					requestCert: true,
					rejectUnauthorized: true,
				},
				(_req, res) => {
					res.writeHead(200, { "Content-Type": "text/plain" });
					res.end("mutual tls content");
				},
			);
			await new Promise<void>((resolveListen) =>
				mutualTlsServer.listen(0, "127.0.0.1", resolveListen),
			);
			mutualTlsPort = (
				mutualTlsServer.address() as import("node:net").AddressInfo
			).port;

			// Minimal implicit-FTPS endpoint. Control and data listeners share TLS
			// ticket keys; the data listener records whether Wget resumed the
			// control session, as native Wget does for protected data channels.
			const ticketKeys = randomBytes(48);
			let releaseData: (() => void) | undefined;
			const dataMayFlow = new Promise<void>((resolveData) => {
				releaseData = resolveData;
			});
			const ftpsPayload = "resumed ftps content\n";
			const ftpsTlsOptions = {
				key: trusted.serverKey,
				cert: trusted.serverCert,
				minVersion: "TLSv1.2" as const,
				maxVersion: "TLSv1.2" as const,
				ticketKeys,
			};
			ftpsDataServer = createTlsServer(ftpsTlsOptions, async (socket) => {
				ftpsDataSessionReused = socket.isSessionReused();
				await dataMayFlow;
				socket.end(ftpsPayload);
			});
			await new Promise<void>((resolveListen) =>
				ftpsDataServer.listen(0, "127.0.0.1", resolveListen),
			);
			ftpsDataPort = (
				ftpsDataServer.address() as import("node:net").AddressInfo
			).port;

			ftpsControlServer = createTlsServer(ftpsTlsOptions, (socket) => {
				let buffered = "";
				socket.write("220 AgentOS FTPS ready\r\n");
				socket.on("data", (chunk) => {
					buffered += chunk.toString("utf8");
					for (;;) {
						const newline = buffered.indexOf("\n");
						if (newline < 0) break;
						const line = buffered.slice(0, newline).trim();
						buffered = buffered.slice(newline + 1);
						const [command = "", ...args] = line.split(/\s+/);
						const argument = args.join(" ");
						switch (command.toUpperCase()) {
							case "USER": socket.write("331 Password required\r\n"); break;
							case "PASS": socket.write("230 Logged in\r\n"); break;
							case "SYST": socket.write("215 UNIX Type: L8\r\n"); break;
							case "PWD": socket.write('257 "/" is current directory\r\n'); break;
							case "TYPE":
							case "PBSZ":
							case "PROT":
							case "OPTS": socket.write("200 OK\r\n"); break;
							case "SIZE": socket.write(`213 ${Buffer.byteLength(ftpsPayload)}\r\n`); break;
							case "MDTM": socket.write("213 20260714000000\r\n"); break;
							case "EPSV": socket.write(`229 Entering Extended Passive Mode (|||${ftpsDataPort}|)\r\n`); break;
							case "PASV": socket.write(`227 Entering Passive Mode (127,0,0,1,${Math.floor(ftpsDataPort / 256)},${ftpsDataPort % 256})\r\n`); break;
							case "RETR":
								socket.write(`150 Opening data connection for ${argument}\r\n`);
								releaseData?.();
								setTimeout(() => socket.write("226 Transfer complete\r\n"), 25);
								break;
							case "QUIT": socket.end("221 Goodbye\r\n"); break;
							default: socket.write("200 OK\r\n"); break;
						}
					}
				});
			});
			await new Promise<void>((resolveListen) =>
				ftpsControlServer.listen(0, "127.0.0.1", resolveListen),
			);
			ftpsControlPort = (
				ftpsControlServer.address() as import("node:net").AddressInfo
			).port;
		}
	});

	afterAll(async () => {
		for (const s of [
			server,
			selfSignedServer,
			validHttpsServer,
			caHttpsServer,
			mutualTlsServer,
			handshakeStallServer,
			readStallServer,
			ftpsControlServer,
			ftpsDataServer,
		]) {
			if (s) {
				await new Promise<void>((resolveClose) => s.close(() => resolveClose()));
			}
		}
	});

	afterEach(async () => {
		await kernel?.dispose();
	});

	async function mountKernel() {
		const filesystem = createInMemoryFileSystem();
		await filesystem.mkdir("/tmp", { recursive: true });
		await filesystem.chmod("/tmp", 0o1777);
		await filesystem.mkdir("/workspace", { recursive: true });
		await filesystem.chown("/workspace", 1000, 1000);
		kernel = createKernel({
			filesystem,
			permissions: allowAll,
			loopbackExemptPorts: [
				port,
				selfSignedPort,
				validHttpsPort,
				caHttpsPort,
				mutualTlsPort,
				handshakeStallPort,
				readStallPort,
				ftpsControlPort,
				ftpsDataPort,
			].filter((p) => typeof p === "number"),
		});
		await kernel.mount(createWasmVmRuntime({ commandDirs: WGET_COMMAND_DIRS }));

		// Seed the Debian-shaped trust store the way the native VM bootstrap
		// does, so wget's default CA bundle resolves in-guest. Only the
		// "trusted" CA is placed here; the cacert-only CA is intentionally
		// absent.
		if (seededCaPem) {
			await filesystem.mkdir("/etc/ssl/certs", { recursive: true });
			await kernel.writeFile("/etc/ssl/certs/ca-certificates.crt", seededCaPem);
		}
		return filesystem;
	}

	it("downloads a file using the URL basename", async () => {
		const filesystem = await mountKernel();

		const result = await kernel.exec(`wget http://127.0.0.1:${port}/file.txt`, {
			timeout: WGET_EXEC_TIMEOUT_MS,
		});

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/file.txt")).toBe(
			"downloaded content",
		);
	}, 15_000);

	it("-O saves to the requested output path", async () => {
		const filesystem = await mountKernel();

		const result = await kernel.exec(
			`wget -O /workspace/output.txt http://127.0.0.1:${port}/data.json`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/output.txt")).toContain(
			'"status":"ok"',
		);
	}, 15_000);

	it("-q suppresses progress output", async () => {
		const filesystem = await mountKernel();

		const result = await kernel.exec(
			`wget -q -O /workspace/quiet.txt http://127.0.0.1:${port}/file.txt`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stderr).toBe("");
		expect(await filesystem.readTextFile("/workspace/quiet.txt")).toBe(
			"downloaded content",
		);
	}, 15_000);

	it("reports failure for a 404 URL", async () => {
		await mountKernel();

		const result = await kernel.exec(
			`wget http://127.0.0.1:${port}/missing.txt`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toMatch(/404|not found|error/i);
	}, 15_000);

	it("follows redirects by default", async () => {
		const filesystem = await mountKernel();

		const result = await kernel.exec(
			`wget -O /workspace/redirected.txt http://127.0.0.1:${port}/redirect`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/redirected.txt")).toBe(
			"arrived after redirect",
		);
	}, 15_000);

	it("--version reports the mbedTLS HTTPS backend", async () => {
		await mountKernel();

		const result = await kernel.exec("wget --version", {
			timeout: WGET_EXEC_TIMEOUT_MS,
		});

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(result.stdout).toContain("GNU Wget 1.24.5");
		// Real in-guest TLS: HTTPS is compiled in and the backend is mbedTLS.
		expect(result.stdout).toMatch(/\+https/);
		expect(result.stdout).toMatch(/ssl\/mbedtls/i);
	}, 15_000);

	it("--compression=auto inflates a gzip response body", async () => {
		const filesystem = await mountKernel();

		const result = await kernel.exec(
			`wget --compression=auto -O /workspace/gz.txt http://127.0.0.1:${port}/gzip`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/gz.txt")).toBe(
			COMPRESSION_PAYLOAD,
		);
	}, 15_000);

	it("times out a stalled TLS handshake instead of hanging", async () => {
		await mountKernel();

		const result = await kernel.exec(
			`wget --tries=1 --connect-timeout=1 --read-timeout=1 --no-check-certificate -O /workspace/handshake-timeout.txt https://127.0.0.1:${handshakeStallPort}/`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toMatch(/timed out|timeout/i);
	}, 15_000);

	itIf(
		hasOpenssl,
		"treats a stalled HTTPS response body as a timeout, not clean EOF",
		async () => {
			await mountKernel();

			const result = await kernel.exec(
				`wget --tries=1 --read-timeout=1 -O /workspace/truncated.txt https://127.0.0.1:${readStallPort}/`,
				{ timeout: WGET_EXEC_TIMEOUT_MS },
			);

			expect(result.exitCode).not.toBe(0);
			expect(result.stderr).toMatch(/timed out|timeout/i);
		},
		15_000,
	);

	itIf(hasOpenssl, "downloads over HTTPS verifying against the seeded CA bundle", async () => {
		const filesystem = await mountKernel();

		// No --no-check-certificate, no --ca-certificate: trust comes solely
		// from the seeded /etc/ssl/certs/ca-certificates.crt, like Debian wget.
		const result = await kernel.exec(
			`wget -O /workspace/secure.txt https://127.0.0.1:${validHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/secure.txt")).toBe(
			"verified https content",
		);
	}, 15_000);

	itIf(hasOpenssl, "fails with a real cert error on an untrusted (self-signed) server", async () => {
		await mountKernel();

		const result = await kernel.exec(
			`wget -O /workspace/nope.txt https://127.0.0.1:${selfSignedPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		// VERIFCERTERR -> WGET_EXIT_SSL_AUTH_FAIL == 5, the native taxonomy.
		expect(result.exitCode).toBe(5);
		expect(result.stderr).toMatch(/cannot verify|certificate|not trusted/i);
	}, 15_000);

	itIf(hasOpenssl, "--no-check-certificate accepts a self-signed server", async () => {
		const filesystem = await mountKernel();

		const result = await kernel.exec(
			`wget --no-check-certificate -O /workspace/insecure.txt https://127.0.0.1:${selfSignedPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/insecure.txt")).toBe(
			"self-signed secure content",
		);
	}, 15_000);

	itIf(hasOpenssl, "--ca-certificate trusts a server signed by that CA", async () => {
		const filesystem = await mountKernel();

		// caHttpsServer's CA is NOT in the seeded bundle, so this only passes if
		// --ca-certificate is honored (real file read + chain build in-guest).
		await kernel.writeFile("/tmp/cacert-only.pem", caOnlyPem);
		const result = await kernel.exec(
			`wget --ca-certificate=/tmp/cacert-only.pem -O /workspace/cacert.txt https://127.0.0.1:${caHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/cacert.txt")).toBe(
			"cacert https content",
		);
	}, 15_000);

	itIf(hasOpenssl, "--ca-certificate augments rather than replaces system trust", async () => {
		const filesystem = await mountKernel();

		// Linux Wget loads system trust first, then adds --ca-certificate. An
		// unrelated supplemental CA must not make an otherwise trusted server
		// fail verification.
		await kernel.writeFile("/tmp/additional-ca.pem", caOnlyPem);
		const result = await kernel.exec(
			`wget --ca-certificate=/tmp/additional-ca.pem -O /workspace/system-trust.txt https://127.0.0.1:${validHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/system-trust.txt")).toBe(
			"verified https content",
		);
	}, 15_000);

	itIf(hasOpenssl, "--ca-certificate with the wrong CA still fails verification", async () => {
		await mountKernel();

		// Point --ca-certificate at the seeded CA, which did NOT sign
		// caHttpsServer's leaf.
		await kernel.writeFile("/tmp/wrong-ca.pem", seededCaPem);
		const result = await kernel.exec(
			`wget --ca-certificate=/tmp/wrong-ca.pem -O /workspace/wrong.txt https://127.0.0.1:${caHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode).toBe(5);
		expect(result.stderr).toMatch(/cannot verify|certificate|not trusted/i);
	}, 15_000);

	itIf(hasOpenssl, "--secure-protocol=TLSv1_2 remains a minimum and permits TLS 1.3", async () => {
		const filesystem = await mountKernel();
		const result = await kernel.exec(
			`wget --secure-protocol=TLSv1_2 -O /workspace/tls13.txt https://127.0.0.1:${validHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/tls13.txt")).toBe(
			"verified https content",
		);
	}, 15_000);

	itIf(hasOpenssl, "rejects unavailable SSLv3 instead of silently upgrading to TLS", async () => {
		await mountKernel();
		const result = await kernel.exec(
			`wget --secure-protocol=SSLv3 -O /workspace/old.txt https://127.0.0.1:${validHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toMatch(/does not support requested protocol|SSLv3/i);
	}, 15_000);

	itIf(hasOpenssl, "honors common OpenSSL HIGH/exclusion cipher policy syntax", async () => {
		const filesystem = await mountKernel();
		const result = await kernel.exec(
			`wget --ciphers='HIGH:!aNULL:!RC4:!MD5:!SRP:!PSK' -O /workspace/cipher.txt https://127.0.0.1:${validHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/cipher.txt")).toBe(
			"verified https content",
		);
	}, 15_000);

	itIf(hasOpenssl, "translates an explicit OpenSSL TLS 1.2 cipher name", async () => {
		const filesystem = await mountKernel();
		await kernel.writeFile("/tmp/cipher-ca.pem", caOnlyPem);
		const result = await kernel.exec(
			`wget --ciphers=ECDHE-RSA-AES128-GCM-SHA256 --ca-certificate=/tmp/cipher-ca.pem ` +
				`-O /workspace/explicit-cipher.txt https://127.0.0.1:${caHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/explicit-cipher.txt")).toBe(
			"cacert https content",
		);
	}, 15_000);

	itIf(hasOpenssl, "leaves TLS 1.3 enabled when --ciphers names a TLS 1.2 suite", async () => {
		const filesystem = await mountKernel();
		const result = await kernel.exec(
			`wget --ciphers=ECDHE-RSA-AES128-GCM-SHA256 -O /workspace/tls13-cipher.txt ` +
				`https://127.0.0.1:${validHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/tls13-cipher.txt")).toBe(
			"verified https content",
		);
	}, 15_000);

	itIf(hasOpenssl, "fails explicitly for an unsupported cipher policy token", async () => {
		await mountKernel();
		const result = await kernel.exec(
			`wget --ciphers=NOT-A-CIPHER -O /workspace/bad-cipher.txt https://127.0.0.1:${validHttpsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode).not.toBe(0);
		expect(result.stderr).toMatch(/unsupported.*cipher policy token|NOT-A-CIPHER/i);
	}, 15_000);

	itIf(hasOpenssl, "presents --certificate and --private-key to a mutual-TLS server", async () => {
		const filesystem = await mountKernel();
		await kernel.writeFile("/tmp/mutual-ca.pem", mutualCaPem);
		await kernel.writeFile("/tmp/client.crt", clientCertPem);
		await kernel.writeFile("/tmp/client.key", clientKeyPem);
		const result = await kernel.exec(
			`wget --ca-certificate=/tmp/mutual-ca.pem --certificate=/tmp/client.crt ` +
				`--private-key=/tmp/client.key -O /workspace/mutual.txt https://127.0.0.1:${mutualTlsPort}/file`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/mutual.txt")).toBe(
			"mutual tls content",
		);
	}, 15_000);

	itIf(hasOpenssl, "resumes the FTPS control session on the protected data channel", async () => {
		const filesystem = await mountKernel();
		const result = await kernel.exec(
			`wget --ftps-implicit -O /workspace/ftps.txt ftps://127.0.0.1:${ftpsControlPort}/file.txt`,
			{ timeout: WGET_EXEC_TIMEOUT_MS },
		);

		expect(result.exitCode, result.stderr || result.stdout).toBe(0);
		expect(await filesystem.readTextFile("/workspace/ftps.txt")).toBe(
			"resumed ftps content\n",
		);
		expect(ftpsDataSessionReused).toBe(true);
	}, 20_000);
});
