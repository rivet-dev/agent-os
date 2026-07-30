"use strict";

var Redis = require("ioredis");

var result = {};

// Verify Redis constructor
result.redisExists = typeof Redis === "function";

// Verify key prototype methods
result.instanceMethods = [
	"connect",
	"disconnect",
	"quit",
	"get",
	"set",
	"del",
	"lpush",
	"lrange",
	"subscribe",
	"unsubscribe",
	"publish",
	"pipeline",
	"multi",
].filter(function (m) {
	return typeof Redis.prototype[m] === "function";
});

// Verify Cluster class
result.clusterExists = typeof Redis.Cluster === "function";

// Verify Command class
result.commandExists = typeof Redis.Command === "function";

// Create instance without connecting
var redis = new Redis({
	lazyConnect: true,
	enableReadyCheck: false,
	retryStrategy: function () {
		return null;
	},
});
result.instanceCreated = redis instanceof Redis;
result.hasOptions = typeof redis.options === "object" && redis.options !== null;
result.optionLazyConnect = redis.options.lazyConnect === true;

// Event emitter functionality
result.hasOn = typeof redis.on === "function";
result.hasEmit = typeof redis.emit === "function";

// Pipeline creation (no connection needed)
var pipeline = redis.pipeline();
result.pipelineCreated = pipeline !== null && typeof pipeline === "object";
result.pipelineMethods = ["set", "get", "del", "lpush", "lrange", "exec"].filter(
	function (m) {
		return typeof pipeline[m] === "function";
	},
);

// Multi/transaction creation (no connection needed)
var multi = redis.multi();
result.multiCreated = multi !== null && typeof multi === "object";
result.multiMethods = ["set", "get", "del", "exec"].filter(function (m) {
	return typeof multi[m] === "function";
});

// Verify Command can build commands
var cmd = new Redis.Command("SET", ["key", "value"]);
result.commandBuilt = cmd !== null && typeof cmd === "object";
result.commandName = cmd.name;

redis.disconnect();

console.log(JSON.stringify(result));
