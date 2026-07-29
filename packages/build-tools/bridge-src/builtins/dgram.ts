import { exposeCustomGlobal } from "../global-exposure.js";
import {
	createErrorWithCode,
	createInvalidArgTypeError2,
	createTypeErrorWithCode,
	formatReceivedType,
} from "./http.js";
import {
	countNetBridgeMetric,
	createFunctionArgTypeError,
	createSocketBadPortError,
	isIPv4String,
	isIPv6String,
	isValidTcpPort,
	NET_BRIDGE_TIMEOUT_SENTINEL,
} from "./net.js";
import {
	registerCapabilityReadiness,
	unregisterCapabilityReadiness,
} from "./readiness.js";
import { tlsModule } from "./tls.js";

var DGRAM_HANDLE_PREFIX = "dgram-socket:";

var registeredDgramSocketsByPort = /* @__PURE__ */ new Map();

var registeredDgramSocketsById = /* @__PURE__ */ new Map();

function registerDgramSocket(socket) {
	if (socket?._socketId) {
		registeredDgramSocketsById.set(socket._socketId, socket);
	}
	const port = socket?._localPort;
	if (typeof port !== "number") {
		return;
	}
	let sockets = registeredDgramSocketsByPort.get(port);
	if (!sockets) {
		sockets = /* @__PURE__ */ new Set();
		registeredDgramSocketsByPort.set(port, sockets);
	}
	sockets.add(socket);
	registerCapabilityReadiness(socket, () => wakeDgramSocketReads(socket));
}

function unregisterDgramSocket(socket) {
	unregisterCapabilityReadiness(socket);
	if (
		socket?._socketId &&
		registeredDgramSocketsById.get(socket._socketId) === socket
	) {
		registeredDgramSocketsById.delete(socket._socketId);
	}
	const port = socket?._localPort;
	if (typeof port !== "number") {
		return;
	}
	const sockets = registeredDgramSocketsByPort.get(port);
	if (!sockets) {
		return;
	}
	sockets.delete(socket);
	if (sockets.size === 0) {
		registeredDgramSocketsByPort.delete(port);
	}
}

function isDgramLoopbackAddress(address) {
	return (
		address === "127.0.0.1" || address === "::1" || address === "localhost"
	);
}

function wakeDgramSocketReads(socket) {
	countNetBridgeMetric("dgramWakeAttempts");
	if (!socket || socket._closed || !socket._bound) {
		countNetBridgeMetric("dgramWakeInvalidTargets");
		return;
	}
	if (socket._receiveLoopRunning) {
		countNetBridgeMetric("dgramWakeAlreadyRunning");
		socket._pendingReceiveWake = true;
		return;
	}
	if (socket._pendingReceiveWake) {
		countNetBridgeMetric("dgramWakeAlreadyPending");
		return;
	}
	socket._pendingReceiveWake = true;
	countNetBridgeMetric("dgramEventWakeups");
	queueMicrotask(() => {
		if (
			!socket._closed &&
			socket._bound &&
			socket._pendingReceiveWake &&
			!socket._receiveLoopRunning
		) {
			socket._pendingReceiveWake = false;
			void socket._pumpMessages();
		}
	});
}

function createBadDgramSocketTypeError() {
	return createTypeErrorWithCode(
		"Bad socket type specified. Valid types are: udp4, udp6",
		"ERR_SOCKET_BAD_TYPE",
	);
}

function createDgramAlreadyBoundError() {
	const error = new Error("Socket is already bound");
	error.code = "ERR_SOCKET_ALREADY_BOUND";
	return error;
}

function createDgramAddressError() {
	return new Error("getsockname EBADF");
}

function createDgramArgTypeError(argumentName, expectedType, value) {
	return createTypeErrorWithCode(
		`The "${argumentName}" argument must be of type ${expectedType}. Received ${formatReceivedType(value)}`,
		"ERR_INVALID_ARG_TYPE",
	);
}

function createDgramMissingArgError(argumentName) {
	return createTypeErrorWithCode(
		`The "${argumentName}" argument must be specified`,
		"ERR_MISSING_ARGS",
	);
}

function createDgramNotRunningError() {
	return createErrorWithCode("Not running", "ERR_SOCKET_DGRAM_NOT_RUNNING");
}

function createDgramIsConnectedError() {
	return createErrorWithCode(
		"Already connected",
		"ERR_SOCKET_DGRAM_IS_CONNECTED",
	);
}

function createDgramNotConnectedError() {
	return createErrorWithCode("Not connected", "ERR_SOCKET_DGRAM_NOT_CONNECTED");
}

function getDgramErrno(code) {
	switch (code) {
		case "EBADF":
			return -9;
		case "EINVAL":
			return -22;
		case "EADDRNOTAVAIL":
			return -99;
		case "ENOPROTOOPT":
			return -92;
	}
}

function createDgramSyscallError(syscall, code) {
	const error = new Error(`${syscall} ${code}`);
	error.code = code;
	error.errno = getDgramErrno(code);
	error.syscall = syscall;
	return error;
}

