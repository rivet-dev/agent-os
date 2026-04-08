// Binary header IPC framing — custom wire format for all message types.
//
// Wire format per frame:
//   [4B total_len (u32 BE, excludes self)]
//   [1B msg_type]
//   [1B sid_len (N)]
//   [N bytes session_id (UTF-8)]
//   [... type-specific fixed fields ...]
//   [M bytes payload (rest of frame)]
//
// Uses node:v8 serialize/deserialize for payload fields instead of @msgpack/msgpack.
// Existing ipc-client.ts (MessagePack framing) is left unchanged.

import v8 from "node:v8";

// Maximum frame payload: 64 MB (same limit as MessagePack framing).
const MAX_FRAME_SIZE = 64 * 1024 * 1024;

// Host → Rust message type codes
const MSG_AUTHENTICATE = 0x01;
const MSG_CREATE_SESSION = 0x02;
const MSG_DESTROY_SESSION = 0x03;
const MSG_INJECT_GLOBALS = 0x04;
const MSG_EXECUTE = 0x05;
const MSG_BRIDGE_RESPONSE = 0x06;
const MSG_STREAM_EVENT = 0x07;
const MSG_TERMINATE_EXECUTION = 0x08;
const MSG_WARM_SNAPSHOT = 0x09;

// Rust → Host message type codes
const MSG_BRIDGE_CALL = 0x81;
const MSG_EXECUTION_RESULT = 0x82;
const MSG_LOG = 0x83;
const MSG_STREAM_CALLBACK = 0x84;

// ExecutionResult flags
const FLAG_HAS_EXPORTS = 0x01;
const FLAG_HAS_ERROR = 0x02;

/** Structured error in binary format. */
export interface ExecutionErrorBin {
	errorType: string;
	message: string;
	stack: string;
	code: string; // empty string = no code
}

/** A decoded binary frame — discriminated union of all message types. */
export type BinaryFrame =
	// Host → Rust
	| { type: "Authenticate"; token: string }
	| {
			type: "CreateSession";
			sessionId: string;
			heapLimitMb: number;
			cpuTimeLimitMs: number;
	  }
	| { type: "DestroySession"; sessionId: string }
	| { type: "InjectGlobals"; sessionId: string; payload: Buffer }
	| {
			type: "Execute";
			sessionId: string;
			mode: number;
			filePath: string;
			bridgeCode: string;
			postRestoreScript: string;
			userCode: string;
	  }
	| {
			type: "BridgeResponse";
			sessionId: string;
			callId: number;
			status: number;
			payload: Buffer;
	  }
	| {
			type: "StreamEvent";
			sessionId: string;
			eventType: string;
			payload: Buffer;
	  }
	| { type: "TerminateExecution"; sessionId: string }
	| { type: "WarmSnapshot"; bridgeCode: string }
	// Rust → Host
	| {
			type: "BridgeCall";
			sessionId: string;
			callId: number;
			method: string;
			payload: Buffer;
	  }
	| {
			type: "ExecutionResult";
			sessionId: string;
			exitCode: number;
			exports: Buffer | null;
			error: ExecutionErrorBin | null;
	  }
	| { type: "Log"; sessionId: string; channel: number; message: string }
	| {
			type: "StreamCallback";
			sessionId: string;
			callbackType: string;
			payload: Buffer;
	  };

/**
 * Encode a binary frame into a Buffer with 4-byte length prefix.
 * Returns a single Buffer ready to write to the socket.
 */
export function encodeFrame(frame: BinaryFrame): Buffer {
	const body = encodeBody(frame);

	if (body.length > MAX_FRAME_SIZE) {
		throw new Error(
			`Frame size ${body.length} exceeds maximum ${MAX_FRAME_SIZE}`,
		);
	}

	const out = Buffer.alloc(4 + body.length);
	out.writeUInt32BE(body.length, 0);
	body.copy(out, 4);
	return out;
}

/**
 * Decode a binary frame body (without the 4-byte length prefix).
 * The input buffer should contain exactly one frame body.
 */
