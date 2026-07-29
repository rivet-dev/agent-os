import { exposeCustomGlobal } from "../global-exposure.js";
import { streamStdlibModuleNs } from "../prelude.js";
import {
	createBadRequestResponseBuffer,
	createTypeErrorWithCode,
	DirectTunnelSocket,
	debugBridgeNetwork,
	dispatchLoopbackServerRequest,
	formatReceivedType,
	parseLoopbackRequestBuffer,
	ServerIncomingMessage,
	serializeLoopbackResponse,
} from "./http.js";
import { http2Dispatch } from "./http2.js";
import {
	registerCapabilityReadiness,
	unregisterCapabilityReadiness,
} from "./readiness.js";
import { setImmediate } from "./timers.js";

const CanonicalDuplex =
	streamStdlibModuleNs.Duplex ?? streamStdlibModuleNs.default?.Duplex;

if (typeof CanonicalDuplex !== "function") {
	throw new Error(
		"node:stream did not provide the canonical Duplex constructor",
	);
}

var NET_SOCKET_REGISTRY_PREFIX = "__agentOsNetSocket:";

var NET_SERVER_HANDLE_PREFIX = "net-server:";

var nextNetSocketLivenessId = 0;

var registeredNetSockets = /* @__PURE__ */ new Map();

var registeredNetServersByPort = /* @__PURE__ */ new Map();

var registeredNetServersByPath = /* @__PURE__ */ new Map();

var registeredNetServersById = /* @__PURE__ */ new Map();

function getRegisteredNetSocket(socketId) {
	return globalThis[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`];
}

function registerNetSocket(socketId, socket) {
	globalThis[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`] = socket;
	registeredNetSockets.set(socketId, socket);
	registerCapabilityReadiness(socket, () => {
		countNetBridgeMetric("readEventWakeups");
		wakeSocketBridgeReads(socket);
	});
}

function unregisterNetSocket(socketId) {
	unregisterCapabilityReadiness(registeredNetSockets.get(socketId));
	delete globalThis[`${NET_SOCKET_REGISTRY_PREFIX}${socketId}`];
	registeredNetSockets.delete(socketId);
}

function isRegisteredNetSocket(socket) {
	return (
		!!socket &&
		socket._socketId !== 0 &&
		registeredNetSockets.get(socket._socketId) === socket
	);
}

function queueSocketBridgeReadPump(socket, origin) {
	if (!socket._applicationReadDemand || socket._bridgeReadPumpQueued) {
		countNetBridgeMetric("readPumpQueueCoalesced");
		return;
	}
	socket._bridgeReadPumpQueued = true;
	queueMicrotask(() => {
		socket._bridgeReadPumpQueued = false;
		if (
			!socket.destroyed &&
			!socket._bridgeReleased &&
			!socket._closeEmitted &&
			socket._applicationReadDemand &&
			isRegisteredNetSocket(socket) &&
			socket._socketId !== 0
		) {
			socket._nextReadPumpOrigin = origin;
			void socket._pumpBridgeReads();
		}
	});
}

function netServerUnixPath(server) {
	const address = server?._address;
	if (typeof address === "string") {
		return address;
	}
	const path = address?.path;
	return typeof path === "string" ? path : void 0;
}

function registerNetServer(server) {
	if (server?._serverId) {
		registeredNetServersById.set(server._serverId, server);
	}
	const port = server?._address?.port;
	if (typeof port === "number") {
		registeredNetServersByPort.set(port, server);
	}
	const path = netServerUnixPath(server);
	if (typeof path === "string") {
		registeredNetServersByPath.set(path, server);
	}
	registerCapabilityReadiness(server, () => wakeNetServerAccept(server));
}

function unregisterNetServer(server) {
	unregisterCapabilityReadiness(server);
	if (
		server?._serverId &&
		registeredNetServersById.get(server._serverId) === server
	) {
		registeredNetServersById.delete(server._serverId);
	}
	const port = server?._address?.port;
	if (
		typeof port === "number" &&
		registeredNetServersByPort.get(port) === server
	) {
		registeredNetServersByPort.delete(port);
	}
	const path = netServerUnixPath(server);
	if (
		typeof path === "string" &&
		registeredNetServersByPath.get(path) === server
	) {
		registeredNetServersByPath.delete(path);
	}
}

function wakeSocketBridgeReads(socket) {
	countNetBridgeMetric("readWakeAttempts");
	if (
		!socket ||
		socket.destroyed ||
		socket._bridgeReleased ||
		socket._closeEmitted ||
		socket._socketId === 0 ||
		socket._loopbackServer ||
		socket._loopbackHttpTarget
	) {
		countNetBridgeMetric("readWakeInvalidTargets");
		return;
	}
	socket._pendingBridgeWake = true;
	socket._pendingBridgeWakeRetries = 0;
	if (socket._bridgeReadLoopRunning) {
		countNetBridgeMetric("readWakeAlreadyRunning");
	}
	{
		countNetBridgeMetric("readWakeCoalesced");
		if (socket._connected) {
			countNetBridgeMetric("readWakeNoTimerConnected");
		}
		if (socket.connecting) {
			countNetBridgeMetric("readWakeNoTimerConnecting");
		}
		countNetBridgeMetric(
			socket._refed ? "readWakeNoTimerRefed" : "readWakeNoTimerUnrefed",
		);
		const hasDataListener = socket.listenerCount("data") > 0;
		const hasReadableListener = socket.listenerCount("readable") > 0;
		if (hasDataListener) {
			countNetBridgeMetric("readWakeNoTimerHasDataListener");
		}
		if (hasReadableListener) {
			countNetBridgeMetric("readWakeNoTimerHasReadableListener");
		}
		if (socket._bridgeWriteFlushScheduled) {
			countNetBridgeMetric("readWakeNoTimerPendingWriteFlush");
			countNetBridgeMetric(
				"readWakeNoTimerPendingWriteBytes",
				socket._pendingBridgeWriteBytes,
			);
		}
		if (
			!socket._bridgeReadPumpStarted &&
			socket._firstReadNoTimerWakeAtUs === 0 &&
			isNetBridgeMetricsEnabled()
		) {
			socket._firstReadNoTimerWakeAtUs = netBridgeNowUs();
		}
		if (
			!socket._bridgeReadPumpStarted &&
			socket._connected &&
			(hasDataListener || hasReadableListener)
		) {
			if (isNetBridgeMetricsEnabled()) {
				countNetBridgeMetric("readFirstPumpScheduleCandidates");
			}
			if (socket._bridgeReadFirstPumpBenchmarkScheduled) {
				if (isNetBridgeMetricsEnabled()) {
					countNetBridgeMetric("readFirstPumpScheduleAlreadyScheduled");
				}
			} else {
				if (isNetBridgeMetricsEnabled()) {
					countNetBridgeMetric("readFirstPumpScheduleQueued");
				}
				socket._bridgeReadFirstPumpBenchmarkScheduled = true;
				const queuedAtUs = netBridgeNowUs();
				queueMicrotask(() => {
					if (isNetBridgeMetricsEnabled()) {
						countNetBridgeMetric("readFirstPumpScheduleRuns");
					}
					const runAtUs = netBridgeNowUs();
					const queuedToRunUs = Math.max(0, runAtUs - queuedAtUs);
					if (isNetBridgeMetricsEnabled()) {
						countNetBridgeMetric(
							"readFirstPumpScheduleQueuedToRunUs",
							queuedToRunUs,
						);
						maxNetBridgeMetric(
							"readFirstPumpScheduleQueuedToRunMaxUs",
							queuedToRunUs,
						);
					}
					socket._bridgeReadFirstPumpBenchmarkScheduled = false;
					if (socket.destroyed) {
						if (isNetBridgeMetricsEnabled()) {
							countNetBridgeMetric("readFirstPumpScheduleSkipDestroyed");
						}
						return;
					}
					if (socket._tlsUpgrading) {
						if (isNetBridgeMetricsEnabled()) {
							countNetBridgeMetric("readFirstPumpScheduleSkipTlsUpgrading");
						}
						return;
					}
					if (socket._bridgeReadPumpStarted) {
						if (isNetBridgeMetricsEnabled()) {
							countNetBridgeMetric("readFirstPumpScheduleSkipPumpStarted");
						}
						return;
					}
					if (socket._bridgeReadLoopRunning) {
						if (isNetBridgeMetricsEnabled()) {
							countNetBridgeMetric("readFirstPumpScheduleSkipLoopRunning");
						}
						return;
					}
					if (socket._socketId === 0) {
						if (isNetBridgeMetricsEnabled()) {
							countNetBridgeMetric("readFirstPumpScheduleSkipSocketClosed");
						}
						return;
					}
					if (isNetBridgeMetricsEnabled()) {
						countNetBridgeMetric("readFirstPumpSchedulePumpCalls");
					}
					socket._nextReadPumpOrigin = "eventWake";
					socket._readFirstPumpScheduleActive = true;
					socket._readFirstPumpScheduleQueuedAtUs = queuedAtUs;
					void socket._pumpBridgeReads();
				});
			}
		}
		if (
			!socket._bridgeReadLoopRunning &&
			socket._connected &&
			socket._applicationReadDemand
		) {
			queueSocketBridgeReadPump(socket, "eventWake");
		}
		return;
	}
}

function wakePeerBridgeReads(socket) {
	countNetBridgeMetric("peerWakeScans");
	if (!socket || socket._socketId === 0) {
		countNetBridgeMetric("peerWakeInvalidTargets");
		return;
	}
	if (
		typeof socket.remotePort === "number" &&
		typeof socket.localPort === "number"
	) {
		for (const peer of registeredNetSockets.values()) {
			if (peer === socket || peer.destroyed) {
				continue;
			}
			if (
				peer.localPort === socket.remotePort &&
				peer.remotePort === socket.localPort
			) {
				countNetBridgeMetric("peerWakeFound");
				wakeSocketBridgeReads(peer);
				return;
			}
		}
		countNetBridgeMetric("peerWakeMiss");
		return;
	}
	const localPath =
		typeof socket._localUnixPath === "string" ? socket._localUnixPath : void 0;
	const remotePath =
		typeof socket._remoteUnixPath === "string"
			? socket._remoteUnixPath
			: void 0;
	if (localPath === void 0 && remotePath === void 0) {
		countNetBridgeMetric("peerWakeInvalidTargets");
		return;
	}
	let foundUnixPeer = false;
	for (const peer of registeredNetSockets.values()) {
		if (peer === socket || peer.destroyed) {
			continue;
		}
		const peerLocalPath =
			typeof peer._localUnixPath === "string" ? peer._localUnixPath : void 0;
		const peerRemotePath =
			typeof peer._remoteUnixPath === "string" ? peer._remoteUnixPath : void 0;
		const fullPathMirror =
			localPath !== void 0 &&
			remotePath !== void 0 &&
			peerLocalPath !== void 0 &&
			peerRemotePath !== void 0 &&
			peerLocalPath === remotePath &&
			peerRemotePath === localPath;
		const remoteToPeerLocal =
			remotePath !== void 0 &&
			peerLocalPath !== void 0 &&
			peerLocalPath === remotePath;
		const localToPeerRemote =
			localPath !== void 0 &&
			peerRemotePath !== void 0 &&
			peerRemotePath === localPath;
		if (fullPathMirror || remoteToPeerLocal || localToPeerRemote) {
			foundUnixPeer = true;
			countNetBridgeMetric("peerWakeUnixFound");
			wakeSocketBridgeReads(peer);
		}
	}
	if (!foundUnixPeer) {
		countNetBridgeMetric("peerWakeUnixMiss");
	}
}

function wakeNetServerAccept(server) {
	countNetBridgeMetric("acceptWakeAttempts");
	if (!server?.listening || server._serverId === 0) {
		countNetBridgeMetric("acceptWakeInvalidTargets");
		return;
	}
	server._pendingAcceptWake = true;
	if (server._acceptLoopRunning) {
		countNetBridgeMetric("acceptWakeAlreadyRunning");
		return;
	}
	if (server._acceptPumpQueued) {
		countNetBridgeMetric("acceptWakeAlreadyQueued");
		return;
	}
	server._acceptPumpQueued = true;
	countNetBridgeMetric("acceptEventWakeups");
	queueMicrotask(() => {
		if (!server._acceptPumpQueued) {
			return;
		}
		server._acceptPumpQueued = false;
		if (server.listening && server._serverId !== 0) {
			server._pendingAcceptWake = false;
			server._nextAcceptPumpOrigin = "eventWake";
			return server._pumpAccepts();
		}
	});
}

function wakeNetServerAcceptForSocket(socket) {
	countNetBridgeMetric("acceptWakeSocketScans");
	const port = socket?.remotePort;
	if (typeof port === "number") {
		const server = registeredNetServersByPort.get(port);
		if (server) {
			countNetBridgeMetric("acceptWakeSocketFound");
		} else {
			countNetBridgeMetric("acceptWakeSocketMiss");
		}
		return wakeNetServerAccept(server);
	}
	const path = socket?._remoteUnixPath;
	if (typeof path !== "string") {
		countNetBridgeMetric("acceptWakeSocketInvalidTargets");
		return;
	}
	const server = registeredNetServersByPath.get(path);
	if (server) {
		countNetBridgeMetric("acceptWakeSocketFound");
		countNetBridgeMetric("acceptWakeSocketUnixFound");
	} else {
		countNetBridgeMetric("acceptWakeSocketMiss");
		countNetBridgeMetric("acceptWakeSocketUnixMiss");
	}
	return wakeNetServerAccept(server);
}

function isTruthySocketOption(value) {
	return value === void 0 ? true : Boolean(value);
}

function normalizeKeepAliveDelay(initialDelay) {
	if (typeof initialDelay !== "number" || !Number.isFinite(initialDelay)) {
		return 0;
	}
	return Math.max(0, Math.floor(initialDelay / 1e3));
}

function createTimeoutArgTypeError(argumentName, value) {
	return createTypeErrorWithCode(
		`The "${argumentName}" argument must be of type number. Received ${formatReceivedType(value)}`,
		"ERR_INVALID_ARG_TYPE",
	);
}

function createFunctionArgTypeError(argumentName, value) {
	return createTypeErrorWithCode(
		`The "${argumentName}" argument must be of type function. Received ${formatReceivedType(value)}`,
		"ERR_INVALID_ARG_TYPE",
	);
}

function createTimeoutRangeError(value) {
	const error = new RangeError(
		`The value of "timeout" is out of range. It must be a non-negative finite number. Received ${String(value)}`,
	);
	error.code = "ERR_OUT_OF_RANGE";
	return error;
}

function createListenArgValueError(message) {
	return createTypeErrorWithCode(message, "ERR_INVALID_ARG_VALUE");
}

function createSocketBadPortError(value) {
	const error = new RangeError(
		`options.port should be >= 0 and < 65536. Received ${formatReceivedType(value)}.`,
	);
	error.code = "ERR_SOCKET_BAD_PORT";
	return error;
}

function isValidTcpPort(value) {
	return Number.isInteger(value) && value >= 0 && value < 65536;
}

function isDecimalIntegerString(value) {
	return /^[0-9]+$/.test(value);
}

function normalizeListenPortValue(value) {
	if (value === void 0 || value === null) {
		return 0;
	}
	if (typeof value === "string" && value.length > 0) {
		const parsed = Number(value);
		if (isValidTcpPort(parsed)) {
			return parsed;
		}
		throw createSocketBadPortError(value);
	}
	if (typeof value === "number") {
		if (isValidTcpPort(value)) {
			return value;
		}
		throw createSocketBadPortError(value);
	}
	throw createListenArgValueError(
		`The argument 'options' is invalid. Received ${String(value)}`,
	);
}