function createDgramTtlArgTypeError(value) {
	return createTypeErrorWithCode(
		`The "ttl" argument must be of type number. Received ${formatReceivedType(value)}`,
		"ERR_INVALID_ARG_TYPE",
	);
}

function createDgramBufferSizeTypeError() {
	return createTypeErrorWithCode(
		"Buffer size must be a positive integer",
		"ERR_SOCKET_BAD_BUFFER_SIZE",
	);
}

function createDgramBufferSizeSystemError(which, code) {
	const syscall = `uv_${which}_buffer_size`;
	const info = {
		errno: code === "EBADF" ? -9 : -22,
		code,
		message: code === "EBADF" ? "bad file descriptor" : "invalid argument",
		syscall,
	};
	const error = new Error(
		`Could not get or set buffer size: ${syscall} returned ${code} (${info.message})`,
	);
	error.name = "SystemError [ERR_SOCKET_BUFFER_SIZE]";
	error.code = "ERR_SOCKET_BUFFER_SIZE";
	error.info = info;
	let errno2 = info.errno;
	let syscallValue = syscall;
	Object.defineProperty(error, "errno", {
		enumerable: true,
		configurable: true,
		get() {
			return errno2;
		},
		set(value) {
			errno2 = value;
		},
	});
	Object.defineProperty(error, "syscall", {
		enumerable: true,
		configurable: true,
		get() {
			return syscallValue;
		},
		set(value) {
			syscallValue = value;
		},
	});
	return error;
}

function getPlatformDgramBufferSize(size) {
	if (size <= 0) {
		return size;
	}
	return process.platform === "linux" ? size * 2 : size;
}

function normalizeDgramTtlValue(value, syscall) {
	if (typeof value !== "number") {
		throw createDgramTtlArgTypeError(value);
	}
	if (!Number.isInteger(value) || value <= 0 || value >= 256) {
		throw createDgramSyscallError(syscall, "EINVAL");
	}
	return value;
}

function isIPv4MulticastAddress(address) {
	if (!isIPv4String(address)) {
		return false;
	}
	const first = Number(address.split(".")[0]);
	return first >= 224 && first <= 239;
}

function isIPv4UnicastAddress(address) {
	return (
		isIPv4String(address) &&
		!isIPv4MulticastAddress(address) &&
		address !== "255.255.255.255"
	);
}

function isIPv6MulticastAddress(address) {
	const zoneIndex = address.indexOf("%");
	const normalized = zoneIndex === -1 ? address : address.slice(0, zoneIndex);
	return isIPv6String(address) && normalized.toLowerCase().startsWith("ff");
}

function validateDgramMulticastAddress(type, syscall, address) {
	if (typeof address !== "string") {
		throw createDgramArgTypeError(
			syscall === "addSourceSpecificMembership" ||
				syscall === "dropSourceSpecificMembership"
				? "groupAddress"
				: "multicastAddress",
			"string",
			address,
		);
	}
	const isValid =
		type === "udp6"
			? isIPv6MulticastAddress(address)
			: isIPv4MulticastAddress(address);
	if (!isValid) {
		throw createDgramSyscallError(syscall, "EINVAL");
	}
	return address;
}

function validateDgramSourceAddress(type, syscall, address) {
	if (typeof address !== "string") {
		throw createDgramArgTypeError("sourceAddress", "string", address);
	}
	const isValid =
		type === "udp6"
			? isIPv6String(address) && !isIPv6MulticastAddress(address)
			: isIPv4UnicastAddress(address);
	if (!isValid) {
		throw createDgramSyscallError(syscall, "EINVAL");
	}
	return address;
}

function normalizeDgramSocketType(value) {
	if (value === "udp4" || value === "udp6") {
		return value;
	}
	throw createBadDgramSocketTypeError();
}

function normalizeDgramSocketOptions(options) {
	if (typeof options === "string") {
		return { type: normalizeDgramSocketType(options) };
	}
	if (!options || typeof options !== "object" || Array.isArray(options)) {
		throw createBadDgramSocketTypeError();
	}
	const typedOptions = options;
	const result = {
		type: normalizeDgramSocketType(typedOptions.type),
	};
	if (typedOptions.recvBufferSize !== void 0) {
		if (typeof typedOptions.recvBufferSize !== "number") {
			throw createInvalidArgTypeError2(
				"options.recvBufferSize",
				"number",
				typedOptions.recvBufferSize,
			);
		}
		result.recvBufferSize = typedOptions.recvBufferSize;
	}
	if (typedOptions.sendBufferSize !== void 0) {
		if (typeof typedOptions.sendBufferSize !== "number") {
			throw createInvalidArgTypeError2(
				"options.sendBufferSize",
				"number",
				typedOptions.sendBufferSize,
			);
		}
		result.sendBufferSize = typedOptions.sendBufferSize;
	}
	return result;
}

