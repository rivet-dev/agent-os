// Compatibility shim for bridge calling conventions.
//
// The bridge bundle code calls host functions via ivm.Reference methods:
//   .applySync(ctx, args)       — sync call
//   .applySyncPromise(ctx, args) — sync call (host may be async)
//   .apply(ctx, args, opts)     — async call (opts ignored, Function.prototype.apply works)
//
// The Rust V8 runtime registers host functions as plain FunctionTemplate functions.
// This shim adds the missing .applySync() and .applySyncPromise() methods.
// .apply() already works via Function.prototype.apply (third arg is ignored).

import {
	HOST_BRIDGE_GLOBAL_KEY_LIST,
} from "./bridge-contract.js";

/**
 * Generate JS source for the ivm-compat shim.
 *
 * Must run AFTER the Rust side registers bridge functions on the global,
 * and BEFORE the bridge bundle IIFE executes.
 */
export function getIvmCompatShimSource(): string {
	const keyListJson = JSON.stringify(
		HOST_BRIDGE_GLOBAL_KEY_LIST.filter(
			// _processConfig and _osConfig are config objects, not callable functions
			(k) => k !== "_processConfig" && k !== "_osConfig",
		),
	);

	return `(function(){
  var keys = ${keyListJson};
  for (var i = 0; i < keys.length; i++) {
    var fn = globalThis[keys[i]];
    if (typeof fn !== 'function') continue;
    fn.applySync = function(ctx, args) { return this.call(null, ...(args || [])); };
    fn.applySyncPromise = function(ctx, args) { return this.call(null, ...(args || [])); };
  }
})();`;
}
