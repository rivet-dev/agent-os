// Entry module for the V8 bridge. The section bodies live in real ES modules
// under bridge-src/ and are bundled directly by scripts/build-v8-bridge.mjs.

import "./polyfills/index.js";
import "./global-exposure.js";
import "./builtins/readiness.js";
import "./transport.js";
import "./builtins/active-handles.js";
import "./builtins/fs.js";
import "./builtins/os.js";
import "./builtins/child-process.js";
import "./builtins/network.js";
import "./builtins/whatwg-url.js";
import "./builtins/events.js";
import "./builtins/process.js";
import "./builtins/module-loader.js";

import {
	_getActiveHandles,
	_processExitRequested,
	_registerHandle2,
	_unregisterHandle2,
	_waitForActiveHandles,
} from "./builtins/active-handles.js";
import { Buffer3 } from "./builtins/buffer-runtime.js";
import { child_process_exports } from "./builtins/child-process.js";
import { cryptoPolyfill } from "./builtins/crypto.js";
import { fs_default } from "./builtins/fs.js";
import {
	createRequire,
	Module,
	module_default,
	SourceMap,
} from "./builtins/module-loader.js";
import { network_exports } from "./builtins/network.js";
import { os_default } from "./builtins/os.js";
import {
	ProcessExitError,
	process_default,
	setupGlobals,
} from "./builtins/process.js";
import {
	clearImmediate,
	clearInterval,
	clearTimeout2,
	setImmediate,
	setInterval,
	setTimeout2,
} from "./builtins/timers.js";
import { URL2, URLSearchParams } from "./builtins/whatwg-url.js";
import {
	CustomEvent,
	Event,
	EventTarget,
	TextDecoder,
	TextEncoder2,
} from "./polyfills/index.js";
import { __export } from "./vendor/esbuild-runtime.js";

var index_exports = {};
__export(index_exports, {
	Buffer: () => Buffer3,
	CustomEvent: () => CustomEvent,
	Event: () => Event,
	EventTarget: () => EventTarget,
	Module: () => Module,
	ProcessExitError: () => ProcessExitError,
	SourceMap: () => SourceMap,
	TextDecoder: () => TextDecoder,
	TextEncoder: () => TextEncoder2,
	URL: () => URL2,
	URLSearchParams: () => URLSearchParams,
	_getActiveHandles: () => _getActiveHandles,
	_processExitRequested: () => _processExitRequested,
	_registerHandle: () => _registerHandle2,
	_unregisterHandle: () => _unregisterHandle2,
	_waitForActiveHandles: () => _waitForActiveHandles,
	childProcess: () => child_process_exports,
	clearImmediate: () => clearImmediate,
	clearInterval: () => clearInterval,
	clearTimeout: () => clearTimeout2,
	createRequire: () => createRequire,
	cryptoPolyfill: () => cryptoPolyfill,
	default: () => index_default,
	fs: () => fs_default,
	module: () => module_default,
	network: () => network_exports,
	os: () => os_default,
	process: () => process_default,
	setImmediate: () => setImmediate,
	setInterval: () => setInterval,
	setTimeout: () => setTimeout2,
	setupGlobals: () => setupGlobals,
});

var index_default = fs_default;
setupGlobals();
/*! Bundled license information:

ieee754/index.js:
(*! ieee754. BSD-3-Clause License. Feross Aboukhadijeh <https://feross.org/opensource> *)

buffer/index.js:
(*!
 * The buffer module from node.js, for the browser.
 *
 * @author   Feross Aboukhadijeh <https://feross.org>
 * @license  MIT
 *)
*/
