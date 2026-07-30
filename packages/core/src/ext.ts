import { toExactArrayBuffer } from "./bytes.js";
import type * as sidecarProtocol from "./generated-protocol.js";

export interface LiveExtEnvelope {
	namespace: string;
	payload: Uint8Array;
}

export function toGeneratedExtEnvelope(
	envelope: LiveExtEnvelope,
): sidecarProtocol.ExtEnvelope {
	return {
		namespace: envelope.namespace,
		payload: toExactArrayBuffer(envelope.payload),
	};
}

export function fromGeneratedExtEnvelope(
	envelope: sidecarProtocol.ExtEnvelope,
): LiveExtEnvelope {
	return {
		namespace: envelope.namespace,
		payload: Buffer.from(envelope.payload),
	};
}