function normalizeDgramAddressValue(address, type, defaultAddress) {
	if (address === void 0 || address === null || address === "") {
		return defaultAddress;
	}
	if (typeof address !== "string") {
		throw createDgramArgTypeError("address", "string", address);
	}
	if (address === "localhost") {
		return type === "udp6" ? "::1" : "127.0.0.1";
	}
	return address;
}

function normalizeDgramPortValue(port) {
	if (typeof port !== "number") {
		throw createDgramArgTypeError("port", "number", port);
	}
	if (!isValidTcpPort(port)) {
		throw createSocketBadPortError(port);
	}
	return port;
}

function createDgramMessageBuffer(value) {
	if (typeof value === "string") {
		return Buffer.from(value);
	}
	if (Buffer.isBuffer(value)) {
		return Buffer.from(value);
	}
	if (ArrayBuffer.isView(value)) {
		return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
	}
	throw createDgramArgTypeError(
		"msg",
		"string or Buffer or Uint8Array or DataView",
		value,
	);
}

function createDgramMessageListBuffer(value) {
	if (Array.isArray(value)) {
		return Buffer.concat(value.map((entry) => createDgramMessageBuffer(entry)));
	}
	return createDgramMessageBuffer(value);
}

function normalizeDgramBridgeResult(value) {
	if (typeof value !== "string") {
		return value;
	}
	try {
		return JSON.parse(value);
	} catch {
		return value;
	}
}

function decodeDgramBridgeBytes(value) {
	if (Buffer.isBuffer(value)) {
		return Buffer.from(value);
	}
	if (value instanceof Uint8Array) {
		return Buffer.from(value);
	}
	if (typeof value === "string") {
		return Buffer.from(value, "base64");
	}
	if (value && typeof value === "object") {
		if (value.__type === "Buffer" && typeof value.data === "string") {
			return Buffer.from(value.data, "base64");
		}
		if (value.__agentOSType === "bytes" && typeof value.base64 === "string") {
			return Buffer.from(value.base64, "base64");
		}
	}
	return Buffer.alloc(0);
}

function normalizeDgramBindArgs(args, type) {
	let port: any;
	let address: any;
	let callback: any;
	if (typeof args[0] === "function") {
		callback = args[0];
	} else if (
		args[0] &&
		typeof args[0] === "object" &&
		!Array.isArray(args[0])
	) {
		const options = args[0];
		port = options.port;
		address = options.address;
		callback = args[1];
	} else {
		port = args[0];
		if (typeof args[1] === "function") {
			callback = args[1];
		} else {
			address = args[1];
			callback = args[2];
		}
	}
	if (callback !== void 0 && typeof callback !== "function") {
		throw createFunctionArgTypeError("callback", callback);
	}
	return {
		port: port === void 0 ? 0 : normalizeDgramPortValue(port),
		address: normalizeDgramAddressValue(
			address,
			type,
			type === "udp6" ? "::" : "0.0.0.0",
		),
		callback,
	};
}

function normalizeDgramSendArgs(args, type, connected) {
	if (args.length === 0) {
		throw createDgramArgTypeError(
			"msg",
			"string or Buffer or Uint8Array or DataView",
			void 0,
		);
	}
	const message = args[0];
	if (connected) {
		if (typeof args[1] === "number" && typeof args[2] === "number") {
			const source = createDgramMessageBuffer(message);
			const callback = args[3];
			if (callback !== void 0 && typeof callback !== "function") {
				throw createFunctionArgTypeError("callback", callback);
			}
			return {
				data: Buffer.from(source.subarray(args[1], args[1] + args[2])),
				port: void 0,
				address: void 0,
				callback,
			};
		}
		const callback = args[1];
		if (callback !== void 0 && typeof callback !== "function") {
			throw createDgramIsConnectedError();
		}
		return {
			data: createDgramMessageListBuffer(message),
			port: void 0,
			address: void 0,
			callback,
		};
	}
	const hasOffsetLength =
		typeof args[1] === "number" &&
		typeof args[2] === "number" &&
		args.length >= 4;
	if (hasOffsetLength) {
		const source = createDgramMessageBuffer(message);
		const offset = args[1];
		const length = args[2];
		const callback2 = typeof args[4] === "function" ? args[4] : args[5];
		if (callback2 !== void 0 && typeof callback2 !== "function") {
			throw createFunctionArgTypeError("callback", callback2);
		}
		return {
			data: Buffer.from(source.subarray(offset, offset + length)),
			port: normalizeDgramPortValue(args[3]),
			address: normalizeDgramAddressValue(
				typeof args[4] === "function" ? void 0 : args[4],
				type,
				type === "udp6" ? "::1" : "127.0.0.1",
			),
			callback: callback2,
		};
	}
	const callback = typeof args[2] === "function" ? args[2] : args[3];
	if (callback !== void 0 && typeof callback !== "function") {
		throw createFunctionArgTypeError("callback", callback);
	}
	return {
		data: createDgramMessageListBuffer(message),
		port: normalizeDgramPortValue(args[1]),
		address: normalizeDgramAddressValue(
			typeof args[2] === "function" ? void 0 : args[2],
			type,
			type === "udp6" ? "::1" : "127.0.0.1",
		),
		callback,
	};
}

