import { createHash, createHmac, timingSafeEqual } from "node:crypto";
import { createServer, type IncomingHttpHeaders } from "node:http";

export interface MockS3Request {
	method: string;
	path: string;
	query: string;
	key: string;
}

export interface MockS3ServerHandle {
	accessKeyId: string;
	bucket: string;
	endpoint: string;
	secretAccessKey: string;
	objectKeys(): string[];
	requests(): MockS3Request[];
	stop(): Promise<void>;
}

const DEFAULT_BUCKET = "test-bucket";
const DEFAULT_ACCESS_KEY_ID = "minioadmin";
const DEFAULT_SECRET_ACCESS_KEY = "minioadmin";
const EMPTY_PAYLOAD_HASH = sha256Hex("");
const SUPPORTED_METHODS = new Set(["GET", "PUT", "DELETE"]);

interface ParsedAuthorization {
	accessKeyId: string;
	date: string;
	region: string;
	service: string;
	terminal: string;
	signedHeaders: string[];
	signature: string;
}

function decodePath(pathname: string): string {
	try {
		return decodeURIComponent(pathname.replace(/\+/g, " "));
	} catch {
		return pathname;
	}
}

function sha256Hex(data: string | Buffer): string {
	return createHash("sha256").update(data).digest("hex");
}

function hmac(key: string | Buffer, data: string): Buffer {
	return createHmac("sha256", key).update(data).digest();
}

function normalizeHeaderValue(value: string | string[] | undefined): string {
	if (value == null) {
		return "";
	}
	return (Array.isArray(value) ? value.join(",") : value)
		.trim()
		.replace(/\s+/g, " ");
}