export function decodeFrame(buf: Buffer): BinaryFrame {
	if (buf.length === 0) {
		throw new Error("Empty frame");
	}

	const msgType = buf[0];
	let pos = 1;

	// Read session_id (all types have sid_len, Authenticate has sid_len=0)
	const sidLen = buf[pos++];
	const sessionId = buf.toString("utf8", pos, pos + sidLen);
	pos += sidLen;

	switch (msgType) {
		case MSG_AUTHENTICATE: {
			const token = buf.toString("utf8", pos);
			return { type: "Authenticate", token };
		}
		case MSG_CREATE_SESSION: {
			const heapLimitMb = buf.readUInt32BE(pos);
			pos += 4;
			const cpuTimeLimitMs = buf.readUInt32BE(pos);
			return { type: "CreateSession", sessionId, heapLimitMb, cpuTimeLimitMs };
		}
		case MSG_DESTROY_SESSION:
			return { type: "DestroySession", sessionId };
		case MSG_INJECT_GLOBALS: {
			const payload = Buffer.from(buf.subarray(pos));
			return { type: "InjectGlobals", sessionId, payload };
		}
		case MSG_EXECUTE: {
			const mode = buf[pos++];
			const fpLen = buf.readUInt16BE(pos);
			pos += 2;
			const filePath = buf.toString("utf8", pos, pos + fpLen);
			pos += fpLen;
			const bcLen = buf.readUInt32BE(pos);
			pos += 4;
			const bridgeCode = buf.toString("utf8", pos, pos + bcLen);
			pos += bcLen;
			const prsLen = buf.readUInt32BE(pos);
			pos += 4;
			const postRestoreScript = buf.toString("utf8", pos, pos + prsLen);
			pos += prsLen;
			const userCode = buf.toString("utf8", pos);
			return {
				type: "Execute",
				sessionId,
				mode,
				filePath,
				bridgeCode,
				postRestoreScript,
				userCode,
			};
		}
		case MSG_BRIDGE_RESPONSE: {
			const callId = Number(buf.readBigUInt64BE(pos));
			pos += 8;
			const status = buf[pos++];
			const payload = Buffer.from(buf.subarray(pos));
			return { type: "BridgeResponse", sessionId, callId, status, payload };
		}
		case MSG_STREAM_EVENT: {
			const etLen = buf.readUInt16BE(pos);
			pos += 2;
			const eventType = buf.toString("utf8", pos, pos + etLen);
			pos += etLen;
			const payload = Buffer.from(buf.subarray(pos));
			return { type: "StreamEvent", sessionId, eventType, payload };
		}
		case MSG_TERMINATE_EXECUTION:
			return { type: "TerminateExecution", sessionId };
		case MSG_WARM_SNAPSHOT: {
			const bcLen = buf.readUInt32BE(pos);
			pos += 4;
			const bridgeCode = buf.toString("utf8", pos, pos + bcLen);
			return { type: "WarmSnapshot", bridgeCode };
		}
		case MSG_BRIDGE_CALL: {
			const callId = Number(buf.readBigUInt64BE(pos));
			pos += 8;
			const mLen = buf.readUInt16BE(pos);
			pos += 2;
			const method = buf.toString("utf8", pos, pos + mLen);
			pos += mLen;
			const payload = Buffer.from(buf.subarray(pos));
			return { type: "BridgeCall", sessionId, callId, method, payload };
		}
		case MSG_EXECUTION_RESULT: {
			const exitCode = buf.readInt32BE(pos);
			pos += 4;
			const flags = buf[pos++];
			let exports: Buffer | null = null;
			if (flags & FLAG_HAS_EXPORTS) {
				const expLen = buf.readUInt32BE(pos);
				pos += 4;
				exports = Buffer.from(buf.subarray(pos, pos + expLen));
				pos += expLen;
			}
			let error: ExecutionErrorBin | null = null;
			if (flags & FLAG_HAS_ERROR) {
				const et = readLenPrefixedU16(buf, pos);
				pos += et.bytesRead;
				const msg = readLenPrefixedU16(buf, pos);
				pos += msg.bytesRead;
				const st = readLenPrefixedU16(buf, pos);
				pos += st.bytesRead;
				const cd = readLenPrefixedU16(buf, pos);
				error = {
					errorType: et.value,
					message: msg.value,
					stack: st.value,
					code: cd.value,
				};
			}
			return { type: "ExecutionResult", sessionId, exitCode, exports, error };
		}
		case MSG_LOG: {
			const channel = buf[pos++];
			const message = buf.toString("utf8", pos);
			return { type: "Log", sessionId, channel, message };
		}
		case MSG_STREAM_CALLBACK: {
			const ctLen = buf.readUInt16BE(pos);
			pos += 2;
			const callbackType = buf.toString("utf8", pos, pos + ctLen);
			pos += ctLen;
			const payload = Buffer.from(buf.subarray(pos));
			return { type: "StreamCallback", sessionId, callbackType, payload };
		}
		default:
			throw new Error(
				`Unknown message type: 0x${msgType.toString(16).padStart(2, "0")}`,
			);
	}
}

