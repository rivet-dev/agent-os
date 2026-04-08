const SHARED_KEY = "__secureExecMime";

function callMimeBridge(op, ...args) {
	if (typeof _loadPolyfill === "undefined") {
		throw new Error("MIME bridge is not available in sandbox");
	}
	const encoded = `__bd:mimeBridge:${JSON.stringify([op, ...args])}`;
	const response = _loadPolyfill.applySyncPromise(undefined, [encoded]);
	const payload = JSON.parse(response);
	if (payload?.__bd_error) {
		const ctor =
			payload.__bd_error.name === "TypeError"
				? TypeError
				: payload.__bd_error.name === "RangeError"
					? RangeError
					: Error;
		const error = new ctor(payload.__bd_error.message);
		error.code = payload.__bd_error.code;
		error.name = payload.__bd_error.code
			? `${payload.__bd_error.name} [${payload.__bd_error.code}]`
			: payload.__bd_error.name;
		throw error;
	}
	return payload.__bd_result;
}

function createMimeModule() {
	const mimeTypeState = new WeakMap();
	const mimeParamsState = new WeakMap();

	function getMimeTypeState(instance) {
		const state = mimeTypeState.get(instance);
		if (!state) {
			throw new TypeError("Invalid receiver");
		}
		return state;
	}

	function getMimeParamsState(instance) {
		const state = mimeParamsState.get(instance);
		if (!state) {
			throw new TypeError("Invalid receiver");
		}
		return state;
	}

	function applySnapshot(instance, snapshot) {
		const state = getMimeTypeState(instance);
		state.value = snapshot.value;
		state.essence = snapshot.essence;
		state.type = snapshot.type;
		state.subtype = snapshot.subtype;
		const paramsState = getMimeParamsState(state.params);
		paramsState.params = new Map(snapshot.params);
	}

	class MIMEParams {
		constructor() {
			mimeParamsState.set(this, {
				owner: null,
				params: new Map(),
			});
		}

		[Symbol.iterator]() {
			return getMimeParamsState(this).params[Symbol.iterator]();
		}

		get size() {
			return getMimeParamsState(this).params.size;
		}

		has(name) {
			return getMimeParamsState(this).params.has(String(name).toLowerCase());
		}

		get(name) {
			return getMimeParamsState(this).params.get(String(name).toLowerCase()) ?? null;
		}

		set(name, value) {
			const state = getMimeParamsState(this);
			if (!state.owner) {
				state.params.set(String(name).toLowerCase(), String(value));
				return;
			}
			applySnapshot(
				state.owner,
				callMimeBridge(
					"setParam",
					getMimeTypeState(state.owner).value,
					String(name),
					String(value),
				),
			);
		}

		delete(name) {
			const state = getMimeParamsState(this);
			if (!state.owner) {
				state.params.delete(String(name).toLowerCase());
				return;
			}
			applySnapshot(
				state.owner,
				callMimeBridge("deleteParam", getMimeTypeState(state.owner).value, String(name)),
			);
		}

		toString() {
			const state = getMimeParamsState(this);
			if (state.owner) {
				const value = getMimeTypeState(state.owner).value;
				const semicolonIndex = value.indexOf(";");
				return semicolonIndex === -1 ? "" : value.slice(semicolonIndex + 1);
			}
			return "";
		}
	}

	class MIMEType {
		constructor(input) {
			const snapshot = callMimeBridge("parse", String(input));
			const params = new MIMEParams();
			mimeTypeState.set(this, {
				...snapshot,
				params,
			});
			mimeParamsState.set(params, {
				owner: this,
				params: new Map(snapshot.params),
			});
		}

		get essence() {
			return getMimeTypeState(this).essence;
		}

		get type() {
			return getMimeTypeState(this).type;
		}

		set type(value) {
			applySnapshot(this, callMimeBridge("setType", getMimeTypeState(this).value, String(value)));
		}

		get subtype() {
			return getMimeTypeState(this).subtype;
		}

		set subtype(value) {
			applySnapshot(
				this,
				callMimeBridge("setSubtype", getMimeTypeState(this).value, String(value)),
			);
		}

		get params() {
			return getMimeTypeState(this).params;
		}

		toString() {
			return getMimeTypeState(this).value;
		}

		toJSON() {
			return this.toString();
		}
	}

	return {
		MIMEType,
		MIMEParams,
	};
}

if (!globalThis[SHARED_KEY]) {
	globalThis[SHARED_KEY] = createMimeModule();
}

export const MIMEType = globalThis[SHARED_KEY].MIMEType;
export const MIMEParams = globalThis[SHARED_KEY].MIMEParams;
