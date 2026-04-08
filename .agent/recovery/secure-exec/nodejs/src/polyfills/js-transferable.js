const SHARED_KEY = "__secureExecJsTransferable";

function defineHidden(target, key, value) {
	Object.defineProperty(target, key, {
		value,
		configurable: true,
		enumerable: false,
		writable: false,
	});
}

export function getJsTransferState() {
	if (globalThis[SHARED_KEY]) {
		return globalThis[SHARED_KEY];
	}

	const transferModes = new WeakMap();
	const state = {
		kClone: Symbol("kClone"),
		kDeserialize: Symbol("kDeserialize"),
		kTransfer: Symbol("kTransfer"),
		kTransferList: Symbol("kTransferList"),
		markTransferMode(target, cloneable, transferable) {
			if ((typeof target !== "object" && typeof target !== "function") || target === null) {
				return target;
			}
			transferModes.set(target, {
				cloneable: Boolean(cloneable),
				transferable: Boolean(transferable),
			});
			return target;
		},
		getTransferMode(target) {
			return transferModes.get(target) ?? { cloneable: false, transferable: false };
		},
		defineTransferHooks(target, brandCheck) {
			if (!target || target[state.kTransfer]) {
				return;
			}
			defineHidden(target, state.kTransfer, function transfer() {
				if (typeof brandCheck === "function" && !brandCheck(this)) {
					const error = new TypeError("Invalid this");
					error.code = "ERR_INVALID_THIS";
					throw error;
				}
				const error = new Error("Transferable web streams are not supported in sandbox");
				error.code = "ERR_NOT_SUPPORTED";
				throw error;
			});
			defineHidden(target, state.kClone, function clone() {
				const error = new Error("Transferable web streams are not supported in sandbox");
				error.code = "ERR_NOT_SUPPORTED";
				throw error;
			});
			defineHidden(target, state.kDeserialize, function deserialize() {
				const error = new Error("Transferable web streams are not supported in sandbox");
				error.code = "ERR_NOT_SUPPORTED";
				throw error;
			});
			defineHidden(target, state.kTransferList, function transferList() {
				return [];
			});
		},
	};

	globalThis[SHARED_KEY] = state;
	return state;
}