var DgramSocket = class {
	_type;
	_socketId;
	capabilityId;
	capabilityGeneration;
	_listeners = {};
	_onceListeners = {};
	_bindPromise = null;
	_connectPromise = null;
	_receiveLoopRunning = false;
	_pendingReceiveWake = false;
	_refed = true;
	_closed = false;
	_bound = false;
	_connected = false;
	_handleRefId = null;
	_localAddress;
	_localPort;
	_localFamily;
	_remoteAddress;
	_remotePort;
	_remoteFamily;
	_recvBufferSize;
	_sendBufferSize;
	_memberships = /* @__PURE__ */ new Set();
	_multicastInterface;
	_broadcast = false;
	_multicastLoopback = 1;
	_multicastTtl = 1;
	_ttl = 64;
	constructor(optionsOrType, listener) {
		if (typeof _dgramSocketCreateRaw === "undefined") {
			throw new Error("dgram.createSocket is not supported in sandbox");
		}
		const options = normalizeDgramSocketOptions(optionsOrType);
		this._type = options.type;
		const result = normalizeDgramBridgeResult(
			_dgramSocketCreateRaw.applySync(void 0, [{ type: this._type }]),
		);
		this._socketId = String(result?.socketId ?? result);
		this.capabilityId = result?.capabilityId;
		this.capabilityGeneration = result?.capabilityGeneration;
		if (listener) {
			this.on("message", listener);
		}
		if (options.recvBufferSize !== void 0) {
			this._setBufferSize("recv", options.recvBufferSize, false);
		}
		if (options.sendBufferSize !== void 0) {
			this._setBufferSize("send", options.sendBufferSize, false);
		}
	}
	bind(...args) {
		const { port, address, callback } = normalizeDgramBindArgs(
			args,
			this._type,
		);
		void this._bindInternal(port, address, callback);
		return this;
	}
	send(...args) {
		const { data, port, address, callback } = normalizeDgramSendArgs(
			args,
			this._type,
			this._connected || this._connectPromise !== null,
		);
		void this._sendInternal(data, port, address, callback);
	}
	sendto(...args) {
		this.send(...args);
	}
	connect(port, address, callback) {
		if (typeof address === "function") {
			callback = address;
			address = void 0;
		}
		if (callback !== void 0 && typeof callback !== "function") {
			throw createFunctionArgTypeError("callback", callback);
		}
		if (this._connected || this._connectPromise) {
			throw createDgramIsConnectedError();
		}
		const normalizedPort = normalizeDgramPortValue(port);
		const normalizedAddress = normalizeDgramAddressValue(
			address,
			this._type,
			this._type === "udp6" ? "::1" : "127.0.0.1",
		);
		void this._connectInternal(normalizedPort, normalizedAddress, callback);
		return this;
	}
	disconnect() {
		if (!this._connected || this._closed) {
			throw createDgramNotConnectedError();
		}
		if (typeof _dgramSocketDisconnectRaw === "undefined") {
			throw createDgramNotConnectedError();
		}
		_dgramSocketDisconnectRaw.applySyncPromise(void 0, [this._socketId]);
		this._connected = false;
		this._remoteAddress = void 0;
		this._remotePort = void 0;
		this._remoteFamily = void 0;
		return this;
	}
	remoteAddress() {
		if (!this._connected || this._closed) {
			throw createDgramNotConnectedError();
		}
		if (typeof _dgramSocketRemoteAddressRaw === "undefined") {
			return {
				address: this._remoteAddress,
				port: this._remotePort,
				family: this._remoteFamily,
			};
		}
		return normalizeDgramBridgeResult(
			_dgramSocketRemoteAddressRaw.applySyncPromise(void 0, [this._socketId]),
		);
	}
	address() {
		if (typeof _dgramSocketAddressRaw === "undefined") {
			throw createDgramAddressError();
		}
		try {
			return normalizeDgramBridgeResult(
				_dgramSocketAddressRaw.applySync(void 0, [this._socketId]),
			);
		} catch {
			throw createDgramAddressError();
		}
	}
	close(callback) {
		if (callback !== void 0 && typeof callback !== "function") {
			throw createFunctionArgTypeError("callback", callback);
		}
		if (callback) {
			this.once("close", callback);
		}
		if (this._closed) {
			return this;
		}
		this._closed = true;
		this._connected = false;
		unregisterDgramSocket(this);
		this._bound = false;
		this._clearReceiveWake();
		this._syncHandleRef();
		if (typeof _dgramSocketCloseRaw === "undefined") {
			queueMicrotask(() => {
				this._emit("close");
			});
			return this;
		}
		try {
			_dgramSocketCloseRaw.applySyncPromise(void 0, [this._socketId]);
		} finally {
			queueMicrotask(() => {
				this._emit("close");
			});
		}
		return this;
	}
	ref() {
		this._refed = true;
		this._syncHandleRef();
		if (this._bound && !this._closed && !this._receiveLoopRunning) {
			void this._pumpMessages();
		}
		return this;
	}
	unref() {
		this._refed = false;
		this._syncHandleRef();
		return this;
	}
	setRecvBufferSize(size) {
		this._setBufferSize("recv", size);
	}
	setSendBufferSize(size) {
		this._setBufferSize("send", size);
	}
	getRecvBufferSize() {
		return this._getBufferSize("recv");
	}
	getSendBufferSize() {
		return this._getBufferSize("send");
	}
	setBroadcast(flag) {
		this._ensureBoundForSocketOption("setBroadcast");
		const enabled = Boolean(flag);
		this._setOption("broadcast", { enabled });
		this._broadcast = enabled;
	}
	setTTL(ttl) {
		this._ensureBoundForSocketOption("setTTL");
		const normalized = normalizeDgramTtlValue(ttl, "setTTL");
		this._setOption("ttl", { ttl: normalized });
		this._ttl = normalized;
		return this._ttl;
	}
	setMulticastTTL(ttl) {
		this._ensureBoundForSocketOption("setMulticastTTL");
		const normalized = normalizeDgramTtlValue(ttl, "setMulticastTTL");
		this._setOption("multicastTtl", { ttl: normalized });
		this._multicastTtl = normalized;
		return this._multicastTtl;
	}
	setMulticastLoopback(flag) {
		this._ensureBoundForSocketOption("setMulticastLoopback");
		const enabled = Boolean(flag);
		this._setOption("multicastLoopback", { enabled });
		this._multicastLoopback = Number(enabled);
		return this._multicastLoopback;
	}
	addMembership(multicastAddress, multicastInterface) {
		if (multicastAddress === void 0) {
			throw createDgramMissingArgError("multicastAddress");
		}
		if (this._closed) {
			throw createDgramNotRunningError();
		}
		const groupAddress = validateDgramMulticastAddress(
			this._type,
			"addMembership",
			multicastAddress,
		);
		if (
			multicastInterface !== void 0 &&
			typeof multicastInterface !== "string"
		) {
			throw createDgramArgTypeError(
				"multicastInterface",
				"string",
				multicastInterface,
			);
		}
		this._setOption("membership", {
			group: groupAddress,
			interface: multicastInterface,
			join: true,
		});
		this._memberships.add(`${groupAddress}|${multicastInterface ?? ""}`);
	}
	dropMembership(multicastAddress, multicastInterface) {
		if (multicastAddress === void 0) {
			throw createDgramMissingArgError("multicastAddress");
		}
		if (this._closed) {
			throw createDgramNotRunningError();
		}
		if (!this._bound) {
			throw createDgramNotRunningError();
		}
		const groupAddress = validateDgramMulticastAddress(
			this._type,
			"dropMembership",
			multicastAddress,
		);
		if (
			multicastInterface !== void 0 &&
			typeof multicastInterface !== "string"
		) {
			throw createDgramArgTypeError(
				"multicastInterface",
				"string",
				multicastInterface,
			);
		}
		const membershipKey = `${groupAddress}|${multicastInterface ?? ""}`;
		this._setOption("membership", {
			group: groupAddress,
			interface: multicastInterface,
			join: false,
		});
		this._memberships.delete(membershipKey);
	}
	addSourceSpecificMembership(sourceAddress, groupAddress, multicastInterface) {
		if (this._closed) {
			throw createDgramNotRunningError();
		}
		if (typeof sourceAddress !== "string") {
			throw createDgramArgTypeError("sourceAddress", "string", sourceAddress);
		}
		if (typeof groupAddress !== "string") {
			throw createDgramArgTypeError("groupAddress", "string", groupAddress);
		}
		const validatedSource = validateDgramSourceAddress(
			this._type,
			"addSourceSpecificMembership",
			sourceAddress,
		);
		const validatedGroup = validateDgramMulticastAddress(
			this._type,
			"addSourceSpecificMembership",
			groupAddress,
		);
		if (
			multicastInterface !== void 0 &&
			typeof multicastInterface !== "string"
		) {
			throw createDgramArgTypeError(
				"multicastInterface",
				"string",
				multicastInterface,
			);
		}
		this._setOption("sourceMembership", {
			source: validatedSource,
			group: validatedGroup,
			interface: multicastInterface,
			join: true,
		});
		this._memberships.add(
			`${validatedSource}>${validatedGroup}|${multicastInterface ?? ""}`,
		);
	}
	dropSourceSpecificMembership(
		sourceAddress,
		groupAddress,
		multicastInterface,
	) {
		if (this._closed) {
			throw createDgramNotRunningError();
		}
		if (!this._bound) {
			throw createDgramNotRunningError();
		}
		if (typeof sourceAddress !== "string") {
			throw createDgramArgTypeError("sourceAddress", "string", sourceAddress);
		}
		if (typeof groupAddress !== "string") {
			throw createDgramArgTypeError("groupAddress", "string", groupAddress);
		}
		const validatedSource = validateDgramSourceAddress(
			this._type,
			"dropSourceSpecificMembership",
			sourceAddress,
		);
		const validatedGroup = validateDgramMulticastAddress(
			this._type,
			"dropSourceSpecificMembership",
			groupAddress,
		);
		if (
			multicastInterface !== void 0 &&
			typeof multicastInterface !== "string"
		) {
			throw createDgramArgTypeError(
				"multicastInterface",
				"string",
				multicastInterface,
			);
		}
		const membershipKey = `${validatedSource}>${validatedGroup}|${multicastInterface ?? ""}`;
		this._setOption("sourceMembership", {
			source: validatedSource,
			group: validatedGroup,
			interface: multicastInterface,
			join: false,
		});
		this._memberships.delete(membershipKey);
	}
	setMulticastInterface(interfaceAddress) {
		if (typeof interfaceAddress !== "string") {
			throw createDgramArgTypeError(
				"interfaceAddress",
				"string",
				interfaceAddress,
			);
		}
		if (this._closed) {
			throw createDgramNotRunningError();
		}
		this._ensureBoundForSocketOption("setMulticastInterface");
		if (this._type === "udp4") {
			if (interfaceAddress === "0.0.0.0") {
				this._setOption("multicastInterface", {
					interface: interfaceAddress,
				});
				this._multicastInterface = interfaceAddress;
				return;
			}
			if (!isIPv4String(interfaceAddress)) {
				throw createDgramSyscallError("setMulticastInterface", "ENOPROTOOPT");
			}
			if (!isIPv4UnicastAddress(interfaceAddress)) {
				throw createDgramSyscallError("setMulticastInterface", "EADDRNOTAVAIL");
			}
			this._setOption("multicastInterface", {
				interface: interfaceAddress,
			});
			this._multicastInterface = interfaceAddress;
			return;
		}
		if (
			interfaceAddress === "" ||
			interfaceAddress === "undefined" ||
			!isIPv6String(interfaceAddress)
		) {
			throw createDgramSyscallError("setMulticastInterface", "EINVAL");
		}
		this._setOption("multicastInterface", { interface: interfaceAddress });
		this._multicastInterface = interfaceAddress;
	}
	on(event, listener) {
		if (!this._listeners[event]) this._listeners[event] = [];
		this._listeners[event].push(listener);
		return this;
	}
	addListener(event, listener) {
		return this.on(event, listener);
	}
	once(event, listener) {
		if (!this._onceListeners[event]) this._onceListeners[event] = [];
		this._onceListeners[event].push(listener);
		return this;
	}
	removeListener(event, listener) {
		const listeners = this._listeners[event];
		if (listeners) {
			const index = listeners.indexOf(listener);
			if (index >= 0) listeners.splice(index, 1);
		}
		const onceListeners = this._onceListeners[event];
		if (onceListeners) {
			const index = onceListeners.indexOf(listener);
			if (index >= 0) onceListeners.splice(index, 1);
		}
		return this;
	}
	off(event, listener) {
		return this.removeListener(event, listener);
	}
	emit(event, ...args) {
		return this._emit(event, ...args);
	}
	async _bindInternal(port, address, callback) {
		if (this._closed) {
			return;
		}
		if (this._bound || this._bindPromise) {
			throw createDgramAlreadyBoundError();
		}
		if (typeof _dgramSocketBindRaw === "undefined") {
			throw new Error("dgram.bind is not supported in sandbox");
		}
		this._bindPromise = (async () => {
			try {
				const result = normalizeDgramBridgeResult(
					_dgramSocketBindRaw.applySyncPromise(void 0, [
						this._socketId,
						{ port, address },
					]),
				);
				this._applyBoundAddress(result);
				this._bound = true;
				registerDgramSocket(this);
				this._applyInitialBufferSizes();
				this._syncHandleRef();
				queueMicrotask(() => {
					if (this._closed) {
						return;
					}
					this._emit("listening");
					callback?.call(this);
					void this._pumpMessages();
				});
			} catch (error) {
				queueMicrotask(() => {
					this._emit("error", error);
				});
				throw error;
			} finally {
				this._bindPromise = null;
			}
		})();
		return this._bindPromise;
	}
	async _connectInternal(port, address, callback) {
		if (this._closed) {
			return;
		}
		if (typeof _dgramSocketConnectRaw === "undefined") {
			throw new Error("dgram.connect is not supported in sandbox");
		}
		this._connectPromise = (async () => {
			try {
				await this._ensureBound();
				const result = normalizeDgramBridgeResult(
					_dgramSocketConnectRaw.applySyncPromise(void 0, [
						this._socketId,
						{ port, address },
					]),
				);
				this._applyBoundAddress(result);
				this._remoteAddress = result?.remoteAddress ?? address;
				this._remotePort = result?.remotePort ?? port;
				this._remoteFamily =
					result?.remoteFamily ?? socketFamilyForAddress(this._remoteAddress);
				this._connected = true;
				queueMicrotask(() => {
					if (this._closed || !this._connected) {
						return;
					}
					this._emit("connect");
					callback?.call(this);
				});
			} catch (error) {
				queueMicrotask(() => {
					this._emit("error", error);
				});
				throw error;
			} finally {
				this._connectPromise = null;
			}
		})();
		return this._connectPromise;
	}
	async _ensureBound() {
		if (this._bound) {
			return;
		}
		if (this._bindPromise) {
			await this._bindPromise;
			return;
		}
		await this._bindInternal(0, this._type === "udp6" ? "::" : "0.0.0.0");
	}
	async _sendInternal(data, port, address, callback) {
		try {
			if (this._connectPromise) {
				await this._connectPromise;
			} else {
				await this._ensureBound();
			}
			if (this._closed || typeof _dgramSocketSendRaw === "undefined") {
				return;
			}
			const result = normalizeDgramBridgeResult(
				_dgramSocketSendRaw.applySyncPromise(void 0, [
					this._socketId,
					data,
					{ port, address },
				]),
			);
			this._applyBoundAddress(result);
			if (
				typeof port === "number" &&
				typeof address === "string" &&
				isDgramLoopbackAddress(address)
			) {
				const sockets = registeredDgramSocketsByPort.get(port);
				if (sockets) {
					countNetBridgeMetric("dgramWakeLoopbackHits");
					for (const socket of sockets) {
						wakeDgramSocketReads(socket);
					}
				} else {
					countNetBridgeMetric("dgramWakeLoopbackMisses");
				}
			}
			if (callback) {
				queueMicrotask(() => {
					callback(
						null,
						typeof result?.bytes === "number" ? result.bytes : data.length,
					);
				});
			}
		} catch (error) {
			if (callback) {
				queueMicrotask(() => {
					callback(error);
				});
				return;
			}
			queueMicrotask(() => {
				this._emit("error", error);
			});
		}
	}
	async _pumpMessages() {
		if (this._receiveLoopRunning || this._closed || !this._bound) {
			return;
		}
		if (typeof _dgramSocketRecvRaw === "undefined") {
			return;
		}
		this._receiveLoopRunning = true;
		try {
			while (!this._closed && this._bound) {
				const payload = normalizeDgramBridgeResult(
					_dgramSocketRecvRaw.applySync(void 0, [this._socketId, 0]),
				);
				if (payload === NET_BRIDGE_TIMEOUT_SENTINEL || !payload) {
					if (this._pendingReceiveWake) {
						this._pendingReceiveWake = false;
						continue;
					}
					return;
				}
				if (payload.type === "message") {
					const message = decodeDgramBridgeBytes(payload.data);
					this._emit("message", message, {
						address: payload.remoteAddress,
						family:
							payload.remoteFamily ??
							socketFamilyForAddress(payload.remoteAddress),
						port: payload.remotePort,
						size: message.length,
					});
					continue;
				}
				if (payload.type === "error") {
					const error = new Error(
						typeof payload.message === "string"
							? payload.message
							: "agentos dgram socket error",
					);
					if (typeof payload.code === "string" && payload.code.length > 0) {
						error.code = payload.code;
					}
					this._emit("error", error);
				}
			}
		} catch (error) {
			this._emit("error", error);
		} finally {
			this._receiveLoopRunning = false;
			if (this._pendingReceiveWake && !this._closed && this._bound) {
				this._pendingReceiveWake = false;
				queueMicrotask(() => {
					if (!this._closed && this._bound) {
						void this._pumpMessages();
					}
				});
			}
		}
	}
	_applyBoundAddress(info) {
		if (!info || typeof info !== "object") {
			return;
		}
		const port =
			typeof info.localPort === "number" ? info.localPort : info.port;
		if (typeof port === "number") {
			this._localPort = port;
		}
		const address =
			typeof info.localAddress === "string" ? info.localAddress : info.address;
		if (typeof address === "string") {
			this._localAddress = address;
		}
		const family =
			typeof info.localFamily === "string" ? info.localFamily : info.family;
		if (typeof family === "string") {
			this._localFamily = family;
		}
	}
	_setOption(name, payload) {
		if (typeof _dgramSocketSetOptionRaw === "undefined") {
			throw createDgramSyscallError(name, "ENOPROTOOPT");
		}
		const result = normalizeDgramBridgeResult(
			_dgramSocketSetOptionRaw.applySyncPromise(void 0, [
				this._socketId,
				name,
				payload,
			]),
		);
		if (!this._bound && typeof result?.localPort === "number") {
			this._applyBoundAddress(result);
			this._bound = true;
			registerDgramSocket(this);
			this._applyInitialBufferSizes();
			this._syncHandleRef();
			queueMicrotask(() => {
				if (this._closed || !this._bound) {
					return;
				}
				this._emit("listening");
				void this._pumpMessages();
			});
		}
		return result?.value ?? result;
	}
	_clearReceiveWake() {
		this._pendingReceiveWake = false;
	}
	_ensureBoundForSocketOption(syscall) {
		if (!this._bound || this._closed) {
			throw createDgramSyscallError(syscall, "EBADF");
		}
	}
	_setBufferSize(which, size, requireRunning = true) {
		if (!Number.isInteger(size) || size <= 0 || !Number.isFinite(size)) {
			throw createDgramBufferSizeTypeError();
		}
		if (size > 2147483647) {
			throw createDgramBufferSizeSystemError(which, "EINVAL");
		}
		if (requireRunning && (!this._bound || this._closed)) {
			throw createDgramBufferSizeSystemError(which, "EBADF");
		}
		if (
			typeof _dgramSocketSetBufferSizeRaw !== "undefined" &&
			this._bound &&
			!this._closed
		) {
			_dgramSocketSetBufferSizeRaw.applySyncPromise(void 0, [
				this._socketId,
				which,
				size,
			]);
		}
		if (which === "recv") {
			this._recvBufferSize = size;
			return;
		}
		this._sendBufferSize = size;
	}
	_getBufferSize(which) {
		if (!this._bound || this._closed) {
			throw createDgramBufferSizeSystemError(which, "EBADF");
		}
		const fallback =
			which === "recv"
				? (this._recvBufferSize ?? 0)
				: (this._sendBufferSize ?? 0);
		if (typeof _dgramSocketGetBufferSizeRaw === "undefined") {
			return getPlatformDgramBufferSize(fallback);
		}
		const rawSize = _dgramSocketGetBufferSizeRaw.applySyncPromise(void 0, [
			this._socketId,
			which,
		]);
		return getPlatformDgramBufferSize(rawSize > 0 ? rawSize : fallback);
	}
	_applyInitialBufferSizes() {
		if (this._recvBufferSize !== void 0) {
			this._setBufferSize("recv", this._recvBufferSize);
		}
		if (this._sendBufferSize !== void 0) {
			this._setBufferSize("send", this._sendBufferSize);
		}
	}
	_syncHandleRef() {
		if (!this._bound || this._closed || !this._refed) {
			if (this._handleRefId && typeof _unregisterHandle === "function") {
				_unregisterHandle(this._handleRefId);
			}
			this._handleRefId = null;
			return;
		}
		const nextHandleId = `${DGRAM_HANDLE_PREFIX}${this._socketId}`;
		if (this._handleRefId === nextHandleId) {
			return;
		}
		if (this._handleRefId && typeof _unregisterHandle === "function") {
			_unregisterHandle(this._handleRefId);
		}
		this._handleRefId = nextHandleId;
		if (typeof _registerHandle === "function") {
			_registerHandle(this._handleRefId, "dgram socket");
		}
	}
	_emit(event, ...args) {
		let handled = false;
		const listeners = this._listeners[event];
		if (listeners) {
			for (const listener of [...listeners]) {
				listener(...args);
				handled = true;
			}
		}
		const onceListeners = this._onceListeners[event];
		if (onceListeners) {
			this._onceListeners[event] = [];
			for (const listener of [...onceListeners]) {
				listener(...args);
				handled = true;
			}
		}
		return handled;
	}
};

