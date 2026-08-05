"use strict";

setTimeout(function () {
	process.exitCode = 7;
	console.log("async-exit-code-ok");
}, 10);