function normalizeListenArgs(
	portOrOptions,
	hostOrCallback,
	backlogOrCallback,
	callback,
) {
	const defaultOptions = {
		port: 0,
		host: "127.0.0.1",
		backlog: 511,
		readableAll: false,
		writableAll: false,
	};
	if (typeof portOrOptions === "function") {
		return {
			...defaultOptions,
			callback: portOrOptions,
		};
	}
	if (portOrOptions !== null && typeof portOrOptions === "object") {
		const options = portOrOptions;
		const hasPort = Object.hasOwn(options, "port");
		const hasPath = Object.hasOwn(options, "path");
		if (!hasPort && !hasPath) {
			throw createListenArgValueError(
				`The argument 'options' must have the property "port" or "path". Received ${String(portOrOptions)}`,
			);
		}
		if (hasPort && hasPath) {
			throw createListenArgValueError(
				`The argument 'options' is invalid. Received ${String(portOrOptions)}`,
			);
		}
		if (
			hasPort &&
			options.port !== void 0 &&
			options.port !== null &&
			typeof options.port !== "number" &&
			typeof options.port !== "string"
		) {
			throw createListenArgValueError(
				`The argument 'options' is invalid. Received ${String(portOrOptions)}`,
			);
		}
		if (hasPath) {
			if (typeof options.path !== "string" || options.path.length === 0) {
				throw createListenArgValueError(
					`The argument 'options' is invalid. Received ${String(portOrOptions)}`,
				);
			}
			return {
				path: options.path,
				backlog:
					typeof options.backlog === "number" &&
					Number.isFinite(options.backlog)
						? options.backlog
						: defaultOptions.backlog,
				readableAll: options.readableAll === true,
				writableAll: options.writableAll === true,
				callback:
					typeof hostOrCallback === "function"
						? hostOrCallback
						: typeof backlogOrCallback === "function"
							? backlogOrCallback
							: callback,
			};
		}
		return {
			port: normalizeListenPortValue(options.port),
			host:
				typeof options.host === "string" && options.host.length > 0
					? options.host
					: defaultOptions.host,
			backlog:
				typeof options.backlog === "number" && Number.isFinite(options.backlog)
					? options.backlog
					: defaultOptions.backlog,
			readableAll: false,
			writableAll: false,
			callback:
				typeof hostOrCallback === "function"
					? hostOrCallback
					: typeof backlogOrCallback === "function"
						? backlogOrCallback
						: callback,
		};
	}
	if (
		portOrOptions !== void 0 &&
		portOrOptions !== null &&
		typeof portOrOptions !== "number" &&
		typeof portOrOptions !== "string"
	) {
		throw createListenArgValueError(
			`The argument 'options' is invalid. Received ${String(portOrOptions)}`,
		);
	}
	if (
		typeof portOrOptions === "string" &&
		portOrOptions.length > 0 &&
		!isDecimalIntegerString(portOrOptions)
	) {
		return {
			path: portOrOptions,
			backlog: defaultOptions.backlog,
			readableAll: false,
			writableAll: false,
			callback:
				typeof hostOrCallback === "function"
					? hostOrCallback
					: typeof backlogOrCallback === "function"
						? backlogOrCallback
						: callback,
		};
	}
	return {
		port: normalizeListenPortValue(portOrOptions),
		host:
			typeof hostOrCallback === "string" ? hostOrCallback : defaultOptions.host,
		backlog:
			typeof backlogOrCallback === "number"
				? backlogOrCallback
				: defaultOptions.backlog,
		readableAll: false,
		writableAll: false,
		callback:
			typeof hostOrCallback === "function"
				? hostOrCallback
				: typeof backlogOrCallback === "function"
					? backlogOrCallback
					: callback,
	};
}

function normalizeConnectArgs(portOrOptions, hostOrCallback, callback) {
	if (portOrOptions !== null && typeof portOrOptions === "object") {
		const normalizedPort =
			typeof portOrOptions.port === "string"
				? Number(portOrOptions.port)
				: portOrOptions.port;
		return {
			host:
				typeof portOrOptions.host === "string" && portOrOptions.host.length > 0
					? portOrOptions.host
					: void 0,
			port: normalizedPort,
			path:
				typeof portOrOptions.path === "string" && portOrOptions.path.length > 0
					? portOrOptions.path
					: void 0,
			localAddress:
				typeof portOrOptions.localAddress === "string" &&
				portOrOptions.localAddress.length > 0
					? portOrOptions.localAddress
					: void 0,
			localPort:
				typeof portOrOptions.localPort === "number"
					? portOrOptions.localPort
					: void 0,
			keepAlive: portOrOptions.keepAlive,
			keepAliveInitialDelay: portOrOptions.keepAliveInitialDelay,
			callback:
				typeof hostOrCallback === "function" ? hostOrCallback : callback,
		};
	}
	if (
		typeof portOrOptions === "string" &&
		!isDecimalIntegerString(portOrOptions)
	) {
		return {
			path: portOrOptions,
			callback:
				typeof hostOrCallback === "function" ? hostOrCallback : callback,
		};
	}
	return {
		port:
			typeof portOrOptions === "number" ? portOrOptions : Number(portOrOptions),
		host: typeof hostOrCallback === "string" ? hostOrCallback : "127.0.0.1",
		callback: typeof hostOrCallback === "function" ? hostOrCallback : callback,
	};
}

function unixSocketRequest(path) {
	if (path.charCodeAt(0) === 0) {
		return {
			abstractPathHex: Buffer.from(path.slice(1), "utf8").toString("hex"),
		};
	}
	return { path };
}

function isValidIPv4Segment(segment) {
	if (!/^[0-9]{1,3}$/.test(segment)) {
		return false;
	}
	if (segment.length > 1 && segment.startsWith("0")) {
		return false;
	}
	const value = Number(segment);
	return Number.isInteger(value) && value >= 0 && value <= 255;
}

function isIPv4String(input) {
	const segments = input.split(".");
	return (
		segments.length === 4 &&
		segments.every((segment) => isValidIPv4Segment(segment))
	);
}

function isValidIPv6Zone(zone) {
	return zone.length > 0 && /^[0-9A-Za-z_.-]+$/.test(zone);
}

function countIPv6Parts(part) {
	if (part.length === 0) {
		return 0;
	}
	const segments = part.split(":");
	let count = 0;
	for (const segment of segments) {
		if (segment.length === 0) {
			return null;
		}
		if (segment.includes(".")) {
			if (segment !== segments[segments.length - 1] || !isIPv4String(segment)) {
				return null;
			}
			count += 2;
			continue;
		}
		if (!/^[0-9A-Fa-f]{1,4}$/.test(segment)) {
			return null;
		}
		count += 1;
	}
	return count;
}

function isIPv6String(input) {
	if (input.length === 0) {
		return false;
	}
	let address = input;
	const zoneIndex = address.indexOf("%");
	if (zoneIndex !== -1) {
		if (address.indexOf("%", zoneIndex + 1) !== -1) {
			return false;
		}
		const zone = address.slice(zoneIndex + 1);
		if (!isValidIPv6Zone(zone)) {
			return false;
		}
		address = address.slice(0, zoneIndex);
	}
	const doubleColonIndex = address.indexOf("::");
	if (doubleColonIndex !== -1) {
		if (address.indexOf("::", doubleColonIndex + 2) !== -1) {
			return false;
		}
		const [left, right] = address.split("::");
		if (left.includes(".")) {
			return false;
		}
		const leftCount = countIPv6Parts(left);
		const rightCount = countIPv6Parts(right);
		if (leftCount === null || rightCount === null) {
			return false;
		}
		return leftCount + rightCount < 8;
	}
	const count = countIPv6Parts(address);
	return count === 8;
}

function coerceIpInput(input) {
	if (input === null || input === void 0) {
		return "";
	}
	return String(input);
}

function classifyIpAddress(input) {
	const value = coerceIpInput(input);
	if (isIPv4String(value)) {
		return 4;
	}
	if (isIPv6String(value)) {
		return 6;
	}
	return 0;
}

function normalizeIpFamilyLabel(address, family) {
	if (family === "ipv4" || family === 4) {
		return "ipv4";
	}
	if (family === "ipv6" || family === 6) {
		return "ipv6";
	}
	const detected = classifyIpAddress(address);
	if (detected === 4) {
		return "ipv4";
	}
	if (detected === 6) {
		return "ipv6";
	}
	throw new TypeError(`Invalid IP address: ${address}`);
}

function ipv4ToBigInt(address) {
	return address
		.split(".")
		.reduce((value, segment) => (value << 8n) + BigInt(Number(segment)), 0n);
}

function expandIpv6Address(address) {
	let normalized = String(address);
	const zoneIndex = normalized.indexOf("%");
	if (zoneIndex !== -1) {
		normalized = normalized.slice(0, zoneIndex);
	}
	if (normalized.includes(".")) {
		const lastColonIndex = normalized.lastIndexOf(":");
		const ipv4Part = normalized.slice(lastColonIndex + 1);
		const ipv4Value = ipv4ToBigInt(ipv4Part);
		const high = Number((ipv4Value >> 16n) & 65535n).toString(16);
		const low = Number(ipv4Value & 65535n).toString(16);
		normalized = `${normalized.slice(0, lastColonIndex)}:${high}:${low}`;
	}
	const hasDoubleColon = normalized.includes("::");
	const [leftRaw, rightRaw] = hasDoubleColon
		? normalized.split("::")
		: [normalized, ""];
	const left = leftRaw.length > 0 ? leftRaw.split(":") : [];
	const right = rightRaw.length > 0 ? rightRaw.split(":") : [];
	const fill = hasDoubleColon
		? Math.max(0, 8 - (left.length + right.length))
		: 0;
	const parts = [...left, ...new Array(fill).fill("0"), ...right];
	if (parts.length !== 8) {
		throw new TypeError(`Invalid IPv6 address: ${address}`);
	}
	return parts.map((part) => (part.length === 0 ? "0" : part));
}

function ipv6ToBigInt(address) {
	return expandIpv6Address(address).reduce(
		(value, part) => (value << 16n) + BigInt(parseInt(part, 16)),
		0n,
	);
}

function ipAddressToBigInt(address, family) {
	return family === "ipv4" ? ipv4ToBigInt(address) : ipv6ToBigInt(address);
}

function formatBlockListRule(rule) {
	if (rule.type === "address") {
		return `Address: ${rule.family === "ipv4" ? "IPv4" : "IPv6"} ${rule.address}`;
	}
	if (rule.type === "range") {
		return `Range: ${rule.family === "ipv4" ? "IPv4" : "IPv6"} ${rule.start}-${rule.end}`;
	}
	return `Subnet: ${rule.family === "ipv4" ? "IPv4" : "IPv6"} ${rule.network}/${rule.prefix}`;
}

var BlockList = class {
	_rules = [];
	addAddress(address, family) {
		const normalizedFamily = normalizeIpFamilyLabel(address, family);
		this._rules.push({
			type: "address",
			family: normalizedFamily,
			address: String(address),
		});
		return this;
	}
	addRange(start, end, family) {
		const normalizedFamily = normalizeIpFamilyLabel(start, family);
		if (normalizeIpFamilyLabel(end, normalizedFamily) !== normalizedFamily) {
			throw new TypeError("BlockList range family mismatch");
		}
		this._rules.push({
			type: "range",
			family: normalizedFamily,
			start: String(start),
			end: String(end),
		});
		return this;
	}
	addSubnet(network, prefix, family) {
		const normalizedFamily = normalizeIpFamilyLabel(network, family);
		const numericPrefix = Number(prefix);
		const maxPrefix = normalizedFamily === "ipv4" ? 32 : 128;
		if (
			!Number.isInteger(numericPrefix) ||
			numericPrefix < 0 ||
			numericPrefix > maxPrefix
		) {
			throw new RangeError(`Invalid subnet prefix: ${prefix}`);
		}
		this._rules.push({
			type: "subnet",
			family: normalizedFamily,
			network: String(network),
			prefix: numericPrefix,
		});
		return this;
	}
	check(address, family) {
		const normalizedFamily = normalizeIpFamilyLabel(address, family);
		const value = ipAddressToBigInt(String(address), normalizedFamily);
		for (const rule of this._rules) {
			if (rule.family !== normalizedFamily) {
				continue;
			}
			if (
				rule.type === "address" &&
				value === ipAddressToBigInt(rule.address, normalizedFamily)
			) {
				return true;
			}
			if (rule.type === "range") {
				const start = ipAddressToBigInt(rule.start, normalizedFamily);
				const end = ipAddressToBigInt(rule.end, normalizedFamily);
				if (value >= start && value <= end) {
					return true;
				}
			}
			if (rule.type === "subnet") {
				const bits = normalizedFamily === "ipv4" ? 32n : 128n;
				const prefixBits = BigInt(rule.prefix);
				const shift = bits - prefixBits;
				const mask =
					prefixBits === 0n ? 0n : ((1n << bits) - 1n) ^ ((1n << shift) - 1n);
				const network = ipAddressToBigInt(rule.network, normalizedFamily);
				if ((value & mask) === (network & mask)) {
					return true;
				}
			}
		}
		return false;
	}
	toJSON() {
		return this._rules.map((rule) => ({ ...rule }));
	}
	fromJSON(value) {
		if (!Array.isArray(value)) {
			throw new TypeError("BlockList JSON must be an array");
		}
		this._rules = value.map((rule) => ({ ...rule }));
		return this;
	}
	get rules() {
		return this._rules.map((rule) => formatBlockListRule(rule));
	}
};

var defaultAutoSelectFamily = true;

var defaultAutoSelectFamilyAttemptTimeout = 250;

var SocketAddress = class _SocketAddress {
	constructor(options = {}) {
		const address = String(options.address ?? "");
		const family = normalizeIpFamilyLabel(address, options.family);
		const port = Number(options.port ?? 0);
		const flowlabel = Number(options.flowlabel ?? 0);
		if (!Number.isInteger(port) || port < 0 || port > 65535) {
			throw new RangeError(`Invalid port: ${options.port}`);
		}
		if (!Number.isInteger(flowlabel) || flowlabel < 0) {
			throw new RangeError(`Invalid flowlabel: ${options.flowlabel}`);
		}
		this.address = address;
		this.port = port;
		this.family = family;
		this.flowlabel = flowlabel;
	}
	toJSON() {
		return {
			address: this.address,
			port: this.port,
			family: this.family,
			flowlabel: this.flowlabel,
		};
	}
	static isSocketAddress(value) {
		return value instanceof _SocketAddress;
	}
	static parse(value) {
		const input = String(value);
		if (input.startsWith("[")) {
			const closingIndex = input.indexOf("]");
			if (closingIndex === -1) {
				return void 0;
			}
			const address = input.slice(1, closingIndex);
			const port =
				input[closingIndex + 1] === ":"
					? Number(input.slice(closingIndex + 2))
					: 0;
			return new _SocketAddress({ address, family: "ipv6", port });
		}
		const lastColonIndex = input.lastIndexOf(":");
		if (lastColonIndex !== -1 && input.indexOf(":") === lastColonIndex) {
			const address = input.slice(0, lastColonIndex);
			const port = Number(input.slice(lastColonIndex + 1));
			if (classifyIpAddress(address) !== 0 && Number.isInteger(port)) {
				return new _SocketAddress({ address, port });
			}
		}
		if (classifyIpAddress(input) !== 0) {
			return new _SocketAddress({ address: input });
		}
		return void 0;
	}
};

function normalizeSocketTimeout(timeout) {
	if (typeof timeout !== "number") {
		throw createTimeoutArgTypeError("timeout", timeout);
	}
	if (!Number.isFinite(timeout) || timeout < 0) {
		throw createTimeoutRangeError(timeout);
	}
	return timeout;
}

