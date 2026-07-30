"use strict";

// readable-stream imports `process/`, which otherwise resolves to the npm
// browser shim. That shim implements nextTick with setTimeout(0), adding a
// timer turn before every stream construction, resume, read, and destroy.
// Delegate dynamically because the AgentOS process global is installed after
// the bundled stream module is initialized.
module.exports = {
	nextTick(callback, ...args) {
		const runtimeProcess = globalThis.process;
		if (
			runtimeProcess &&
			runtimeProcess !== module.exports &&
			typeof runtimeProcess.nextTick === "function"
		) {
			return runtimeProcess.nextTick(callback, ...args);
		}
		return queueMicrotask(() => callback(...args));
	},
	get stdout() {
		return globalThis.process?.stdout;
	},
	get stderr() {
		return globalThis.process?.stderr;
	},
};
