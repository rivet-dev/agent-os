// Bridge module entry point
// This file is compiled to a single JS bundle that gets injected into the isolate
//
// Each module provides polyfills for Node.js built-in modules that need to
// communicate with the host environment via bridge functions.

// IMPORTANT: Import polyfills FIRST before any other modules!
// Some packages (like whatwg-url) use TextEncoder/TextDecoder at module load time.
// This import installs them on globalThis before other imports execute.
import "./polyfills.js";

// Active handles mechanism - must be imported early so other modules can use it.
// See: docs-internal/node/ACTIVE_HANDLES.md
import {
	_registerHandle,
	_unregisterHandle,
	_waitForActiveHandles,
	_getActiveHandles,
} from "./active-handles.js";

// File system module
import fs from "./fs.js";

// Operating system module
import os from "./os.js";

// Child process module
import * as childProcess from "./child-process.js";

// Network modules (fetch, dns, http, https)
import * as network from "./network.js";

// Process and global polyfills
import process, {
  setupGlobals,
  setTimeout,
  clearTimeout,
  setInterval,
  clearInterval,
  setImmediate,
  clearImmediate,
  URL,
  URLSearchParams,
  TextEncoder,
  TextDecoder,
  Event,
  CustomEvent,
  EventTarget,
  Buffer,
  cryptoPolyfill,
  ProcessExitError,
} from "./process.js";

// Module system (createRequire, Module class)
import moduleModule, { createRequire, Module, SourceMap } from "./module.js";

// Export all modules
export {
  // Core modules
  fs,
  os,
  childProcess,
  process,
  moduleModule as module,

  // Network
  network,

  // Process globals
  setupGlobals,
  setTimeout,
  clearTimeout,
  setInterval,
  clearInterval,
  setImmediate,
  clearImmediate,
  URL,
  URLSearchParams,
  TextEncoder,
  TextDecoder,
  Event,
  CustomEvent,
  EventTarget,
  Buffer,
  cryptoPolyfill,
  ProcessExitError,

  // Module utilities
  createRequire,
  Module,
  SourceMap,

  // Active handles (see docs-internal/node/ACTIVE_HANDLES.md)
  _registerHandle,
  _unregisterHandle,
  _waitForActiveHandles,
  _getActiveHandles,
};

// Default export is fs for backward compatibility
export default fs;

// Auto-setup globals when bridge loads
// This installs process, timers, URL, Buffer, crypto, etc. on globalThis
setupGlobals();