function parseNetSocketInfo(data) {
	if (!data) {
		return null;
	}
	try {
		const parsed = JSON.parse(data);
		return parsed && typeof parsed === "object" ? parsed : null;
	} catch {
		return null;
	}
}

function normalizeNetSocketHandle(handle) {
	if (!handle) {
		throw new Error("net.connect bridge returned an empty socket handle");
	}
	if (typeof handle === "string") {
		return {
			socketId: handle,
		};
	}
	if (
		typeof handle === "object" &&
		(typeof handle.socketId === "string" || typeof handle.socketId === "number")
	) {
		return handle;
	}
	if (typeof handle === "object" && handle.loopbackHttpTarget) {
		return handle;
	}
	throw new Error("net.connect bridge returned an invalid socket handle");
}

function serializeTlsValue(value) {
	if (value === void 0 || value === null) {
		return void 0;
	}
	if (Array.isArray(value)) {
		const entries = value
			.map((entry) => serializeTlsValue(entry))
			.flatMap((entry) =>
				Array.isArray(entry) ? entry : entry ? [entry] : [],
			);
		return entries.length > 0 ? entries : void 0;
	}
	if (typeof value === "string") {
		return { kind: "string", data: value };
	}
	if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
		return { kind: "buffer", data: Buffer.from(value).toString("base64") };
	}
	return void 0;
}

function isTlsSecureContextWrapper(value) {
	return (
		!!value && typeof value === "object" && "__agentOsTlsContext" in value
	);
}

function buildSerializedTlsOptions(options, extra) {
	const contextOptions = isTlsSecureContextWrapper(options?.secureContext)
		? options.secureContext.__agentOsTlsContext
		: void 0;
	const serialized = {
		...(contextOptions ?? {}),
		...extra,
	};
	const key = serializeTlsValue(options?.key);
	const cert = serializeTlsValue(options?.cert);
	const ca = serializeTlsValue(options?.ca);
	if (key !== void 0) serialized.key = key;
	if (cert !== void 0) serialized.cert = cert;
	if (ca !== void 0) serialized.ca = ca;
	if (typeof options?.passphrase === "string")
		serialized.passphrase = options.passphrase;
	if (typeof options?.ciphers === "string")
		serialized.ciphers = options.ciphers;
	if (typeof options?.host === "string" && options.host.length > 0)
		serialized.host = options.host;
	if (
		Buffer.isBuffer(options?.session) ||
		options?.session instanceof Uint8Array
	) {
		serialized.session = Buffer.from(options.session).toString("base64");
	}
	if (Array.isArray(options?.ALPNProtocols)) {
		const protocols = options.ALPNProtocols.filter(
			(value) => typeof value === "string",
		);
		if (protocols.length > 0) {
			serialized.ALPNProtocols = protocols;
		}
	}
	if (typeof options?.minVersion === "string")
		serialized.minVersion = options.minVersion;
	if (typeof options?.maxVersion === "string")
		serialized.maxVersion = options.maxVersion;
	if (typeof options?.servername === "string")
		serialized.servername = options.servername;
	if (typeof options?.rejectUnauthorized === "boolean") {
		serialized.rejectUnauthorized = options.rejectUnauthorized;
	}
	if (typeof options?.requestCert === "boolean") {
		serialized.requestCert = options.requestCert;
	}
	return serialized;
}

function parseTlsState(payload) {
	if (!payload) {
		return null;
	}
	try {
		return JSON.parse(payload);
	} catch {
		return null;
	}
}

function parseTlsClientHello(payload) {
	if (!payload) {
		return null;
	}
	try {
		return JSON.parse(payload);
	} catch {
		return null;
	}
}

function createBridgedTlsError(payload) {
	if (!payload) {
		return new Error("socket error");
	}
	try {
		const parsed = JSON.parse(payload);
		const error = new Error(parsed.message);
		if (parsed.name) error.name = parsed.name;
		if (parsed.code) {
			error.code = parsed.code;
		}
		if (parsed.stack) error.stack = parsed.stack;
		return error;
	} catch {
		return new Error(payload);
	}
}

function deserializeTlsBridgeValue(value, refs = /* @__PURE__ */ new Map()) {
	if (
		value === null ||
		typeof value === "boolean" ||
		typeof value === "number" ||
		typeof value === "string"
	) {
		return value;
	}
	if (value.type === "undefined") {
		return void 0;
	}
	if (value.type === "buffer") {
		return Buffer.from(value.data, "base64");
	}
	if (value.type === "array") {
		return value.value.map((entry) => deserializeTlsBridgeValue(entry, refs));
	}
	if (value.type === "ref") {
		return refs.get(value.id);
	}
	const target = {};
	refs.set(value.id, target);
	for (const [key, entry] of Object.entries(value.value)) {
		target[key] = deserializeTlsBridgeValue(entry, refs);
	}
	return target;
}

function queryTlsSocket(socketId, query, detailed) {
	if (typeof _netSocketTlsQueryRaw === "undefined") {
		return void 0;
	}
	const payload = _netSocketTlsQueryRaw.applySync(
		void 0,
		detailed === void 0 ? [socketId, query] : [socketId, query, detailed],
	);
	return deserializeTlsBridgeValue(JSON.parse(payload));
}

function finalizeTlsUpgrade(
	socket,
	eventName = "secureConnect",
	startReadPump = true,
) {
	socket._tlsUpgrading = false;
	socket.encrypted = true;
	socket.authorized = socket.authorizationError == null;
	if (typeof socket._socketId === "string" && socket._socketId.length > 0) {
		const protocol = queryTlsSocket(socket._socketId, "getProtocol");
		if (typeof protocol === "string" || protocol === null) {
			socket._tlsProtocol = protocol;
		}
		const cipher = queryTlsSocket(socket._socketId, "getCipher");
		if (cipher !== void 0) {
			socket._tlsCipher = cipher;
		}
		const reused = queryTlsSocket(socket._socketId, "isSessionReused");
		if (typeof reused === "boolean") {
			socket._tlsSessionReused = reused;
		}
	}
	socket._touchTimeout();
	socket._emitNet(eventName);
	if (eventName !== "secure") {
		socket._emitNet("secure");
	}
	if (startReadPump && !socket.destroyed && !socket._bridgeReadLoopRunning) {
		socket._nextReadPumpOrigin = "tls";
		void socket._pumpBridgeReads();
	}
}

function createConnectedSocketHandle(socketId) {
	return {
		socketId,
		setNoDelay(enable) {
			_netSocketSetNoDelayRaw?.applySync(void 0, [socketId, enable !== false]);
			return this;
		},
		setKeepAlive(enable, initialDelay) {
			_netSocketSetKeepAliveRaw?.applySync(void 0, [
				socketId,
				enable !== false,
				normalizeKeepAliveDelay(initialDelay),
			]);
			return this;
		},
		ref() {
			return this;
		},
		unref() {
			return this;
		},
	};
}

function createAcceptedClientHandle(socketId, info) {
	return {
		socketId,
		info,
		setNoDelay(enable) {
			_netSocketSetNoDelayRaw?.applySync(void 0, [socketId, enable !== false]);
			return this;
		},
		setKeepAlive(enable, initialDelay) {
			_netSocketSetKeepAliveRaw?.applySync(void 0, [
				socketId,
				enable !== false,
				normalizeKeepAliveDelay(initialDelay),
			]);
			return this;
		},
		ref() {
			return this;
		},
		unref() {
			return this;
		},
	};
}

// Must match JAVASCRIPT_NET_TIMEOUT_SENTINEL in crates/native-sidecar/src/execution/mod.rs.
// A mismatched sentinel is NOT a soft failure: every no-data poll response then
// falls through to base64 decoding and injects the decoded sentinel bytes into
// the socket stream as phantom data.
var NET_BRIDGE_TIMEOUT_SENTINEL = "__agentos_net_timeout__";

function isNetBridgeTraceEnabled() {
	const env =
		typeof process !== "undefined"
			? process.env
			: globalThis.__agentOSProcessConfigEnv;
	return env?.AGENTOS_NET_BRIDGE_TRACE === "1";
}

function isNetRetainOwnedWriteBufferEnabled() {
	const processEnv = typeof process !== "undefined" ? process.env : undefined;
	const configEnv = globalThis.__agentOSProcessConfigEnv;
	return (
		processEnv?.AGENTOS_NET_RETAIN_OWNED_WRITE_BUFFER !== "0" &&
		configEnv?.AGENTOS_NET_RETAIN_OWNED_WRITE_BUFFER !== "0"
	);
}

function createNetBridgeMetrics() {
	return {
		userWriteCalls: 0,
		userWriteBytes: 0,
		queuedWriteChunks: 0,
		queuedWriteBytes: 0,
		queuedWriteCopiedChunks: 0,
		queuedWriteCopiedBytes: 0,
		queuedWriteRetainedChunks: 0,
		queuedWriteRetainedBytes: 0,
		flushCalls: 0,
		flushChunks: 0,
		flushBytes: 0,
		writeBufferedBytesMax: 0,
		writeBufferedChunksMax: 0,
		writeBase64EncodeCalls: 0,
		writeBase64EncodeBytes: 0,
		writeBase64EncodeUs: 0,
		writeRawCalls: 0,
		writeRawBytes: 0,
		writeRawElapsedUs: 0,
		readRawCalls: 0,
		readRawElapsedUs: 0,
		readPumpRuns: 0,
		readPumpSkippedNoDemand: 0,
		readPumpSkippedLoopRunning: 0,
		readPumpSkippedRpcInFlight: 0,
		readPumpSkippedRemoteEnded: 0,
		readPumpSkippedReleased: 0,
		readPumpSkippedCloseEmitted: 0,
		readPumpSkippedRawMissing: 0,
		readPumpSkippedUnregistered: 0,
		readTimeoutSentinels: 0,
		readPollTimersScheduled: 0,
		readPollTimerFires: 0,
		readPollTimerFireLagUs: 0,
		readPollTimerFireLagMaxUs: 0,
		readDataEvents: 0,
		readBytes: 0,
		readBase64DecodeCalls: 0,
		readBase64DecodeBytes: 0,
		readBase64DecodeChars: 0,
		readBase64DecodeUs: 0,
		readPayloadMaterializeCalls: 0,
		readPayloadMaterializeBytes: 0,
		readPayloadMaterializeUs: 0,
		readEndEvents: 0,
		readMacrotaskYields: 0,
		readMacrotaskYieldElapsedUs: 0,
		readMacrotaskYieldMaxUs: 0,
		queueReadablePayloads: 0,
		queueReadablePayloadElapsedUs: 0,
		queueReadablePayloadMaxUs: 0,
		queueReadableBytes: 0,
		queueReadableBytesMax: 0,
		queueReadableImmediateReadCalls: 0,
		queueReadableImmediateReadUs: 0,
		queueReadableImmediateReadMaxUs: 0,
		socketReadableEmits: 0,
		socketReadableEmitUs: 0,
		socketReadableEmitMaxUs: 0,
		socketDataEmits: 0,
		socketDataEmitUs: 0,
		socketDataEmitMaxUs: 0,
		readPostDeliveryProbeCalls: 0,
		readPostDeliveryProbeTimeoutSentinels: 0,
		readPostDeliveryProbeDataEvents: 0,
		readPostDeliveryNextRawCalls: 0,
		readPostDeliveryNextRawTimeoutSentinels: 0,
		readPostDeliveryNextRawDataEvents: 0,
		readPostDeliveryToProbeStartUs: 0,
		readPostDeliveryProbeElapsedUs: 0,
		readPostDeliveryProbeMaxUs: 0,
		readPostDeliveryPendingWriteFlushes: 0,
		readPostDeliveryPendingWriteBytes: 0,
		userWriteDuringDataEmitCalls: 0,
		dataEmitStartToUserWriteUs: 0,
		dataEmitEndToUserWriteUs: 0,
		writeQueuedToFlushStartUs: 0,
		writeQueuedToFlushStartMaxUs: 0,
		writeFlushQueuedToRawUs: 0,
		writeFlushQueuedToRawMaxUs: 0,
		readWakeQueuedToPumpStartUs: 0,
		readWakeQueuedToPumpStartMaxUs: 0,
		acceptWakeQueuedToPumpStartUs: 0,
		acceptWakeQueuedToPumpStartMaxUs: 0,
		socketEndEmits: 0,
		socketCloseEmits: 0,
		socketConnectEmits: 0,
		serverCloseCalls: 0,
		serverCloseConnectionsAtCall: 0,
		serverCloseConnectionsAtCallMax: 0,
		serverCloseCallsWithConnections: 0,
		serverCloseConnectionDrainEvents: 0,
		serverCloseEmits: 0,
		acceptRawCalls: 0,
		acceptRawElapsedUs: 0,
		acceptPumpRuns: 0,
		acceptLoopAlreadyRunning: 0,
		acceptTimeoutSentinels: 0,
		acceptPollTimersScheduled: 0,
		acceptPollTimerFires: 0,
		acceptPollTimerFireLagUs: 0,
		acceptPollTimerFireLagMaxUs: 0,
		acceptConnections: 0,
		acceptJsonParseUs: 0,
		acceptOnConnectionUs: 0,
		connectionEmits: 0,
		readEventWakeups: 0,
		readWakeAttempts: 0,
		readWakeInvalidTargets: 0,
		readWakeAlreadyRunning: 0,
		readWakeNoTimer: 0,
		readWakeNoTimerBeforeFirstPump: 0,
		readWakeNoTimerAfterFirstPump: 0,
		readWakeNoTimerConnected: 0,
		readWakeNoTimerConnecting: 0,
		readWakeNoTimerRefed: 0,
		readWakeNoTimerUnrefed: 0,
		readWakeNoTimerHasDataListener: 0,
		readWakeNoTimerHasReadableListener: 0,
		readWakeNoTimerPendingWriteFlush: 0,
		readWakeNoTimerPendingWriteBytes: 0,
		readFirstPumpAfterNoTimerWakeCalls: 0,
		readFirstPumpAfterNoTimerWakeUs: 0,
		readFirstPumpAfterNoTimerWakeMaxUs: 0,
		readFirstPumpOriginConnectWait: 0,
		readFirstPumpOriginAcceptedHandle: 0,
		readFirstPumpOriginEventWake: 0,
		readFirstPumpOriginTimer: 0,
		readFirstPumpOriginRef: 0,
		readFirstPumpOriginTls: 0,
		readFirstPumpOriginUnknown: 0,
		readFirstPumpResultData: 0,
		readFirstPumpResultEnd: 0,
		readFirstPumpResultTimeout: 0,
		readFirstPumpScheduleCandidates: 0,
		readFirstPumpScheduleQueued: 0,
		readFirstPumpScheduleAlreadyScheduled: 0,
		readFirstPumpScheduleRuns: 0,
		readFirstPumpSchedulePumpCalls: 0,
		readFirstPumpScheduleSkipDestroyed: 0,
		readFirstPumpScheduleSkipTlsUpgrading: 0,
		readFirstPumpScheduleSkipPumpStarted: 0,
		readFirstPumpScheduleSkipLoopRunning: 0,
		readFirstPumpScheduleSkipSocketClosed: 0,
		readFirstPumpScheduleQueuedToRunUs: 0,
		readFirstPumpScheduleQueuedToRunMaxUs: 0,
		readFirstPumpScheduleQueuedToPumpStartUs: 0,
		readFirstPumpScheduleQueuedToPumpStartMaxUs: 0,
		readFirstPumpScheduleResultData: 0,
		readFirstPumpScheduleResultTimeout: 0,
		readFirstPumpScheduleResultEnd: 0,
		peerWakeScans: 0,
		peerWakeInvalidTargets: 0,
		peerWakeFound: 0,
		peerWakeMiss: 0,
		peerWakeUnixFound: 0,
		peerWakeUnixMiss: 0,
		peerWakeOnShutdown: 0,
		peerWakeOnDestroy: 0,
		acceptEventWakeups: 0,
		acceptWakeAttempts: 0,
		acceptWakeInvalidTargets: 0,
		acceptWakeNoTimer: 0,
		acceptWakeNoTimerBeforeFirstPump: 0,
		acceptWakeNoTimerAfterFirstPump: 0,
		acceptWakeNoTimerLoopRunning: 0,
		acceptWakeNoTimerLoopActive: 0,
		acceptWakeNoTimerRefed: 0,
		acceptWakeNoTimerUnrefed: 0,
		acceptWakeNoTimerConnections: 0,
		acceptWakeNoTimerConnectionsMax: 0,
		acceptFirstPumpAfterNoTimerWakeCalls: 0,
		acceptFirstPumpAfterNoTimerWakeUs: 0,
		acceptFirstPumpAfterNoTimerWakeMaxUs: 0,
		acceptFirstPumpOriginListen: 0,
		acceptFirstPumpOriginEventWake: 0,
		acceptFirstPumpOriginTimer: 0,
		acceptFirstPumpOriginRef: 0,
		acceptFirstPumpOriginUnknown: 0,
		acceptFirstPumpResultConnection: 0,
		acceptFirstPumpResultTimeout: 0,
		acceptFirstPumpResultEmpty: 0,
		acceptWakeAlreadyRunning: 0,
		acceptWakeSocketScans: 0,
		acceptWakeSocketInvalidTargets: 0,
		acceptWakeSocketFound: 0,
		acceptWakeSocketMiss: 0,
		acceptWakeSocketUnixFound: 0,
		acceptWakeSocketUnixMiss: 0,
		dgramWakeAttempts: 0,
		dgramWakeInvalidTargets: 0,
		dgramWakeAlreadyRunning: 0,
		dgramWakeNoTimer: 0,
		dgramEventWakeups: 0,
		dgramWakeLoopbackHits: 0,
		dgramWakeLoopbackMisses: 0,
	};
}