var dgramModule = {
	Socket: DgramSocket,
	createSocket(optionsOrType, callback) {
		return new DgramSocket(optionsOrType, callback);
	},
};

exposeCustomGlobal("_tlsModule", tlsModule);
exposeCustomGlobal("_dgramSocketDispatch", (payload) => {
	const socket = registeredDgramSocketsById.get(payload?.socketId);
	if (socket) {
		wakeDgramSocketReads(socket);
	}
});

export {
	createBadDgramSocketTypeError,
	createDgramAddressError,
	createDgramAlreadyBoundError,
	createDgramArgTypeError,
	createDgramBufferSizeSystemError,
	createDgramBufferSizeTypeError,
	createDgramMessageBuffer,
	createDgramMessageListBuffer,
	createDgramMissingArgError,
	createDgramNotRunningError,
	createDgramSyscallError,
	createDgramTtlArgTypeError,
	DGRAM_HANDLE_PREFIX,
	DgramSocket,
	decodeDgramBridgeBytes,
	dgramModule,
	getDgramErrno,
	getPlatformDgramBufferSize,
	isIPv4MulticastAddress,
	isIPv4UnicastAddress,
	isIPv6MulticastAddress,
	normalizeDgramAddressValue,
	normalizeDgramBindArgs,
	normalizeDgramBridgeResult,
	normalizeDgramPortValue,
	normalizeDgramSendArgs,
	normalizeDgramSocketOptions,
	normalizeDgramSocketType,
	normalizeDgramTtlValue,
	validateDgramMulticastAddress,
	validateDgramSourceAddress,
};
