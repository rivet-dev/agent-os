const SHARED_KEY = "__secureExecUtilTypes";

function createUtilTypesState() {
	return {
		isPromise(value) {
			return value instanceof Promise;
		},
	};
}

const state = globalThis[SHARED_KEY] ?? createUtilTypesState();
globalThis[SHARED_KEY] = state;

export const isPromise = state.isPromise;
export default state;