var netBridgeTraceForced = false;

var netBridgeMetrics = createNetBridgeMetrics();

function isNetBridgeMetricsEnabled() {
	return netBridgeTraceForced || isNetBridgeTraceEnabled();
}

function netBridgeNowUs() {
	if (typeof __agentOsHrNowUs === "function") {
		return Math.round(__agentOsHrNowUs());
	}
	if (
		typeof performance !== "undefined" &&
		typeof performance.now === "function"
	) {
		return Math.round(performance.now() * 1000);
	}
	return Date.now() * 1000;
}

function countNetBridgeMetric(name, amount = 1) {
	if (!isNetBridgeMetricsEnabled()) return;
	netBridgeMetrics[name] = (netBridgeMetrics[name] ?? 0) + amount;
}

function maxNetBridgeMetric(name, value) {
	if (!isNetBridgeMetricsEnabled()) return;
	const numeric = Number(value);
	if (!Number.isFinite(numeric)) return;
	netBridgeMetrics[name] = Math.max(netBridgeMetrics[name] ?? 0, numeric);
}

function countReadFirstPumpOrigin(origin) {
	switch (origin) {
		case "connectWait":
			countNetBridgeMetric("readFirstPumpOriginConnectWait");
			break;
		case "acceptedHandle":
			countNetBridgeMetric("readFirstPumpOriginAcceptedHandle");
			break;
		case "eventWake":
			countNetBridgeMetric("readFirstPumpOriginEventWake");
			break;
		case "timer":
			countNetBridgeMetric("readFirstPumpOriginTimer");
			break;
		case "ref":
			countNetBridgeMetric("readFirstPumpOriginRef");
			break;
		case "tls":
			countNetBridgeMetric("readFirstPumpOriginTls");
			break;
		default:
			countNetBridgeMetric("readFirstPumpOriginUnknown");
			break;
	}
}

function countAcceptFirstPumpOrigin(origin) {
	switch (origin) {
		case "listen":
			countNetBridgeMetric("acceptFirstPumpOriginListen");
			break;
		case "eventWake":
			countNetBridgeMetric("acceptFirstPumpOriginEventWake");
			break;
		case "timer":
			countNetBridgeMetric("acceptFirstPumpOriginTimer");
			break;
		case "ref":
			countNetBridgeMetric("acceptFirstPumpOriginRef");
			break;
		default:
			countNetBridgeMetric("acceptFirstPumpOriginUnknown");
			break;
	}
}

exposeCustomGlobal("__agentOSNetBridgeMetrics", {
	get enabled() {
		return isNetBridgeMetricsEnabled();
	},
	enable() {
		netBridgeTraceForced = true;
	},
	disable() {
		netBridgeTraceForced = false;
	},
	reset() {
		netBridgeMetrics = createNetBridgeMetrics();
		if (typeof _benchNetTcpMetricsResetRaw !== "undefined") {
			_benchNetTcpMetricsResetRaw.applySync(void 0, []);
		}
	},
	snapshot() {
		let sidecarNetTrace = void 0;
		if (typeof _benchNetTcpMetricsSnapshotRaw !== "undefined") {
			sidecarNetTrace = _benchNetTcpMetricsSnapshotRaw.applySync(void 0, []);
		}
		return {
			...netBridgeMetrics,
			...(sidecarNetTrace ? { sidecarNetTrace } : {}),
		};
	},
});

function yieldBridgeMacrotask() {
	return new Promise((resolve) => {
		if (typeof setImmediate === "function") {
			setImmediate(resolve);
		} else {
			setTimeout(resolve, 0);
		}
	});
}

function netSocketDispatch(socketId, event, data) {
	if (socketId === "net_socket" && event && typeof event === "object") {
		const payload = event;
		if (payload.event === "accept") {
			const server = registeredNetServersById.get(payload.serverId);
			if (server) {
				wakeNetServerAccept(server);
			}
			return;
		}
		if (payload.event === "dgram") {
			globalThis._dgramSocketDispatch?.(payload);
			return;
		}
		if (payload.event === "http2") {
			globalThis._http2RetainDispatch?.(payload);
			return;
		}
		const target = getRegisteredNetSocket(payload.socketId);
		if (target) {
			countNetBridgeMetric("readEventWakeups");
			wakeSocketBridgeReads(target);
		}
		return;
	}
	if (socketId && typeof socketId === "object") {
		const payload = socketId;
		if (payload.event === "accept") {
			const server = registeredNetServersById.get(payload.serverId);
			if (server) {
				wakeNetServerAccept(server);
			}
			return;
		}
		if (payload.event === "dgram") {
			globalThis._dgramSocketDispatch?.(payload);
			return;
		}
		if (payload.event === "http2") {
			globalThis._http2RetainDispatch?.(payload);
			return;
		}
		const target = getRegisteredNetSocket(payload.socketId);
		if (target) {
			countNetBridgeMetric("readEventWakeups");
			wakeSocketBridgeReads(target);
		}
		return;
	}
	if (socketId === 0 && event.startsWith("http2:")) {
		debugBridgeNetwork("http2 dispatch via netSocket", event);
		try {
			const payload = data ? JSON.parse(data) : {};
			http2Dispatch(
				event.slice("http2:".length),
				Number(payload.id ?? 0),
				payload.data,
				payload.extra,
				payload.extraNumber,
				payload.extraHeaders,
				payload.flags,
			);
		} catch {}
		return;
	}
	const socket = getRegisteredNetSocket(socketId);
	if (!socket) return;
	switch (event) {
		case "connect": {
			socket._applySocketInfo(parseNetSocketInfo(data));
			socket._connected = true;
			socket.connecting = false;
			socket._touchTimeout();
			socket._emitNet("connect");
			socket._emitNet("ready");
			break;
		}
		case "secureConnect":
		case "secure": {
			const state = parseTlsState(data);
			if (state) {
				socket.authorized = state.authorized === true;
				socket.authorizationError = state.authorizationError;
				socket.alpnProtocol = state.alpnProtocol ?? false;
				socket.servername = state.servername ?? socket.servername;
				socket._tlsProtocol = state.protocol ?? null;
				socket._tlsSessionReused = state.sessionReused === true;
				socket._tlsCipher = state.cipher ?? null;
			}
			finalizeTlsUpgrade(socket, event);
			break;
		}
		case "data": {
			const buf =
				typeof Buffer !== "undefined"
					? Buffer.from(data, "base64")
					: new Uint8Array(0);
			socket._touchTimeout();
			socket._emitNet("data", buf);
			break;
		}
		case "end":
			socket._handleRemoteReadableEnd();
			break;
		case "session": {
			const session =
				typeof Buffer !== "undefined"
					? Buffer.from(data ?? "", "base64")
					: new Uint8Array(0);
			socket._tlsSession = Buffer.from(session);
			socket._emitNet("session", session);
			break;
		}
		case "error":
			if (data) {
				try {
					const parsed = JSON.parse(data);
					socket.authorized = parsed.authorized === true;
					socket.authorizationError = parsed.authorizationError;
				} catch {}
			}
			socket._emitNet("error", createBridgedTlsError(data));
			break;
		case "close":
			if (socket._remoteEnded) {
				socket._maybeEmitSocketClose();
			} else {
				// An abrupt transport close has no readable EOF to drain. Graceful
				// closes take the `_handleRemoteReadableEnd` path and must wait for
				// canonical Readable `end` ordering before `close`.
				socket._emitSocketClose(false);
			}
			break;
	}
}

exposeCustomGlobal("_netSocketDispatch", netSocketDispatch);

const NET_BRIDGE_MAX_RAW_WRITE_BYTES = 256 * 1024;

