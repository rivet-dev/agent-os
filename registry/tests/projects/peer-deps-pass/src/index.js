"use strict";

const plugin = require("@peer-test/plugin");

const result = {
	plugin: plugin.pluginName,
	host: plugin.resolvedHost.name,
	hostVersion: plugin.resolvedHost.version
};

console.log(JSON.stringify(result));
