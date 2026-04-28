"use strict";

class AsyncLocalStorage {
	constructor() {
		this._enabled = false;
		this._store = undefined;
	}

	disable() {
		this._enabled = false;
		this._store = undefined;
	}

	enterWith(store) {
		this._enabled = true;
		this._store = store;
	}

	exit(callback, ...args) {
		const previousEnabled = this._enabled;
		const previousStore = this._store;
		this._enabled = false;
		this._store = undefined;
		try {
			return callback(...args);
		} finally {
			this._enabled = previousEnabled;
			this._store = previousStore;
		}
	}

	getStore() {
		return this._enabled ? this._store : undefined;
	}

	run(store, callback, ...args) {
		const previousEnabled = this._enabled;
		const previousStore = this._store;
		this._enabled = true;
		this._store = store;
		try {
			return callback(...args);
		} finally {
			this._enabled = previousEnabled;
			this._store = previousStore;
		}
	}
}

class AsyncResource {
	constructor(type = "AgentOsAsyncResource") {
		this.type = type;
	}

	bind(fn, thisArg = undefined) {
		if (typeof fn !== "function") {
			return fn;
		}
		return (...args) => this.runInAsyncScope(fn, thisArg ?? this, ...args);
	}

	emitDestroy() {}

	runInAsyncScope(fn, thisArg, ...args) {
		return fn.apply(thisArg, args);
	}
}

function createHook() {
	return {
		enable() {
			return this;
		},
		disable() {
			return this;
		},
	};
}

function executionAsyncId() {
	return 0;
}

function triggerAsyncId() {
	return 0;
}

module.exports = {
	AsyncLocalStorage,
	AsyncResource,
	createHook,
	executionAsyncId,
	triggerAsyncId,
};