var NetSocket = class _NetSocket extends CanonicalDuplex {
	_livenessId = `net-socket:${++nextNetSocketLivenessId}`;
	_handleRefId = null;
	_socketId = 0;
	capabilityId;
	capabilityGeneration;
	_loopbackServer = null;
	_loopbackBuffer = Buffer.alloc(0);
	_loopbackDispatchRunning = false;
	_loopbackDispatchPending = false;
	_loopbackReadableEnded = false;
	_loopbackUpgradeSocket = null;
	_loopbackEventQueue = Promise.resolve();
	_noDelayState = false;
	_keepAliveState = false;
	_keepAliveDelaySeconds = 0;
	_refed = true;
	_bridgeReadLoopRunning = false;
	_bridgeReadRpcInFlight = false;
	_bridgeReadPumpQueued = false;
	_pendingBridgeWake = false;
	_pendingBridgeWakeRetries = 0;
	_bridgeReadPumpStarted = false;
	_applicationReadDemand = false;
	_bridgeReadFirstPumpBenchmarkScheduled = false;
	_readFirstPumpScheduleActive = false;
	_readFirstPumpScheduleQueuedAtUs = 0;
	_nextReadPumpOrigin = null;
	_firstReadNoTimerWakeAtUs = 0;
	_timeoutMs = 0;
	_timeoutTimer = null;
	_pendingBridgeWriteChunks = null;
	_pendingBridgeWriteCallbacks = null;
	_pendingBridgeWriteBytes = 0;
	_bridgeWriteFlushScheduled = false;
	_bridgeWriteTail = Promise.resolve();
	_bridgeWriteFlushQueuedAtUs = 0;
	_lastReadDeliveryEndUs = 0;
	_currentDataEmitStartUs = 0;
	_emittingData = false;
	_wroteDuringDataEmit = false;
	_lastDataEmitEndUs = 0;
	_readWakeQueuedAtUs = 0;
	_tlsUpgrading = false;
	_remoteEnded = false;
	_closeEmitted = false;
	_bridgeReleased = false;
	_connected = false;
	connecting = false;
	readyState = "open";
	remoteAddress;
	remotePort;
	remoteFamily;
	localAddress = "0.0.0.0";
	localPort = 0;
	localFamily = "IPv4";
	_localUnixPath;
	_remoteUnixPath;
	bytesRead = 0;
	bytesWritten = 0;
	pending = true;
	encrypted = false;
	authorized = false;
	authorizationError;
	servername;
	alpnProtocol = false;
	server;
	_tlsCipher = null;
	_tlsProtocol = null;
	_tlsSession = null;
	_tlsSessionReused = false;
	_handle = null;
	constructor(options) {
		super({
			allowHalfOpen: options?.allowHalfOpen === true,
			autoDestroy: false,
			emitClose: false,
			readableHighWaterMark: options?.readableHighWaterMark ?? 16 * 1024,
			writableHighWaterMark: options?.writableHighWaterMark ?? 16 * 1024,
		});
		// Canonical Duplex commits the readable and writable halves independently.
		// Observe both durable events so a paused buffer always drains through
		// `end` before `close`, while allowHalfOpen sockets remain writable until
		// their caller explicitly finishes that half.
		this.once("end", () => {
			this._releaseDeferredBridgeSocket();
			this._maybeEmitSocketClose();
		});
		this.once("finish", () => this._maybeEmitSocketClose());
		if (options?.handle) this._handle = options.handle;
	}
	connect(portOrOptions, hostOrCallback, callback) {
		if (typeof _netSocketConnectRaw === "undefined") {
			throw new Error(
				"net.Socket is not supported in sandbox (bridge not available)",
			);
		}
		const {
			host = "127.0.0.1",
			port = 0,
			path,
			localAddress,
			localPort,
			keepAlive,
			keepAliveInitialDelay,
			callback: cb,
		} = normalizeConnectArgs(portOrOptions, hostOrCallback, callback);
		if (cb) this.once("connect", cb);
		this.connecting = true;
		if (path) {
			delete this.remoteAddress;
			delete this.remotePort;
			delete this.remoteFamily;
			delete this.localAddress;
			delete this.localPort;
			delete this.localFamily;
			delete this.localPath;
			delete this.remotePath;
		} else {
			this.remoteAddress = host;
			this.remotePort = port;
			this.localAddress ??= "0.0.0.0";
			this.localPort ??= 0;
			this.localFamily ??= "IPv4";
		}
		this._remoteUnixPath = path;
		this.pending = false;
		let handle: any;
		try {
			handle = normalizeNetSocketHandle(
				_netSocketConnectRaw.applySync(void 0, [
					path
						? unixSocketRequest(path)
						: { host, port, localAddress, localPort },
				]),
			);
		} catch (error) {
			this.connecting = false;
			this.pending = false;
			queueMicrotask(() => {
				if (!this.destroyed) {
					// Emit before destroying so protocol users observe the original
					// connect failure before the socket's close notification. Passing
					// the error to destroy() would defer it through readable-stream and
					// allow close to win the race.
					this._emitNet("error", error);
					this.destroy();
				}
			});
			return this;
		}
		if (handle.loopbackHttpTarget) {
			this._loopbackHttpTarget = handle.loopbackHttpTarget;
			this._applySocketInfo(handle);
			this._connected = true;
			this.connecting = false;
			this._syncHandleRef();
			queueMicrotask(() => {
				if (this.destroyed || this._closeEmitted || !this._connected) {
					return;
				}
				this._touchTimeout();
				this._emitNet("connect");
				this._emitNet("ready");
			});
			return this;
		}
		debugBridgeNetwork(
			"socket connect",
			handle.socketId,
			host,
			port,
			path ?? null,
		);
		this._socketId = handle.socketId;
		this.capabilityId = handle.capabilityId;
		this.capabilityGeneration = handle.capabilityGeneration;
		this._handle = createConnectedSocketHandle(this._socketId);
		// Native sockets begin reading as soon as they connect, buffering data up
		// to the stream high-water mark even without a data/readable listener. In
		// particular, this is required to observe a peer FIN and emit close on a
		// paused socket. push(false) still clears demand and applies backpressure.
		this._applicationReadDemand = true;
		this._applySocketInfo(handle);
		registerNetSocket(this._socketId, this);
		this._syncHandleRef();
		void this._waitForConnect();
		if (keepAlive) {
			this.once("connect", () => {
				this.setKeepAlive(true, keepAliveInitialDelay);
			});
		}
		return this;
	}
	_read() {
		if (
			this.destroyed ||
			this._remoteEnded ||
			this._bridgeReleased ||
			this._closeEmitted
		) {
			return;
		}
		this._applicationReadDemand = true;
		if (typeof _netSocketSetReadInterestRaw !== "undefined" && this._socketId) {
			_netSocketSetReadInterestRaw.applySync(void 0, [this._socketId, true]);
		}
		if (this._connected && !this._tlsUpgrading && isRegisteredNetSocket(this)) {
			this._nextReadPumpOrigin = "duplexRead";
			queueSocketBridgeReadPump(this, "duplexRead");
		}
	}
	_write(data, encoding, callback) {
		const buf = Buffer.isBuffer(data) ? data : Buffer.from(data, encoding);
		if (this._loopbackServer || this._loopbackHttpTarget) {
			debugBridgeNetwork("socket write loopback", this._socketId, buf.length);
			this.bytesWritten += buf.length;
			if (this._loopbackUpgradeSocket) {
				this._touchTimeout();
				this._loopbackUpgradeSocket._pushData(buf);
				callback();
				return;
			}
			this._loopbackBuffer = Buffer.concat([this._loopbackBuffer, buf]);
			this._touchTimeout();
			this._dispatchLoopbackHttpRequest();
			callback();
			return;
		}
		if (
			typeof _netSocketWriteRaw === "undefined" ||
			this.destroyed ||
			!this._socketId
		) {
			callback(
				Object.assign(new Error("socket is not writable"), {
					code: "ERR_SOCKET_CLOSED",
				}),
			);
			return;
		}
		countNetBridgeMetric("userWriteCalls");
		countNetBridgeMetric("userWriteBytes", buf.length);
		if (this._emittingData) {
			this._wroteDuringDataEmit = true;
		}
		if (isNetBridgeMetricsEnabled()) {
			const nowUs = netBridgeNowUs();
			if (this._emittingData && this._currentDataEmitStartUs > 0) {
				countNetBridgeMetric("userWriteDuringDataEmitCalls");
				countNetBridgeMetric(
					"dataEmitStartToUserWriteUs",
					nowUs - this._currentDataEmitStartUs,
				);
			} else if (this._lastDataEmitEndUs > 0) {
				countNetBridgeMetric(
					"dataEmitEndToUserWriteUs",
					nowUs - this._lastDataEmitEndUs,
				);
			}
		}
		this.bytesWritten += buf.length;
		this._queueBridgeWrite(buf, callback, false);
	}
	_writev(entries, callback) {
		if (entries.length === 0) {
			callback();
			return;
		}
		let remaining = entries.length;
		let firstError = null;
		for (const entry of entries) {
			this._write(entry.chunk, entry.encoding, (error) => {
				firstError ??= error ?? null;
				remaining -= 1;
				if (remaining === 0) {
					callback(firstError);
				}
			});
		}
	}
	_final(callback) {
		if (this._loopbackServer || this._loopbackHttpTarget) {
			if (this._loopbackUpgradeSocket) {
				queueMicrotask(() => {
					this._loopbackUpgradeSocket?._pushEnd();
				});
			} else if (!this._loopbackReadableEnded) {
				queueMicrotask(() => {
					this._closeLoopbackReadable();
				});
			}
			callback();
			return;
		}
		if (
			typeof _netSocketEndRaw !== "undefined" &&
			this._socketId &&
			!this.destroyed
		) {
			// A program may wait only for `close`, without consuming `data` or
			// `readable`. Node still observes the peer FIN after the local write
			// half ends. Keep transport read interest enabled while no inbound
			// bytes are buffered so EOF cannot be stranded behind application
			// demand; normal push(false) backpressure still stops further data.
			if (this.readableLength === 0) {
				this._applicationReadDemand = true;
				if (typeof _netSocketSetReadInterestRaw !== "undefined") {
					try {
						_netSocketSetReadInterestRaw.applySync(void 0, [
							this._socketId,
							true,
						]);
					} catch (error) {
						callback(error);
						return;
					}
				}
				queueSocketBridgeReadPump(this, "localEnd");
			}
			const pendingWrites = this._flushBridgeWrites();
			debugBridgeNetwork("socket end", this._socketId);
			Promise.resolve(pendingWrites)
				.then(() => _netSocketEndRaw(this._socketId))
				.then(
					() => {
						countNetBridgeMetric("peerWakeOnShutdown");
						wakePeerBridgeReads(this);
						this._touchTimeout();
						callback();
						this._maybeEmitSocketClose();
					},
					(error) => callback(error),
				);
			return;
		}
		callback();
		this._maybeEmitSocketClose();
	}
	_destroy(error, callback) {
		debugBridgeNetwork(
			"socket destroy",
			this._socketId,
			error?.message ?? null,
		);
		this._applicationReadDemand = false;
		this._syncHandleRef();
		if (typeof _netSocketSetReadInterestRaw !== "undefined" && this._socketId) {
			try {
				_netSocketSetReadInterestRaw.applySync(void 0, [this._socketId, false]);
			} catch {
				// Destruction still closes the capability below; stale-handle errors
				// must not replace the original stream error.
			}
		}
		this._readableState.endEmitted = true;
		this._readableState.ended = true;
		this._clearTimeoutTimer();
		if (this._loopbackServer || this._loopbackHttpTarget) {
			this._loopbackUpgradeSocket?.destroy(error);
			this._loopbackUpgradeSocket = null;
			this._loopbackServer = null;
			this._loopbackHttpTarget = null;
		} else {
			this._releaseBridgeSocket();
		}
		callback(error);
		queueMicrotask(() => this._emitSocketClose(Boolean(error)));
	}
	_queueBridgeWrite(buf, callback, retainInput = false) {
		if (!this._pendingBridgeWriteChunks) {
			this._pendingBridgeWriteChunks = [];
			this._pendingBridgeWriteCallbacks = [];
		}
		const chunk = retainInput ? buf : Buffer.from(buf);
		this._pendingBridgeWriteChunks.push(chunk);
		this._pendingBridgeWriteBytes += chunk.length;
		countNetBridgeMetric("queuedWriteChunks");
		countNetBridgeMetric("queuedWriteBytes", chunk.length);
		if (retainInput) {
			countNetBridgeMetric("queuedWriteRetainedChunks");
			countNetBridgeMetric("queuedWriteRetainedBytes", chunk.length);
		} else {
			countNetBridgeMetric("queuedWriteCopiedChunks");
			countNetBridgeMetric("queuedWriteCopiedBytes", chunk.length);
		}
		maxNetBridgeMetric("writeBufferedBytesMax", this._pendingBridgeWriteBytes);
		maxNetBridgeMetric(
			"writeBufferedChunksMax",
			this._pendingBridgeWriteChunks.length,
		);
		if (callback) {
			this._pendingBridgeWriteCallbacks.push(callback);
		}
		if (this._emittingData) {
			countNetBridgeMetric("writeFlushInlineDuringDataEmit");
			void this._flushBridgeWrites();
		} else if (!this._bridgeWriteFlushScheduled) {
			this._bridgeWriteFlushScheduled = true;
			if (isNetBridgeMetricsEnabled()) {
				this._bridgeWriteFlushQueuedAtUs = netBridgeNowUs();
			}
			queueMicrotask(() => {
				void this._flushBridgeWrites();
			});
		}
	}
	_flushBridgeWrites() {
		const chunks = this._pendingBridgeWriteChunks;
		if (!chunks || chunks.length === 0) {
			this._bridgeWriteFlushScheduled = false;
			this._bridgeWriteFlushQueuedAtUs = 0;
			return this._bridgeWriteTail;
		}
		const callbacks = this._pendingBridgeWriteCallbacks ?? [];
		const totalBytes = this._pendingBridgeWriteBytes;
		this._pendingBridgeWriteChunks = null;
		this._pendingBridgeWriteCallbacks = null;
		this._pendingBridgeWriteBytes = 0;
		this._bridgeWriteFlushScheduled = false;
		if (
			this.destroyed ||
			!this._socketId ||
			typeof _netSocketWriteRaw === "undefined"
		) {
			const error = Object.assign(new Error("socket is not writable"), {
				code: "ERR_SOCKET_CLOSED",
			});
			for (const callback of callbacks) {
				callback(error);
			}
			return Promise.resolve();
		}
		const traceMetrics = isNetBridgeMetricsEnabled();
		if (traceMetrics && this._bridgeWriteFlushQueuedAtUs > 0) {
			const queuedToFlushUs = Math.max(
				0,
				netBridgeNowUs() - this._bridgeWriteFlushQueuedAtUs,
			);
			countNetBridgeMetric("writeQueuedToFlushStartUs", queuedToFlushUs);
			maxNetBridgeMetric("writeQueuedToFlushStartMaxUs", queuedToFlushUs);
			countNetBridgeMetric("writeFlushQueuedToRawUs", queuedToFlushUs);
			maxNetBridgeMetric("writeFlushQueuedToRawMaxUs", queuedToFlushUs);
			this._bridgeWriteFlushQueuedAtUs = 0;
		}
		debugBridgeNetwork(
			"socket write",
			this._socketId,
			totalBytes,
			chunks.length,
		);
		countNetBridgeMetric("flushCalls");
		countNetBridgeMetric("flushChunks", chunks.length);
		countNetBridgeMetric("flushBytes", totalBytes);
		const writeStartUs = traceMetrics ? netBridgeNowUs() : 0;
		const payloads = [];
		let pending = [];
		let pendingBytes = 0;
		const flushPending = () => {
			if (pendingBytes === 0) return;
			const payload =
				pending.length === 1
					? pending[0]
					: Buffer.concat(pending, pendingBytes);
			countNetBridgeMetric("writeRawCalls");
			countNetBridgeMetric("writeRawBytes", payload.length);
			payloads.push(payload);
			pending = [];
			pendingBytes = 0;
		};
		for (const chunk of chunks) {
			for (
				let offset = 0;
				offset < chunk.length;
				offset += NET_BRIDGE_MAX_RAW_WRITE_BYTES
			) {
				const piece = chunk.subarray(
					offset,
					offset + NET_BRIDGE_MAX_RAW_WRITE_BYTES,
				);
				if (
					pendingBytes > 0 &&
					pendingBytes + piece.length > NET_BRIDGE_MAX_RAW_WRITE_BYTES
				) {
					flushPending();
				}
				pending.push(piece);
				pendingBytes += piece.length;
				if (pendingBytes >= NET_BRIDGE_MAX_RAW_WRITE_BYTES) {
					flushPending();
				}
			}
		}
		flushPending();
		const socketId = this._socketId;
		// Every detached batch joins one socket-owned tail. A batch can be split
		// across several bounded bridge calls, and _writev() can detach another
		// batch before the first call settles. Without this tail, those independent
		// Promise chains can issue A1, B, A2 and violate TCP byte and callback order.
		let completion = this._bridgeWriteTail;
		for (const payload of payloads) {
			completion = completion.then(() => _netSocketWriteRaw(socketId, payload));
		}
		// Preserve rejection on the tail: later detached batches must fail behind a
		// failed predecessor instead of writing a suffix after a broken prefix.
		// Every completion below installs a rejection handler, so the tail is still
		// observed even when no later batch or _final() joins it.
		this._bridgeWriteTail = completion;
		completion.then(
			() => {
				if (traceMetrics) {
					countNetBridgeMetric(
						"writeRawElapsedUs",
						netBridgeNowUs() - writeStartUs,
					);
				}
				wakePeerBridgeReads(this);
				this._touchTimeout();
				for (const callback of callbacks) callback(null);
			},
			(error) => {
				const writeError =
					error instanceof Error ? error : new Error(String(error));
				for (const callback of callbacks) callback(writeError);
			},
		);
		return completion;
	}
	_emitSocketClose(hadError = false) {
		if (this._closeEmitted) {
			return;
		}
		this._closeEmitted = true;
		this._connected = false;
		this.connecting = false;
		this.pending = false;
		this.readable = false;
		this.writable = false;
		this._syncHandleRef();
		this._clearTimeoutTimer();
		if (this._socketId) {
			unregisterNetSocket(this._socketId);
		}
		this._deferBridgeReleaseUntilReadDrained = false;
		this._releaseBridgeSocket();
		this._emitNet("close", hadError);
	}
	_releaseDeferredBridgeSocket() {
		if (
			!this._deferBridgeReleaseUntilReadDrained ||
			this.readableLength > 0 ||
			!this._readableState.endEmitted
		) {
			return;
		}
		this._deferBridgeReleaseUntilReadDrained = false;
		this._maybeEmitSocketClose();
	}
	_releaseBridgeSocket() {
		if (
			this._bridgeReleased ||
			!this._socketId ||
			typeof _netSocketDestroyRaw === "undefined"
		) {
			return;
		}
		this._bridgeReleased = true;
		try {
			_netSocketDestroyRaw.applySync(void 0, [this._socketId]);
		} catch {}
		countNetBridgeMetric("peerWakeOnDestroy");
		wakePeerBridgeReads(this);
	}
	_maybeEmitSocketClose() {
		if (
			this.destroyed ||
			this._closeEmitted ||
			!this._remoteEnded ||
			!this._readableState.endEmitted ||
			!this.writableFinished
		) {
			return;
		}
		queueMicrotask(() => {
			if (
				!this.destroyed &&
				!this._closeEmitted &&
				this._remoteEnded &&
				this._readableState.endEmitted &&
				this.writableFinished
			) {
				this._emitSocketClose(false);
			}
		});
	}
	_handleRemoteReadableEnd() {
		if (this.destroyed || this._remoteEnded) {
			return;
		}
		debugBridgeNetwork("socket remote end", this._socketId);
		this._remoteEnded = true;
		this._applicationReadDemand = false;
		this.push(null);
		// `push(null)` alone leaves a paused, empty Readable in its pre-end
		// state. A zero-byte read commits EOF without consuming buffered data,
		// matching net.Socket's `end` before `close` behavior.
		this.read(0);
		if (!this._readableState.endEmitted) {
			this._deferBridgeReleaseUntilReadDrained = true;
		}
		this._maybeEmitSocketClose();
	}
	_applySocketInfo(info) {
		if (!info) {
			return;
		}
		if (info.localPath !== undefined || info.remotePath !== undefined) {
			this._localUnixPath = info.localPath;
			this._remoteUnixPath = info.remotePath ?? this._remoteUnixPath;
			delete this.localAddress;
			delete this.localPort;
			delete this.localFamily;
			delete this.remoteAddress;
			delete this.remotePort;
			delete this.remoteFamily;
			delete this.localPath;
			delete this.remotePath;
			return;
		}
		this.localAddress = info.localAddress;
		this.localPort = info.localPort;
		this.localFamily = info.localFamily;
		this.remoteAddress = info.remoteAddress ?? this.remoteAddress;
		this.remotePort = info.remotePort ?? this.remotePort;
		this.remoteFamily = info.remoteFamily ?? this.remoteFamily;
	}
	_applyAcceptedKeepAlive(initialDelay) {
		this._keepAliveState = true;
		this._keepAliveDelaySeconds = normalizeKeepAliveDelay(initialDelay);
	}
	static fromAcceptedHandle(handle, options) {
		const socket = new _NetSocket({ allowHalfOpen: options?.allowHalfOpen });
		socket._socketId = handle.socketId;
		socket.capabilityId = handle.info?.capabilityId;
		socket.capabilityGeneration = handle.info?.capabilityGeneration;
		socket._handle = createConnectedSocketHandle(handle.socketId);
		socket._applySocketInfo(handle.info);
		socket._applicationReadDemand = true;
		socket._connected = true;
		socket.connecting = false;
		socket.pending = false;
		registerNetSocket(handle.socketId, socket);
		socket._syncHandleRef();
		// Native Node starts the initial libuv read from the Socket constructor.
		// read(0) reaches _read() without consuming bytes, and push(false) still
		// stops transport reads at the configured high-water mark.
		socket.read(0);
		queueMicrotask(() => {
			if (!socket.destroyed && !socket._tlsUpgrading) {
				socket._nextReadPumpOrigin = "acceptedHandle";
				void socket._pumpBridgeReads();
			}
		});
		return socket;
	}
	setKeepAlive(enable, initialDelay) {
		const nextEnable = isTruthySocketOption(enable);
		const nextDelaySeconds = normalizeKeepAliveDelay(initialDelay);
		if (
			nextEnable === this._keepAliveState &&
			(!nextEnable || nextDelaySeconds === this._keepAliveDelaySeconds)
		) {
			return this;
		}
		this._keepAliveState = nextEnable;
		this._keepAliveDelaySeconds = nextEnable ? nextDelaySeconds : 0;
		debugBridgeNetwork(
			"socket setKeepAlive",
			this._socketId,
			nextEnable,
			nextDelaySeconds,
		);
		this._handle?.setKeepAlive?.(nextEnable, nextDelaySeconds);
		return this;
	}
	setNoDelay(noDelay) {
		const nextState = isTruthySocketOption(noDelay);
		if (nextState === this._noDelayState) {
			return this;
		}
		this._noDelayState = nextState;
		debugBridgeNetwork("socket setNoDelay", this._socketId, nextState);
		this._handle?.setNoDelay?.(nextState);
		return this;
	}
	setTimeout(timeout, callback) {
		const nextTimeout = normalizeSocketTimeout(timeout);
		if (callback !== void 0 && typeof callback !== "function") {
			throw createFunctionArgTypeError("callback", callback);
		}
		if (callback) {
			this.once("timeout", callback);
		}
		this._timeoutMs = nextTimeout;
		if (nextTimeout === 0) {
			this._clearTimeoutTimer();
			return this;
		}
		this._touchTimeout();
		return this;
	}
	ref() {
		this._refed = true;
		this._handle?.ref?.();
		this._syncHandleRef();
		if (this._timeoutTimer && typeof this._timeoutTimer.ref === "function") {
			this._timeoutTimer.ref();
		}
		if (
			!this.destroyed &&
			this._connected &&
			!this._loopbackServer &&
			!this._loopbackHttpTarget &&
			!this._bridgeReadLoopRunning
		) {
			this._nextReadPumpOrigin = "ref";
			void this._pumpBridgeReads();
		}
		return this;
	}
	unref() {
		this._refed = false;
		this._handle?.unref?.();
		this._syncHandleRef();
		if (this._timeoutTimer && typeof this._timeoutTimer.unref === "function") {
			this._timeoutTimer.unref();
		}
		return this;
	}
	_syncHandleRef() {
		const active =
			this._refed &&
			!this.destroyed &&
			!this._closeEmitted &&
			(this.connecting ||
				this._connected ||
				this._socketId !== 0 ||
				this._loopbackHttpTarget);
		if (!active) {
			if (this._handleRefId && typeof _unregisterHandle === "function") {
				_unregisterHandle(this._handleRefId);
			}
			this._handleRefId = null;
			return;
		}
		if (this._handleRefId === this._livenessId) {
			return;
		}
		if (this._handleRefId && typeof _unregisterHandle === "function") {
			_unregisterHandle(this._handleRefId);
		}
		this._handleRefId = this._livenessId;
		if (typeof _registerHandle === "function") {
			_registerHandle(this._handleRefId, "net socket");
		}
	}
	address() {
		if (
			this._localUnixPath !== undefined ||
			this._remoteUnixPath !== undefined
		) {
			return {};
		}
		return {
			port: this.localPort,
			family: this.localFamily,
			address: this.localAddress,
		};
	}
	getCipher() {
		return queryTlsSocket(this._socketId, "getCipher") ?? this._tlsCipher;
	}
	getSession() {
		const session = queryTlsSocket(this._socketId, "getSession");
		if (Buffer.isBuffer(session)) {
			this._tlsSession = Buffer.from(session);
			return Buffer.from(session);
		}
		return this._tlsSession ? Buffer.from(this._tlsSession) : null;
	}
	isSessionReused() {
		const reused = queryTlsSocket(this._socketId, "isSessionReused");
		return typeof reused === "boolean" ? reused : this._tlsSessionReused;
	}
	getPeerCertificate(detailed) {
		const cert = queryTlsSocket(
			this._socketId,
			"getPeerCertificate",
			detailed === true,
		);
		return cert && typeof cert === "object" ? cert : {};
	}
	getCertificate() {
		const cert = queryTlsSocket(this._socketId, "getCertificate");
		return cert && typeof cert === "object" ? cert : {};
	}
	getProtocol() {
		const protocol = queryTlsSocket(this._socketId, "getProtocol");
		return typeof protocol === "string" ? protocol : this._tlsProtocol;
	}
	emit(event, ...args) {
		return this._emitNet(event, ...args);
	}
	_emitNet(event, ...args) {
		const traceEmit =
			isNetBridgeMetricsEnabled() && (event === "readable" || event === "data");
		const emitStartUs = traceEmit ? netBridgeNowUs() : 0;
		if (event === "data") {
			this._emittingData = true;
			this._currentDataEmitStartUs = emitStartUs;
		}
		if (event === "readable") {
			countNetBridgeMetric("socketReadableEmits");
		} else if (event === "data") {
			countNetBridgeMetric("socketDataEmits");
		} else if (event === "end") {
			countNetBridgeMetric("socketEndEmits");
		} else if (event === "close") {
			countNetBridgeMetric("socketCloseEmits");
		} else if (event === "connect") {
			countNetBridgeMetric("socketConnectEmits");
		}
		let handled = false;
		try {
			handled = super.emit(event, ...args);
		} finally {
			if (event === "data") {
				this._emittingData = false;
			}
			if (traceEmit) {
				const elapsedUs = netBridgeNowUs() - emitStartUs;
				if (event === "readable") {
					countNetBridgeMetric("socketReadableEmitUs", elapsedUs);
					maxNetBridgeMetric("socketReadableEmitMaxUs", elapsedUs);
				} else if (event === "data") {
					countNetBridgeMetric("socketDataEmitUs", elapsedUs);
					maxNetBridgeMetric("socketDataEmitMaxUs", elapsedUs);
					this._lastDataEmitEndUs = netBridgeNowUs();
				}
			}
			if (event === "data") {
				this._currentDataEmitStartUs = 0;
			}
		}
		return handled;
	}
	_queueReadablePayload(payload) {
		if (!payload || payload.length === 0) {
			return;
		}
		const traceMetrics = isNetBridgeMetricsEnabled();
		const queueStartUs = traceMetrics ? netBridgeNowUs() : 0;
		try {
			countNetBridgeMetric("queueReadablePayloads");
			countNetBridgeMetric("queueReadableBytes", payload.length);
			const wantsMore = this.push(payload);
			maxNetBridgeMetric("queueReadableBytesMax", this.readableLength);
			if (!wantsMore) {
				this._applicationReadDemand = false;
				if (
					typeof _netSocketSetReadInterestRaw !== "undefined" &&
					this._socketId &&
					!this.destroyed &&
					!this._bridgeReleased &&
					isRegisteredNetSocket(this)
				) {
					_netSocketSetReadInterestRaw.applySync(void 0, [
						this._socketId,
						false,
					]);
				}
				countNetBridgeMetric("readBackpressureStops");
			}
			return wantsMore;
		} finally {
			if (traceMetrics) {
				const queueElapsedUs = netBridgeNowUs() - queueStartUs;
				countNetBridgeMetric("queueReadablePayloadElapsedUs", queueElapsedUs);
				maxNetBridgeMetric("queueReadablePayloadMaxUs", queueElapsedUs);
			}
		}
	}
	async _waitForConnect() {
		if (
			typeof _netSocketWaitConnectRaw === "undefined" ||
			this._socketId === 0
		) {
			return;
		}
		try {
			const infoJson = await _netSocketWaitConnectRaw.apply(
				void 0,
				[this._socketId],
				{ result: { promise: true } },
			);
			if (this.destroyed) {
				return;
			}
			this._applySocketInfo(parseNetSocketInfo(infoJson));
			await wakeNetServerAcceptForSocket(this);
			this._connected = true;
			this.connecting = false;
			if (
				this._applicationReadDemand &&
				typeof _netSocketSetReadInterestRaw !== "undefined"
			) {
				_netSocketSetReadInterestRaw.applySync(void 0, [this._socketId, true]);
			}
			debugBridgeNetwork(
				"socket connected",
				this._socketId,
				this.localAddress,
				this.localPort,
				this.remoteAddress,
				this.remotePort,
			);
			this._touchTimeout();
			debugBridgeNetwork(
				"socket emit connect",
				this._socketId,
				this.listenerCount("connect"),
			);
			this._emitNet("connect");
			debugBridgeNetwork(
				"socket emit ready",
				this._socketId,
				this.listenerCount("ready"),
			);
			this._emitNet("ready");
			if (!this._tlsUpgrading) {
				// Node's connect completion calls read(0), which starts EOF
				// observation even for sockets with only a close listener.
				this.read(0);
				this._nextReadPumpOrigin = "connectWait";
				await this._pumpBridgeReads();
			}
		} catch (error) {
			if (this.destroyed) {
				return;
			}
			const err = error instanceof Error ? error : new Error(String(error));
			debugBridgeNetwork(
				"socket connect error",
				this._socketId,
				err.message,
				err.stack ?? null,
			);
			this._emitNet("error", err);
			this.destroy();
		}
	}
	async _pumpBridgeReads() {
		if (!this._applicationReadDemand) {
			countNetBridgeMetric("readPumpSkippedNoDemand");
			return;
		}
		if (this._bridgeReadLoopRunning) {
			countNetBridgeMetric("readPumpSkippedLoopRunning");
			return;
		}
		if (this._bridgeReadRpcInFlight) {
			countNetBridgeMetric("readPumpSkippedRpcInFlight");
			return;
		}
		if (this._remoteEnded) {
			countNetBridgeMetric("readPumpSkippedRemoteEnded");
			return;
		}
		if (this._bridgeReleased) {
			countNetBridgeMetric("readPumpSkippedReleased");
			return;
		}
		if (this._closeEmitted) {
			countNetBridgeMetric("readPumpSkippedCloseEmitted");
			return;
		}
		if (typeof _netSocketReadRaw === "undefined") {
			countNetBridgeMetric("readPumpSkippedRawMissing");
			return;
		}
		if (!isRegisteredNetSocket(this)) {
			countNetBridgeMetric("readPumpSkippedUnregistered");
			return;
		}
		countNetBridgeMetric("readPumpRuns");
		const firstPumpRun = !this._bridgeReadPumpStarted;
		const scheduleActive = this._readFirstPumpScheduleActive === true;
		if (firstPumpRun) {
			countReadFirstPumpOrigin(this._nextReadPumpOrigin);
			if (this._firstReadNoTimerWakeAtUs > 0 && isNetBridgeMetricsEnabled()) {
				const elapsedUs = Math.max(
					0,
					netBridgeNowUs() - this._firstReadNoTimerWakeAtUs,
				);
				countNetBridgeMetric("readFirstPumpAfterNoTimerWakeCalls");
				countNetBridgeMetric("readFirstPumpAfterNoTimerWakeUs", elapsedUs);
				maxNetBridgeMetric("readFirstPumpAfterNoTimerWakeMaxUs", elapsedUs);
			}
			if (
				scheduleActive &&
				this._readFirstPumpScheduleQueuedAtUs > 0 &&
				isNetBridgeMetricsEnabled()
			) {
				const queuedToPumpStartUs = Math.max(
					0,
					netBridgeNowUs() - this._readFirstPumpScheduleQueuedAtUs,
				);
				countNetBridgeMetric(
					"readFirstPumpScheduleQueuedToPumpStartUs",
					queuedToPumpStartUs,
				);
				maxNetBridgeMetric(
					"readFirstPumpScheduleQueuedToPumpStartMaxUs",
					queuedToPumpStartUs,
				);
			}
		}
		this._readFirstPumpScheduleActive = false;
		this._readFirstPumpScheduleQueuedAtUs = 0;
		this._nextReadPumpOrigin = null;
		this._bridgeReadPumpStarted = true;
		let firstPumpResultRecorded = false;
		let scheduleResultRecorded = false;
		if (isNetBridgeMetricsEnabled() && this._readWakeQueuedAtUs > 0) {
			const queuedToPumpUs = Math.max(
				0,
				netBridgeNowUs() - this._readWakeQueuedAtUs,
			);
			countNetBridgeMetric("readWakeQueuedToPumpStartUs", queuedToPumpUs);
			maxNetBridgeMetric("readWakeQueuedToPumpStartMaxUs", queuedToPumpUs);
			this._readWakeQueuedAtUs = 0;
		}
		this._bridgeReadLoopRunning = true;
		let deliveredPayloads = 0;
		try {
			while (
				this._applicationReadDemand &&
				!this._remoteEnded &&
				!this.destroyed &&
				!this._bridgeReleased &&
				!this._closeEmitted &&
				isRegisteredNetSocket(this)
			) {
				const traceMetrics = isNetBridgeMetricsEnabled();
				const postDeliveryProbeStartUs =
					traceMetrics && this._lastReadDeliveryEndUs > 0
						? netBridgeNowUs()
						: 0;
				if (postDeliveryProbeStartUs > 0) {
					countNetBridgeMetric("readPostDeliveryProbeCalls");
					countNetBridgeMetric("readPostDeliveryNextRawCalls");
					countNetBridgeMetric(
						"readPostDeliveryToProbeStartUs",
						postDeliveryProbeStartUs - this._lastReadDeliveryEndUs,
					);
					if (this._bridgeWriteFlushScheduled) {
						countNetBridgeMetric("readPostDeliveryPendingWriteFlushes");
						countNetBridgeMetric(
							"readPostDeliveryPendingWriteBytes",
							this._pendingBridgeWriteBytes,
						);
					}
					this._lastReadDeliveryEndUs = 0;
				}
				if (!isRegisteredNetSocket(this)) {
					return;
				}
				// applySync may run nested JavaScript while the host response is in
				// flight. Serialize the transport read itself so a reentrant pump
				// cannot issue a second read before this one commits EOF or data.
				if (this._bridgeReadRpcInFlight) {
					return;
				}
				countNetBridgeMetric("readRawCalls");
				const readStartUs = traceMetrics ? netBridgeNowUs() : 0;
				this._bridgeReadRpcInFlight = true;
				let chunk: string | Uint8Array | null;
				try {
					chunk = _netSocketReadRaw.applySync(void 0, [this._socketId]);
				} finally {
					this._bridgeReadRpcInFlight = false;
				}
				if (this._remoteEnded) {
					return;
				}
				if (traceMetrics) {
					const readElapsedUs = netBridgeNowUs() - readStartUs;
					countNetBridgeMetric("readRawElapsedUs", readElapsedUs);
					if (postDeliveryProbeStartUs > 0) {
						countNetBridgeMetric(
							"readPostDeliveryProbeElapsedUs",
							readElapsedUs,
						);
						maxNetBridgeMetric("readPostDeliveryProbeMaxUs", readElapsedUs);
					}
				}
				if (this.destroyed) {
					return;
				}
				if (chunk === NET_BRIDGE_TIMEOUT_SENTINEL) {
					if (firstPumpRun && !firstPumpResultRecorded) {
						firstPumpResultRecorded = true;
						countNetBridgeMetric("readFirstPumpResultTimeout");
					}
					if (scheduleActive && !scheduleResultRecorded) {
						scheduleResultRecorded = true;
						countNetBridgeMetric("readFirstPumpScheduleResultTimeout");
					}
					countNetBridgeMetric("readTimeoutSentinels");
					if (postDeliveryProbeStartUs > 0) {
						countNetBridgeMetric("readPostDeliveryProbeTimeoutSentinels");
						countNetBridgeMetric("readPostDeliveryNextRawTimeoutSentinels");
					}
					if (this._pendingBridgeWake) {
						this._pendingBridgeWake = false;
						this._pendingBridgeWakeRetries = 0;
						countNetBridgeMetric("readPendingWakeImmediateRetries");
						this._nextReadPumpOrigin = "eventWake";
						await Promise.resolve();
						continue;
					}
					countNetBridgeMetric("readWaitsForWake");
					return;
				}
				if (chunk === null) {
					if (firstPumpRun && !firstPumpResultRecorded) {
						firstPumpResultRecorded = true;
						countNetBridgeMetric("readFirstPumpResultEnd");
					}
					if (scheduleActive && !scheduleResultRecorded) {
						scheduleResultRecorded = true;
						countNetBridgeMetric("readFirstPumpScheduleResultEnd");
					}
					countNetBridgeMetric("readEndEvents");
					this._pendingBridgeWake = false;
					this._pendingBridgeWakeRetries = 0;
					this._handleRemoteReadableEnd();
					return;
				}
				if (postDeliveryProbeStartUs > 0) {
					countNetBridgeMetric("readPostDeliveryProbeDataEvents");
					countNetBridgeMetric("readPostDeliveryNextRawDataEvents");
				}
				if (firstPumpRun && !firstPumpResultRecorded) {
					firstPumpResultRecorded = true;
					countNetBridgeMetric("readFirstPumpResultData");
				}
				if (scheduleActive && !scheduleResultRecorded) {
					scheduleResultRecorded = true;
					countNetBridgeMetric("readFirstPumpScheduleResultData");
				}
				let payload: any;
				if (typeof chunk === "string") {
					const decodeStartUs = traceMetrics ? netBridgeNowUs() : 0;
					payload = Buffer.from(chunk, "base64");
					if (traceMetrics) {
						countNetBridgeMetric("readBase64DecodeCalls");
						countNetBridgeMetric("readBase64DecodeBytes", payload.length);
						countNetBridgeMetric("readBase64DecodeChars", chunk.length);
						countNetBridgeMetric(
							"readBase64DecodeUs",
							netBridgeNowUs() - decodeStartUs,
						);
					}
				} else {
					const materializeStartUs = traceMetrics ? netBridgeNowUs() : 0;
					payload = Buffer.from(chunk);
					if (traceMetrics) {
						countNetBridgeMetric("readPayloadMaterializeCalls");
						countNetBridgeMetric("readPayloadMaterializeBytes", payload.length);
						countNetBridgeMetric(
							"readPayloadMaterializeUs",
							netBridgeNowUs() - materializeStartUs,
						);
					}
				}
				debugBridgeNetwork("socket data", this._socketId, payload.length);
				countNetBridgeMetric("readDataEvents");
				countNetBridgeMetric("readBytes", payload.length);
				this.bytesRead += payload.length;
				this._touchTimeout();
				// Yield to a macrotask before delivering each payload so that socket
				// bytes surface across distinct event-loop turns, exactly as they do
				// on real Node where each readable arrives in its own I/O callback.
				// _netSocketReadRaw is synchronous, so without this the loop drains an
				// entire HTTP response and emits "readable"/"data" in one synchronous
				// burst. That collapses the turn boundaries undici's keep-alive socket
				// recycling depends on: its setImmediate(client[kResume]) never runs
				// before the caller's microtask dispatches the next request, so the
				// pool keeps every Client at kNeedDrain and allocates a fresh
				// Client+socket per request — leaking EventEmitter listeners
				// (MaxListenersExceededWarning) and unbounded memory until the VM dies.
				if (deliveredPayloads > 0) {
					countNetBridgeMetric("readMacrotaskYields");
					const yieldStartUs = traceMetrics ? netBridgeNowUs() : 0;
					await yieldBridgeMacrotask();
					if (traceMetrics) {
						const yieldElapsedUs = netBridgeNowUs() - yieldStartUs;
						countNetBridgeMetric("readMacrotaskYieldElapsedUs", yieldElapsedUs);
						maxNetBridgeMetric("readMacrotaskYieldMaxUs", yieldElapsedUs);
					}
					if (this.destroyed) {
						return;
					}
				} else {
					countNetBridgeMetric("readFirstPayloadImmediateDeliveries");
				}
				this._pendingBridgeWake = false;
				this._pendingBridgeWakeRetries = 0;
				deliveredPayloads++;
				if (!this._queueReadablePayload(payload)) {
					return;
				}
				if (this._wroteDuringDataEmit) {
					this._wroteDuringDataEmit = false;
					countNetBridgeMetric("readPumpYieldAfterInlineDataWrite");
					queueSocketBridgeReadPump(this, "postDeliveryInlineWrite");
					return;
				}
				if (this._bridgeWriteFlushScheduled) {
					countNetBridgeMetric("readPumpYieldToPendingWriteFlush");
					queueSocketBridgeReadPump(this, "postDeliveryWriteFlush");
					return;
				}
				if (traceMetrics) {
					this._lastReadDeliveryEndUs = netBridgeNowUs();
				}
			}
		} finally {
			this._bridgeReadLoopRunning = false;
			if (
				this._applicationReadDemand &&
				this._pendingBridgeWake &&
				!this.destroyed &&
				isRegisteredNetSocket(this)
			) {
				queueSocketBridgeReadPump(this, "eventWake");
			}
		}
	}
	_dispatchLoopbackHttpRequest() {
		if (
			(!this._loopbackServer && !this._loopbackHttpTarget) ||
			this.destroyed
		) {
			return;
		}
		if (this._loopbackDispatchRunning) {
			this._loopbackDispatchPending = true;
			return;
		}
		this._loopbackDispatchRunning = true;
		void this._processLoopbackHttpRequests().finally(() => {
			this._loopbackDispatchRunning = false;
			if (this._loopbackDispatchPending && this._loopbackBuffer.length > 0) {
				this._loopbackDispatchPending = false;
				this._dispatchLoopbackHttpRequest();
			} else {
				this._loopbackDispatchPending = false;
			}
		});
	}
	async _processLoopbackHttpRequests() {
		let closeAfterDrain = false;
		while (
			(this._loopbackServer || this._loopbackHttpTarget) &&
			!this.destroyed
		) {
			const parserServer = this._loopbackServer ?? { listenerCount: () => 0 };
			const parsed = parseLoopbackRequestBuffer(
				this._loopbackBuffer,
				parserServer,
			);
			if (parsed.kind === "incomplete") {
				if (closeAfterDrain) {
					this._closeLoopbackReadable();
				}
				return;
			}
			if (parsed.kind === "bad-request") {
				this._pushLoopbackData(createBadRequestResponseBuffer());
				if (parsed.closeConnection) {
					this._closeLoopbackReadable();
				}
				this._loopbackBuffer = Buffer.alloc(0);
				return;
			}
			this._loopbackBuffer = this._loopbackBuffer.subarray(
				parsed.bytesConsumed,
			);
			if (parsed.upgradeHead) {
				this._dispatchLoopbackUpgrade(parsed.request, parsed.upgradeHead);
				return;
			}
			let responseJson: any;
			if (this._loopbackHttpTarget) {
				if (typeof _networkHttpServerRequestRaw === "undefined") {
					throw new Error("HTTP loopback bridge is not available");
				}
				responseJson = _networkHttpServerRequestRaw.applySync(void 0, [
					{
						...this._loopbackHttpTarget,
						request: JSON.stringify(parsed.request),
					},
				]);
			} else {
				({ responseJson } = await dispatchLoopbackServerRequest(
					this._loopbackServer,
					parsed.request,
				));
			}
			const response = JSON.parse(responseJson);
			const serialized = serializeLoopbackResponse(
				response,
				parsed.request,
				parsed.closeConnection,
			);
			if (!closeAfterDrain && serialized.payload.length > 0) {
				this._pushLoopbackData(serialized.payload);
			}
			if (serialized.closeConnection) {
				closeAfterDrain = true;
				if (this._loopbackBuffer.length === 0) {
					this._closeLoopbackReadable();
					return;
				}
			}
		}
	}
	_pushLoopbackData(data) {
		if (data.length === 0 || this._loopbackReadableEnded) {
			return;
		}
		const payload = Buffer.from(data);
		this._queueLoopbackEvent(() => {
			if (this.destroyed) {
				return;
			}
			this.bytesRead += payload.length;
			this._touchTimeout();
			this._queueReadablePayload(payload);
		});
	}
	_closeLoopbackReadable() {
		if (this._loopbackReadableEnded) {
			return;
		}
		this._loopbackReadableEnded = true;
		this._remoteEnded = true;
		this._applicationReadDemand = false;
		this._clearTimeoutTimer();
		this._queueLoopbackEvent(() => {
			this.push(null);
			if (!this._readableState.endEmitted) {
				this._deferBridgeReleaseUntilReadDrained = true;
			}
			this._maybeEmitSocketClose();
		});
	}
	_queueLoopbackEvent(callback) {
		this._loopbackEventQueue = this._loopbackEventQueue.then(
			() =>
				new Promise((resolve) => {
					queueMicrotask(() => {
						try {
							callback();
						} finally {
							resolve();
						}
					});
				}),
		);
	}
	_dispatchLoopbackUpgrade(request, head) {
		if (!this._loopbackServer) {
			return;
		}
		try {
			const socket = new DirectTunnelSocket({
				host: this.remoteAddress,
				port: this.remotePort,
			});
			socket._attachPeer({
				_pushData: (data) => this._pushLoopbackData(data),
				_pushEnd: () => this._closeLoopbackReadable(),
			});
			this._loopbackUpgradeSocket = socket;
			this._loopbackServer._emit(
				"upgrade",
				new ServerIncomingMessage(request),
				socket,
				head,
			);
		} catch (error) {
			const rethrow = error instanceof Error ? error : new Error(String(error));
			let handled = false;
			let exitCodeFromHandler = null;
			if (
				typeof process !== "undefined" &&
				typeof process.emit === "function"
			) {
				const processEmitter = process;
				try {
					handled = processEmitter.emit(
						"uncaughtException",
						rethrow,
						"uncaughtException",
					);
				} catch (emitError) {
					if (
						emitError &&
						typeof emitError === "object" &&
						emitError.name === "ProcessExitError"
					) {
						handled = true;
						const exitCode = Number(emitError.code);
						exitCodeFromHandler = Number.isFinite(exitCode) ? exitCode : 0;
					} else {
						throw emitError;
					}
				}
			}
			if (handled) {
				if (exitCodeFromHandler !== null) {
					process.exitCode = exitCodeFromHandler;
				}
				this._loopbackServer?.close();
				this.destroy();
				return;
			}
			throw rethrow;
		}
	}
	// Upgrade this socket to TLS
	_upgradeTls(options) {
		if (typeof _netSocketUpgradeTlsAsyncRaw === "undefined") {
			throw new Error(
				"tls.connect is not supported in sandbox (bridge not available)",
			);
		}
		this._tlsUpgrading = true;
		if (
			this._loopbackServer &&
			(typeof this._socketId !== "string" || this._socketId.length === 0)
		) {
			queueMicrotask(() => {
				if (!this.destroyed) {
					finalizeTlsUpgrade(this);
				}
			});
			return;
		}
		const finalize = () => {
			if (!this.destroyed) {
				finalizeTlsUpgrade(this);
			}
		};
		const fail = (error) => {
			if (this.destroyed) {
				return;
			}
			this._tlsUpgrading = false;
			const tlsError =
				error instanceof Error ? error : new Error(String(error));
			this._emitNet("error", tlsError);
			this.destroy();
		};
		let upgrade: unknown;
		try {
			upgrade = _netSocketUpgradeTlsAsyncRaw(
				this._socketId,
				JSON.stringify(options ?? {}),
			);
		} catch (error) {
			fail(error);
			return;
		}
		Promise.resolve(upgrade).then(() => {
			if (options?.isServer) {
				queueMicrotask(finalize);
			} else {
				setTimeout(() => {
					if (!this.destroyed) {
						finalizeTlsUpgrade(this, "secureConnect", false);
					}
				}, 0);
			}
		}, fail);
	}
	_touchTimeout() {
		if (this._timeoutMs === 0 || this.destroyed) {
			return;
		}
		this._clearTimeoutTimer();
		this._timeoutTimer = setTimeout(() => {
			this._timeoutTimer = null;
			if (this.destroyed) {
				return;
			}
			this._emitNet("timeout");
		}, this._timeoutMs);
		if (!this._refed && typeof this._timeoutTimer.unref === "function") {
			this._timeoutTimer.unref();
		}
	}
	_clearTimeoutTimer() {
		if (this._timeoutTimer) {
			clearTimeout(this._timeoutTimer);
			this._timeoutTimer = null;
		}
	}
};