/**
 * Extract session_id from raw frame bytes without full deserialization.
 * `raw` starts at the first byte after the 4-byte length prefix (i.e. the msg_type byte).
 * Returns null for Authenticate (which has no session_id).
 */
export function extractSessionId(raw: Buffer): string | null {
	if (raw.length < 2) {
		throw new Error("Frame too short");
	}
	const msgType = raw[0];
	if (msgType === MSG_AUTHENTICATE || msgType === MSG_WARM_SNAPSHOT) {
		return null;
	}
	const sidLen = raw[1];
	if (raw.length < 2 + sidLen) {
		throw new Error("Frame too short for session_id");
	}
	return raw.toString("utf8", 2, 2 + sidLen);
}

// -- Internal encode --

function encodeBody(frame: BinaryFrame): Buffer {
	const parts: Buffer[] = [];

	switch (frame.type) {
		case "Authenticate": {
			parts.push(Buffer.from([MSG_AUTHENTICATE, 0])); // sid_len = 0
			parts.push(Buffer.from(frame.token, "utf8"));
			break;
		}
		case "CreateSession": {
			parts.push(Buffer.from([MSG_CREATE_SESSION]));
			parts.push(encodeSessionId(frame.sessionId));
			const fixed = Buffer.alloc(8);
			fixed.writeUInt32BE(frame.heapLimitMb, 0);
			fixed.writeUInt32BE(frame.cpuTimeLimitMs, 4);
			parts.push(fixed);
			break;
		}
		case "DestroySession": {
			parts.push(Buffer.from([MSG_DESTROY_SESSION]));
			parts.push(encodeSessionId(frame.sessionId));
			break;
		}
		case "InjectGlobals": {
			parts.push(Buffer.from([MSG_INJECT_GLOBALS]));
			parts.push(encodeSessionId(frame.sessionId));
			parts.push(frame.payload);
			break;
		}
		case "Execute": {
			parts.push(Buffer.from([MSG_EXECUTE]));
			parts.push(encodeSessionId(frame.sessionId));
			parts.push(Buffer.from([frame.mode]));
			// file_path (u16 BE length prefix)
			const fpBuf = Buffer.from(frame.filePath, "utf8");
			const fpLen = Buffer.alloc(2);
			fpLen.writeUInt16BE(fpBuf.length, 0);
			parts.push(fpLen);
			parts.push(fpBuf);
			// bridge_code (u32 BE length prefix)
			const bcBuf = Buffer.from(frame.bridgeCode, "utf8");
			const bcLen = Buffer.alloc(4);
			bcLen.writeUInt32BE(bcBuf.length, 0);
			parts.push(bcLen);
			parts.push(bcBuf);
			// post_restore_script (u32 BE length prefix)
			const prsBuf = Buffer.from(frame.postRestoreScript, "utf8");
			const prsLen = Buffer.alloc(4);
			prsLen.writeUInt32BE(prsBuf.length, 0);
			parts.push(prsLen);
			parts.push(prsBuf);
			// user_code (rest of frame)
			parts.push(Buffer.from(frame.userCode, "utf8"));
			break;
		}
		case "BridgeResponse": {
			parts.push(Buffer.from([MSG_BRIDGE_RESPONSE]));
			parts.push(encodeSessionId(frame.sessionId));
			const fixed = Buffer.alloc(9);
			fixed.writeBigUInt64BE(BigInt(frame.callId), 0);
			fixed[8] = frame.status;
			parts.push(fixed);
			parts.push(frame.payload);
			break;
		}
		case "StreamEvent": {
			parts.push(Buffer.from([MSG_STREAM_EVENT]));
			parts.push(encodeSessionId(frame.sessionId));
			const etBuf = Buffer.from(frame.eventType, "utf8");
			const etLen = Buffer.alloc(2);
			etLen.writeUInt16BE(etBuf.length, 0);
			parts.push(etLen);
			parts.push(etBuf);
			parts.push(frame.payload);
			break;
		}
		case "TerminateExecution": {
			parts.push(Buffer.from([MSG_TERMINATE_EXECUTION]));
			parts.push(encodeSessionId(frame.sessionId));
			break;
		}
		case "WarmSnapshot": {
			parts.push(Buffer.from([MSG_WARM_SNAPSHOT, 0])); // no session_id
			const bcBuf = Buffer.from(frame.bridgeCode, "utf8");
			const bcLen = Buffer.alloc(4);
			bcLen.writeUInt32BE(bcBuf.length, 0);
			parts.push(bcLen);
			parts.push(bcBuf);
			break;
		}
		case "BridgeCall": {
			parts.push(Buffer.from([MSG_BRIDGE_CALL]));
			parts.push(encodeSessionId(frame.sessionId));
			const callIdBuf = Buffer.alloc(8);
			callIdBuf.writeBigUInt64BE(BigInt(frame.callId), 0);
			parts.push(callIdBuf);
			const mBuf = Buffer.from(frame.method, "utf8");
			const mLen = Buffer.alloc(2);
			mLen.writeUInt16BE(mBuf.length, 0);
			parts.push(mLen);
			parts.push(mBuf);
			parts.push(frame.payload);
			break;
		}
		case "ExecutionResult": {
			parts.push(Buffer.from([MSG_EXECUTION_RESULT]));
			parts.push(encodeSessionId(frame.sessionId));
			const hdr = Buffer.alloc(5);
			hdr.writeInt32BE(frame.exitCode, 0);
			let flags = 0;
			if (frame.exports !== null) flags |= FLAG_HAS_EXPORTS;
			if (frame.error !== null) flags |= FLAG_HAS_ERROR;
			hdr[4] = flags;
			parts.push(hdr);
			if (frame.exports !== null) {
				const expLen = Buffer.alloc(4);
				expLen.writeUInt32BE(frame.exports.length, 0);
				parts.push(expLen);
				parts.push(frame.exports);
			}
			if (frame.error !== null) {
				parts.push(writeLenPrefixedU16(frame.error.errorType));
				parts.push(writeLenPrefixedU16(frame.error.message));
				parts.push(writeLenPrefixedU16(frame.error.stack));
				parts.push(writeLenPrefixedU16(frame.error.code));
			}
			break;
		}
		case "Log": {
			parts.push(Buffer.from([MSG_LOG]));
			parts.push(encodeSessionId(frame.sessionId));
			parts.push(Buffer.from([frame.channel]));
			parts.push(Buffer.from(frame.message, "utf8"));
			break;
		}
		case "StreamCallback": {
			parts.push(Buffer.from([MSG_STREAM_CALLBACK]));
			parts.push(encodeSessionId(frame.sessionId));
			const ctBuf = Buffer.from(frame.callbackType, "utf8");
			const ctLen = Buffer.alloc(2);
			ctLen.writeUInt16BE(ctBuf.length, 0);
			parts.push(ctLen);
			parts.push(ctBuf);
			parts.push(frame.payload);
			break;
		}
	}

	return Buffer.concat(parts);
}

function encodeSessionId(sid: string): Buffer {
	const bytes = Buffer.from(sid, "utf8");
	if (bytes.length > 255) {
		throw new Error(
			`Session ID byte length ${bytes.length} exceeds maximum 255`,
		);
	}
	const out = Buffer.alloc(1 + bytes.length);
	out[0] = bytes.length;
	bytes.copy(out, 1);
	return out;
}

function writeLenPrefixedU16(s: string): Buffer {
	const bytes = Buffer.from(s, "utf8");
	if (bytes.length > 0xffff) {
		throw new Error(
			`String byte length ${bytes.length} exceeds maximum 65535`,
		);
	}
	const out = Buffer.alloc(2 + bytes.length);
	out.writeUInt16BE(bytes.length, 0);
	bytes.copy(out, 2);
	return out;
}

function readLenPrefixedU16(
	buf: Buffer,
	pos: number,
): { value: string; bytesRead: number } {
	const len = buf.readUInt16BE(pos);
	const value = buf.toString("utf8", pos + 2, pos + 2 + len);
	return { value, bytesRead: 2 + len };
}

// Re-export v8 serialize/deserialize for convenience
export const serializePayload = v8.serialize;
export const deserializePayload = v8.deserialize;