function encodeAwsComponent(value: string): string {
	return encodeURIComponent(value).replace(
		/[!'()*]/g,
		(char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`,
	);
}

function canonicalQueryString(searchParams: URLSearchParams): string {
	return [...searchParams.entries()]
		.sort(([aKey, aValue], [bKey, bValue]) =>
			aKey === bKey ? aValue.localeCompare(bValue) : aKey.localeCompare(bKey),
		)
		.map(([key, value]) => `${encodeAwsComponent(key)}=${encodeAwsComponent(value)}`)
		.join("&");
}

function parseAuthorization(
	authorization: string | undefined,
): ParsedAuthorization {
	if (!authorization) {
		throw new Error("missing Authorization header");
	}

	const [algorithm, ...parts] = authorization.split(/\s+/);
	if (algorithm !== "AWS4-HMAC-SHA256") {
		throw new Error(`unsupported authorization algorithm: ${algorithm}`);
	}

	const kv = new Map<string, string>();
	for (const part of parts.join(" ").split(",")) {
		const [rawKey, rawValue] = part.trim().split("=");
		if (rawKey && rawValue) {
			kv.set(rawKey, rawValue);
		}
	}

	const credential = kv.get("Credential");
	const signedHeaders = kv.get("SignedHeaders");
	const signature = kv.get("Signature");
	if (!credential || !signedHeaders || !signature) {
		throw new Error("missing SigV4 credential components");
	}

	const credentialParts = credential.split("/");
	if (credentialParts.length !== 5) {
		throw new Error(`invalid credential scope: ${credential}`);
	}

	const [accessKeyId, date, region, service, terminal] = credentialParts;
	return {
		accessKeyId,
		date,
		region,
		service,
		terminal,
		signedHeaders: signedHeaders.split(";").filter(Boolean),
		signature,
	};
}

function buildCanonicalHeaders(
	headers: IncomingHttpHeaders,
	signedHeaders: string[],
): string {
	return signedHeaders
		.map((headerName) => {
			const value = normalizeHeaderValue(headers[headerName]);
			if (!value) {
				throw new Error(`missing signed header: ${headerName}`);
			}
			return `${headerName}:${value}\n`;
		})
		.join("");
}

function canonicalPath(pathname: string): string {
	return pathname
		.split("/")
		.map((segment) => encodeAwsComponent(decodeURIComponent(segment)))
		.join("/");
}

function verifySigV4(options: {
	accessKeyId: string;
	body: Buffer;
	headers: IncomingHttpHeaders;
	method: string;
	pathname: string;
	query: string;
	secretAccessKey: string;
}) {
	const parsed = parseAuthorization(
		normalizeHeaderValue(options.headers.authorization),
	);
	if (parsed.accessKeyId !== options.accessKeyId) {
		throw new Error(
			`unexpected access key id: ${parsed.accessKeyId} (expected ${options.accessKeyId})`,
		);
	}
	if (parsed.service !== "s3" || parsed.terminal !== "aws4_request") {
		throw new Error(
			`unexpected credential scope: ${parsed.service}/${parsed.terminal}`,
		);
	}

	const payloadHash = normalizeHeaderValue(
		options.headers["x-amz-content-sha256"],
	);
	if (!payloadHash) {
		throw new Error("missing x-amz-content-sha256 header");
	}

	const expectedPayloadHash = sha256Hex(options.body);
	if (
		payloadHash !== "UNSIGNED-PAYLOAD" &&
		payloadHash !== expectedPayloadHash
	) {
		throw new Error(
			`payload hash mismatch: expected ${expectedPayloadHash}, got ${payloadHash}`,
		);
	}

	const xAmzDate = normalizeHeaderValue(options.headers["x-amz-date"]);
	if (!xAmzDate) {
		throw new Error("missing x-amz-date header");
	}
	if (!xAmzDate.startsWith(parsed.date)) {
		throw new Error(
			`x-amz-date ${xAmzDate} does not match credential date ${parsed.date}`,
		);
	}

	const canonicalRequest = [
		options.method,
		canonicalPath(options.pathname),
		options.query,
		buildCanonicalHeaders(options.headers, parsed.signedHeaders),
		parsed.signedHeaders.join(";"),
		payloadHash,
	].join("\n");
	const stringToSign = [
		"AWS4-HMAC-SHA256",
		xAmzDate,
		`${parsed.date}/${parsed.region}/${parsed.service}/${parsed.terminal}`,
		sha256Hex(canonicalRequest),
	].join("\n");

	const signingKey = hmac(
		hmac(
			hmac(
				hmac(`AWS4${options.secretAccessKey}`, parsed.date),
				parsed.region,
			),
			parsed.service,
		),
		parsed.terminal,
	);
	const expectedSignature = createHmac("sha256", signingKey)
		.update(stringToSign)
		.digest("hex");

	const actual = Buffer.from(parsed.signature, "hex");
	const expected = Buffer.from(expectedSignature, "hex");
	if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) {
		throw new Error(
			`signature mismatch: expected ${expectedSignature}, got ${parsed.signature}`,
		);
	}
}

function xmlError(code: string, message: string): Buffer {
	return Buffer.from(
		`<Error><Code>${code}</Code><Message>${message}</Message></Error>`,
	);
}

export async function startMockS3Server(): Promise<MockS3ServerHandle> {
	const objects = new Map<string, Buffer>();
	const requestLog: MockS3Request[] = [];
	const protocolViolations: string[] = [];

	const server = createServer(async (request, response) => {
		const method = request.method ?? "GET";
		const target = new URL(request.url ?? "/", "http://127.0.0.1");
		const pathname = target.pathname;
		const decodedPath = decodePath(pathname);
		const pathWithoutLeadingSlash = decodedPath.replace(/^\/+/, "");
		const [bucket, ...keyParts] = pathWithoutLeadingSlash.split("/");
		const key = keyParts.join("/");
		const query = canonicalQueryString(target.searchParams);
		const targetSummary = `${method} ${target.pathname}${target.search}`;

		const fail = (status: number, code: string, message: string) => {
			protocolViolations.push(`${targetSummary}: ${message}`);
			const body = xmlError(code, message);
			response.writeHead(status, {
				"Content-Type": "application/xml",
				"Content-Length": String(body.length),
				"x-amz-request-id": "test",
			});
			response.end(body);
		};

		if (!SUPPORTED_METHODS.has(method)) {
			fail(
				501,
				"NotImplemented",
				`unsupported S3 method ${method}; update the local harness before adding new protocol calls`,
			);
			return;
		}
		const expectedOperationId =
			method === "GET"
				? "GetObject"
				: method === "PUT"
					? "PutObject"
					: "DeleteObject";
		if (query && query !== `x-id=${expectedOperationId}`) {
			fail(
				501,
				"NotImplemented",
				`unsupported S3 query string '${query}'; only x-id=${expectedOperationId} is allowed for ${method}`,
			);
			return;
		}
		if (bucket !== DEFAULT_BUCKET || !key) {
			fail(
				400,
				"InvalidURI",
				`expected path-style /${DEFAULT_BUCKET}/<key> request, got ${decodedPath}`,
			);
			return;
		}

		const chunks: Buffer[] = [];
		for await (const chunk of request) {
			chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
		}
		const body = Buffer.concat(chunks);

		try {
			verifySigV4({
				accessKeyId: DEFAULT_ACCESS_KEY_ID,
				body,
				headers: request.headers,
				method,
				pathname,
				query,
				secretAccessKey: DEFAULT_SECRET_ACCESS_KEY,
			});
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			fail(403, "SignatureDoesNotMatch", message);
			return;
		}

		requestLog.push({ method, path: decodedPath, query, key });

		switch (method) {
			case "GET": {
				const stored = objects.get(key);
				if (!stored) {
					const body = xmlError("NoSuchKey", "missing");
					response.writeHead(404, {
						"Content-Type": "application/xml",
						"Content-Length": String(body.length),
						"x-amz-request-id": "test",
					});
					response.end(body);
					return;
				}

				response.writeHead(200, {
					"Content-Type": "application/octet-stream",
					"Content-Length": String(stored.length),
					"x-amz-request-id": "test",
				});
				response.end(stored);
				return;
			}
			case "PUT": {
				objects.set(key, body);
				response.writeHead(200, {
					"Content-Type": "application/xml",
					"Content-Length": "0",
					"x-amz-request-id": "test",
				});
				response.end();
				return;
			}
			case "DELETE": {
				objects.delete(key);
				response.writeHead(204, {
					"Content-Type": "application/xml",
					"Content-Length": "0",
					"x-amz-request-id": "test",
				});
				response.end();
				return;
			}
		}
	});

	await new Promise<void>((resolve, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", () => {
			server.off("error", reject);
			resolve();
		});
	});

	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("Mock S3 server did not expose a TCP address.");
	}

	return {
		accessKeyId: DEFAULT_ACCESS_KEY_ID,
		bucket: DEFAULT_BUCKET,
		endpoint: `http://127.0.0.1:${address.port}`,
		secretAccessKey: DEFAULT_SECRET_ACCESS_KEY,
		objectKeys() {
			return [...objects.keys()].sort();
		},
		requests() {
			return [...requestLog];
		},
		stop() {
			return new Promise<void>((resolve, reject) => {
				server.close((error) => {
					if (error) {
						reject(error);
						return;
					}
					if (protocolViolations.length > 0) {
						reject(
							new Error(
								`Strict mock S3 server saw unsupported protocol usage:\n- ${protocolViolations.join("\n- ")}`,
							),
						);
						return;
					}
					resolve();
				});
			});
		},
	};
}

export const __mockS3Internals = {
	EMPTY_PAYLOAD_HASH,
	parseAuthorization,
	verifySigV4,
};