function netConnect(portOrOptions, hostOrCallback, callback) {
	const socket = new NetSocket();
	socket.connect(portOrOptions, hostOrCallback, callback);
	return socket;
}

var NetServer = class {
	_listeners = {};
	_onceListeners = {};
	_serverId = 0;
	capabilityId;
	capabilityGeneration;
	_address = null;
	_acceptLoopActive = false;
	_acceptLoopRunning = false;
	_acceptPumpQueued = false;
	_pendingAcceptWake = false;
	_acceptPumpStarted = false;
	_nextAcceptPumpOrigin = null;
	_firstAcceptNoTimerWakeAtUs = 0;
	_acceptWakeQueuedAtUs = 0;
	_handleRefId = null;
	_connections = /* @__PURE__ */ new Set();
	_closePending = false;
	_closeQueued = false;
	_pendingTransportCloses = 0;
	_pendingRelisten = null;
	_refed = true;
	listening = false;
	keepAlive = false;
	keepAliveInitialDelay = 0;
	allowHalfOpen = false;
	maxConnections;
	_handle;
	constructor(optionsOrListener, maybeListener) {
		if (typeof optionsOrListener === "function") {
			this.on("connection", optionsOrListener);
		} else {
			this.allowHalfOpen = optionsOrListener?.allowHalfOpen === true;
			this.keepAlive = optionsOrListener?.keepAlive === true;
			this.keepAliveInitialDelay =
				optionsOrListener?.keepAliveInitialDelay ?? 0;
			if (maybeListener) {
				this.on("connection", maybeListener);
			}
		}
		this._handle = {
			onconnection: (err, clientHandle) => {
				if (err) {
					this._emit("error", err);
					return;
				}
				if (!clientHandle) {
					return;
				}
				if (
					typeof this.maxConnections === "number" &&
					this.maxConnections >= 0 &&
					this._connections.size >= this.maxConnections
				) {
					this._emit("drop", {
						localAddress: clientHandle.info.localAddress,
						localPort: clientHandle.info.localPort,
						localFamily: clientHandle.info.localFamily,
						remoteAddress: clientHandle.info.remoteAddress,
						remotePort: clientHandle.info.remotePort,
						remoteFamily: clientHandle.info.remoteFamily,
					});
					_netSocketDestroyRaw?.applySync(void 0, [clientHandle.socketId]);
					countNetBridgeMetric("peerWakeOnDestroy");
					wakePeerBridgeReads({
						_socketId: clientHandle.socketId,
						localPort: clientHandle.info.localPort,
						remotePort: clientHandle.info.remotePort,
						_localUnixPath: clientHandle.info.localPath,
						_remoteUnixPath: clientHandle.info.remotePath,
					});
					return;
				}
				if (this.keepAlive) {
					clientHandle.setKeepAlive?.(true, this.keepAliveInitialDelay);
				}
				const socket = NetSocket.fromAcceptedHandle(clientHandle, {
					allowHalfOpen: this.allowHalfOpen,
				});
				socket.server = this;
				this._connections.add(socket);
				socket.once("close", () => {
					this._connections.delete(socket);
					if (this._closePending) {
						countNetBridgeMetric("serverCloseConnectionDrainEvents");
					}
					this._emitCloseIfDrained();
				});
				if (this.keepAlive) {
					socket._applyAcceptedKeepAlive(this.keepAliveInitialDelay);
				}
				countNetBridgeMetric("connectionEmits");
				this._emit("connection", socket);
			},
		};
	}
	listen(portOrOptions, hostOrCallback, backlogOrCallback, callback) {
		if (
			typeof _netServerListenRaw === "undefined" ||
			typeof _netServerAcceptRaw === "undefined"
		) {
			throw new Error("net.createServer is not supported in sandbox");
		}
		if (this._pendingTransportCloses > 0) {
			if (this._pendingRelisten) {
				const error = new Error(
					"Listen method has been called more than once without closing.",
				);
				error.code = "ERR_SERVER_ALREADY_LISTEN";
				throw error;
			}
			this._pendingRelisten = () => {
				this.listen(portOrOptions, hostOrCallback, backlogOrCallback, callback);
			};
			return this;
		}
		const {
			port,
			host,
			path,
			backlog,
			readableAll,
			writableAll,
			callback: cb,
		} = normalizeListenArgs(
			portOrOptions,
			hostOrCallback,
			backlogOrCallback,
			callback,
		);
		if (cb) {
			this.once("listening", cb);
		}
		try {
			const resultValue = _netServerListenRaw.applySyncPromise(void 0, [
				{
					port,
					host,
					...(path ? unixSocketRequest(path) : {}),
					backlog,
					readableAll,
					writableAll,
				},
			]);
			const result =
				typeof resultValue === "string" ? JSON.parse(resultValue) : resultValue;
			const address = result.address ?? result;
			this._serverId = result.serverId;
			this.capabilityId = result.capabilityId;
			this.capabilityGeneration = result.capabilityGeneration;
			this._address = address.localPath
				? address.localPath
				: {
						address: address.localAddress,
						family: address.localFamily ?? address.family,
						port: address.localPort,
					};
			this.listening = true;
			registerNetServer(this);
			this._syncHandleRef();
			this._acceptLoopActive = true;
			queueMicrotask(() => {
				if (!this.listening || this._serverId === 0) {
					return;
				}
				this._emit("listening");
				this._nextAcceptPumpOrigin = "listen";
				void this._pumpAccepts();
			});
		} catch (error) {
			queueMicrotask(() => {
				this._emit("error", error);
			});
		}
		return this;
	}
	close(callback) {
		countNetBridgeMetric("serverCloseCalls");
		countNetBridgeMetric(
			"serverCloseConnectionsAtCall",
			this._connections.size,
		);
		maxNetBridgeMetric(
			"serverCloseConnectionsAtCallMax",
			this._connections.size,
		);
		if (this._connections.size > 0) {
			countNetBridgeMetric("serverCloseCallsWithConnections");
		}
		if (callback) {
			this.once("close", callback);
		}
		if (!this.listening || typeof _netServerCloseRaw === "undefined") {
			this._closePending = true;
			this._emitCloseIfDrained();
			return this;
		}
		this._closePending = true;
		this.listening = false;
		this._acceptLoopActive = false;
		this._pendingAcceptWake = false;
		this._acceptPumpQueued = false;
		unregisterNetServer(this);
		this._syncHandleRef();
		const serverId = this._serverId;
		const unlinkNodePath =
			typeof this._address === "string" && !this._address.startsWith("\0");
		this._serverId = 0;
		this._address = null;
		this._pendingTransportCloses += 1;
		const finishTransportClose = (error) => {
			if (this._pendingTransportCloses <= 0) {
				throw new Error(
					"ERR_AGENTOS_NET_CLOSE_ACCOUNTING: listener transport close completed without a pending operation",
				);
			}
			this._pendingTransportCloses -= 1;
			this._emitCloseIfDrained();
			if (this._pendingTransportCloses === 0 && this._pendingRelisten) {
				const relisten = this._pendingRelisten;
				this._pendingRelisten = null;
				relisten();
			}
			if (error !== void 0) {
				const transportError =
					error instanceof Error ? error : new Error(String(error));
				if (!this._emit("error", transportError)) {
					queueMicrotask(() => {
						throw transportError;
					});
				}
			}
		};
		try {
			// Node removes a pathname Unix-domain socket when Server.close()
			// completes. TCP and abstract listeners leave no filesystem path.
			Promise.resolve(_netServerCloseRaw(serverId, unlinkNodePath)).then(
				() => finishTransportClose(),
				finishTransportClose,
			);
		} catch (error) {
			finishTransportClose(error);
		}
		return this;
	}
	_emitCloseIfDrained() {
		if (
			!this._closePending ||
			this._closeQueued ||
			this.listening ||
			this._pendingTransportCloses !== 0 ||
			this._connections.size !== 0
		) {
			return;
		}
		this._closeQueued = true;
		queueMicrotask(() => {
			if (!this._closePending) {
				return;
			}
			this._closeQueued = false;
			this._closePending = false;
			countNetBridgeMetric("serverCloseEmits");
			this._emit("close");
		});
	}
	address() {
		return this._address;
	}
	getConnections(callback) {
		if (typeof callback !== "function") {
			throw createFunctionArgTypeError("callback", callback);
		}
		queueMicrotask(() => {
			callback(null, this._connections.size);
		});
		return this;
	}
	ref() {
		this._refed = true;
		this._syncHandleRef();
		if (this.listening && this._acceptLoopActive && !this._acceptLoopRunning) {
			this._nextAcceptPumpOrigin = "ref";
			void this._pumpAccepts();
		}
		return this;
	}
	unref() {
		this._refed = false;
		this._syncHandleRef();
		return this;
	}
	on(event, listener) {
		if (!this._listeners[event]) this._listeners[event] = [];
		this._listeners[event].push(listener);
		return this;
	}
	once(event, listener) {
		if (!this._onceListeners[event]) this._onceListeners[event] = [];
		this._onceListeners[event].push(listener);
		return this;
	}
	emit(event, ...args) {
		return this._emit(event, ...args);
	}
	_emit(event, ...args) {
		let handled = false;
		const listeners = this._listeners[event];
		if (listeners) {
			for (const fn of [...listeners]) {
				fn.call(this, ...args);
				handled = true;
			}
		}
		const onceListeners = this._onceListeners[event];
		if (onceListeners) {
			this._onceListeners[event] = [];
			for (const fn of [...onceListeners]) {
				fn.call(this, ...args);
				handled = true;
			}
		}
		return handled;
	}
	_syncHandleRef() {
		if (!this.listening || this._serverId === 0 || !this._refed) {
			if (this._handleRefId && typeof _unregisterHandle === "function") {
				_unregisterHandle(this._handleRefId);
			}
			this._handleRefId = null;
			return;
		}
		const nextHandleId = `${NET_SERVER_HANDLE_PREFIX}${this._serverId}`;
		if (this._handleRefId === nextHandleId) {
			return;
		}
		if (this._handleRefId && typeof _unregisterHandle === "function") {
			_unregisterHandle(this._handleRefId);
		}
		this._handleRefId = nextHandleId;
		if (typeof _registerHandle === "function") {
			_registerHandle(this._handleRefId, "net server");
		}
	}
	async _pumpAccepts() {
		if (typeof _netServerAcceptRaw === "undefined" || this._acceptLoopRunning) {
			if (this._acceptLoopRunning) {
				countNetBridgeMetric("acceptLoopAlreadyRunning");
			}
			return;
		}
		// A direct listen/ref pump can begin before an already-queued readiness
		// microtask. It consumes the same durable pending bit, so cancel that
		// redundant scheduled turn instead of polling the empty accept queue twice.
		this._acceptPumpQueued = false;
		countNetBridgeMetric("acceptPumpRuns");
		const firstPumpRun = !this._acceptPumpStarted;
		if (firstPumpRun) {
			countAcceptFirstPumpOrigin(this._nextAcceptPumpOrigin);
			if (this._firstAcceptNoTimerWakeAtUs > 0 && isNetBridgeMetricsEnabled()) {
				const elapsedUs = Math.max(
					0,
					netBridgeNowUs() - this._firstAcceptNoTimerWakeAtUs,
				);
				countNetBridgeMetric("acceptFirstPumpAfterNoTimerWakeCalls");
				countNetBridgeMetric("acceptFirstPumpAfterNoTimerWakeUs", elapsedUs);
				maxNetBridgeMetric("acceptFirstPumpAfterNoTimerWakeMaxUs", elapsedUs);
			}
		}
		this._nextAcceptPumpOrigin = null;
		this._acceptPumpStarted = true;
		let firstPumpResultRecorded = false;
		if (isNetBridgeMetricsEnabled() && this._acceptWakeQueuedAtUs > 0) {
			const queuedToPumpUs = Math.max(
				0,
				netBridgeNowUs() - this._acceptWakeQueuedAtUs,
			);
			countNetBridgeMetric("acceptWakeQueuedToPumpStartUs", queuedToPumpUs);
			maxNetBridgeMetric("acceptWakeQueuedToPumpStartMaxUs", queuedToPumpUs);
			this._acceptWakeQueuedAtUs = 0;
		}
		this._acceptLoopRunning = true;
		try {
			while (this._acceptLoopActive && this._serverId !== 0) {
				countNetBridgeMetric("acceptRawCalls");
				const traceMetrics = isNetBridgeMetricsEnabled();
				const acceptStartUs = traceMetrics ? netBridgeNowUs() : 0;
				const payload = _netServerAcceptRaw.applySync(void 0, [this._serverId]);
				if (traceMetrics) {
					countNetBridgeMetric(
						"acceptRawElapsedUs",
						netBridgeNowUs() - acceptStartUs,
					);
				}
				if (payload === NET_BRIDGE_TIMEOUT_SENTINEL) {
					if (firstPumpRun && !firstPumpResultRecorded) {
						firstPumpResultRecorded = true;
						countNetBridgeMetric("acceptFirstPumpResultTimeout");
					}
					countNetBridgeMetric("acceptTimeoutSentinels");
					if (this._pendingAcceptWake) {
						this._pendingAcceptWake = false;
						this._nextAcceptPumpOrigin = "eventWake";
						continue;
					}
					countNetBridgeMetric("acceptWaitsForWake");
					return;
				}
				if (!payload) {
					if (firstPumpRun && !firstPumpResultRecorded) {
						firstPumpResultRecorded = true;
						countNetBridgeMetric("acceptFirstPumpResultEmpty");
					}
					return;
				}
				try {
					const parseStartUs = traceMetrics ? netBridgeNowUs() : 0;
					const accepted = JSON.parse(payload);
					if (traceMetrics) {
						countNetBridgeMetric(
							"acceptJsonParseUs",
							netBridgeNowUs() - parseStartUs,
						);
					}
					countNetBridgeMetric("acceptConnections");
					if (firstPumpRun && !firstPumpResultRecorded) {
						firstPumpResultRecorded = true;
						countNetBridgeMetric("acceptFirstPumpResultConnection");
					}
					const clientHandle = createAcceptedClientHandle(
						accepted.socketId,
						accepted.info,
					);
					const onConnectionStartUs = traceMetrics ? netBridgeNowUs() : 0;
					this._handle.onconnection(null, clientHandle);
					if (traceMetrics) {
						countNetBridgeMetric(
							"acceptOnConnectionUs",
							netBridgeNowUs() - onConnectionStartUs,
						);
					}
				} catch (error) {
					this._emit("error", error);
				}
			}
		} finally {
			this._acceptLoopRunning = false;
			if (this._pendingAcceptWake && this.listening && this._serverId !== 0) {
				wakeNetServerAccept(this);
			}
		}
	}
};

