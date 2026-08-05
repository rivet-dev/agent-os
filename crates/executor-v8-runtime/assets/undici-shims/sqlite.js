"use strict";

const error = new Error("No such built-in module: node:sqlite");
error.code = "ERR_UNKNOWN_BUILTIN_MODULE";
throw error;