function NetServerCallable(optionsOrListener, maybeListener) {
	return new NetServer(optionsOrListener, maybeListener);
}

var netModule = {
	BlockList,
	Socket: NetSocket,
	SocketAddress,
	Server: NetServerCallable,
	Stream: NetSocket,
	connect: netConnect,
	createConnection: netConnect,
	createServer(optionsOrListener, maybeListener) {
		return new NetServer(optionsOrListener, maybeListener);
	},
	getDefaultAutoSelectFamily() {
		return defaultAutoSelectFamily;
	},
	getDefaultAutoSelectFamilyAttemptTimeout() {
		return defaultAutoSelectFamilyAttemptTimeout;
	},
	isIP(input) {
		return classifyIpAddress(input);
	},
	isIPv4(input) {
		return classifyIpAddress(input) === 4;
	},
	isIPv6(input) {
		return classifyIpAddress(input) === 6;
	},
	setDefaultAutoSelectFamily(value) {
		defaultAutoSelectFamily = value !== false;
	},
	setDefaultAutoSelectFamilyAttemptTimeout(value) {
		const numeric = Number(value);
		if (!Number.isFinite(numeric) || numeric < 0) {
			throw new RangeError(
				`Invalid auto-select family attempt timeout: ${value}`,
			);
		}
		defaultAutoSelectFamilyAttemptTimeout = Math.trunc(numeric);
	},
};

export {
	BlockList,
	buildSerializedTlsOptions,
	classifyIpAddress,
	coerceIpInput,
	countAcceptFirstPumpOrigin,
	countIPv6Parts,
	countNetBridgeMetric,
	countReadFirstPumpOrigin,
	createAcceptedClientHandle,
	createBridgedTlsError,
	createConnectedSocketHandle,
	createFunctionArgTypeError,
	createListenArgValueError,
	createNetBridgeMetrics,
	createSocketBadPortError,
	createTimeoutArgTypeError,
	createTimeoutRangeError,
	defaultAutoSelectFamily,
	defaultAutoSelectFamilyAttemptTimeout,
	deserializeTlsBridgeValue,
	expandIpv6Address,
	finalizeTlsUpgrade,
	formatBlockListRule,
	getRegisteredNetSocket,
	ipAddressToBigInt,
	ipv4ToBigInt,
	ipv6ToBigInt,
	isDecimalIntegerString,
	isIPv4String,
	isIPv6String,
	isNetBridgeMetricsEnabled,
	isNetBridgeTraceEnabled,
	isNetRetainOwnedWriteBufferEnabled,
	isTlsSecureContextWrapper,
	isTruthySocketOption,
	isValidIPv4Segment,
	isValidIPv6Zone,
	isValidTcpPort,
	maxNetBridgeMetric,
	NET_BRIDGE_MAX_RAW_WRITE_BYTES,
	NET_BRIDGE_TIMEOUT_SENTINEL,
	NET_SERVER_HANDLE_PREFIX,
	NET_SOCKET_REGISTRY_PREFIX,
	NetServer,
	NetServerCallable,
	NetSocket,
	netBridgeMetrics,
	netBridgeNowUs,
	netBridgeTraceForced,
	netConnect,
	netModule,
	netSocketDispatch,
	normalizeConnectArgs,
	normalizeIpFamilyLabel,
	normalizeKeepAliveDelay,
	normalizeListenArgs,
	normalizeListenPortValue,
	normalizeNetSocketHandle,
	normalizeSocketTimeout,
	parseNetSocketInfo,
	parseTlsClientHello,
	parseTlsState,
	queryTlsSocket,
	registeredNetServersByPort,
	registeredNetSockets,
	registerNetServer,
	registerNetSocket,
	SocketAddress,
	serializeTlsValue,
	unregisterNetServer,
	unregisterNetSocket,
	wakeNetServerAccept,
	wakeNetServerAcceptForSocket,
	wakePeerBridgeReads,
	wakeSocketBridgeReads,
	yieldBridgeMacrotask,
};
