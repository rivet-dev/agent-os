"use strict";

var mysql = require("mysql2");
var mysqlPromise = require("mysql2/promise");

var result = {};

// Core factory functions
result.createConnectionExists = typeof mysql.createConnection === "function";
result.createPoolExists = typeof mysql.createPool === "function";
result.createPoolClusterExists = typeof mysql.createPoolCluster === "function";

// Protocol types and charsets
var Types = mysql.Types;
result.typesExists = typeof Types === "object" && Types !== null;
result.hasCharsets = typeof mysql.Charsets === "object" && mysql.Charsets !== null;

// Escape and format utilities — comprehensive coverage
result.escapeString = mysql.escape("hello 'world'");
result.escapeNumber = mysql.escape(42);
result.escapeNull = mysql.escape(null);
result.escapeBool = mysql.escape(true);
result.escapeArray = mysql.escape([1, "two", null]);
result.escapeNested = mysql.escape([[1, 2], [3, 4]]);
result.escapeId = mysql.escapeId("table name");
result.escapeIdQualified = mysql.escapeId("db.table");
result.formatSql = mysql.format("SELECT ? FROM ??", ["value", "table"]);
result.formatMulti = mysql.format("INSERT INTO ?? SET ?", [
	"users",
	{ name: "test", age: 30 },
]);

// raw() for prepared statement placeholders
result.hasRaw = typeof mysql.raw === "function";
var rawVal = mysql.raw("NOW()");
result.rawEscape = mysql.escape(rawVal);

// Connection pool configuration (no connection needed — exercises config parsing)
var pool = mysql.createPool({
	host: "127.0.0.1",
	port: 0,
	user: "root",
	password: "test",
	database: "testdb",
	waitForConnections: true,
	connectionLimit: 5,
	queueLimit: 0,
	enableKeepAlive: true,
	keepAliveInitialDelay: 10000,
});
result.poolCreated = pool !== null && typeof pool === "object";
result.poolMethods = [
	"getConnection",
	"query",
	"execute",
	"end",
	"on",
	"promise",
].filter(function (m) {
	return typeof pool[m] === "function";
});

// Pool event emitter interface
result.poolHasOn = typeof pool.on === "function";
result.poolHasEmit = typeof pool.emit === "function";

// Pool cluster configuration
var cluster = mysql.createPoolCluster({
	canRetry: true,
	removeNodeErrorCount: 5,
	defaultSelector: "RR",
});
result.clusterCreated = cluster !== null && typeof cluster === "object";
result.clusterMethods = ["add", "remove", "getConnection", "of", "end", "on"].filter(
	function (m) {
		return typeof cluster[m] === "function";
	},
);

// Add nodes to cluster (exercises config validation — no connections made)
cluster.add("MASTER", {
	host: "127.0.0.1",
	port: 0,
	user: "root",
	password: "test",
	database: "testdb",
});
cluster.add("REPLICA1", {
	host: "127.0.0.1",
	port: 0,
	user: "root",
	password: "test",
	database: "testdb",
});

// Cluster pattern selector
var clusterOf = cluster.of("REPLICA*");
result.clusterOfCreated = clusterOf !== null && typeof clusterOf === "object";

// Promise wrapper — deeper coverage
result.promiseCreateConnection = typeof mysqlPromise.createConnection === "function";
result.promiseCreatePool = typeof mysqlPromise.createPool === "function";
result.promiseCreatePoolCluster =
	typeof mysqlPromise.createPoolCluster === "function";

// Promise pool with same config shape
var promisePool = mysqlPromise.createPool({
	host: "127.0.0.1",
	port: 0,
	user: "root",
	password: "test",
	database: "testdb",
	connectionLimit: 2,
});
result.promisePoolCreated = promisePool !== null && typeof promisePool === "object";
result.promisePoolMethods = ["getConnection", "query", "execute", "end"].filter(
	function (m) {
		return typeof promisePool[m] === "function";
	},
);

// Type casting and field metadata
result.typeNames = [
	"DECIMAL",
	"TINY",
	"SHORT",
	"LONG",
	"FLOAT",
	"DOUBLE",
	"TIMESTAMP",
	"LONGLONG",
	"INT24",
	"DATE",
	"TIME",
	"DATETIME",
	"YEAR",
	"NEWDATE",
	"VARCHAR",
	"BIT",
	"JSON",
	"NEWDECIMAL",
	"ENUM",
	"SET",
	"TINY_BLOB",
	"MEDIUM_BLOB",
	"LONG_BLOB",
	"BLOB",
	"VAR_STRING",
	"STRING",
	"GEOMETRY",
].filter(function (t) {
	return typeof Types[t] === "number";
});

// Format with Date objects (use epoch 0 for timezone-stable output)
var d = new Date(0);
result.formatDateType = typeof mysql.format("SELECT ?", [d]);

// Format with Buffer
result.formatBuffer = mysql.format("SELECT ?", [Buffer.from("binary")]);

// Format with nested object (SET clause)
result.formatObject = mysql.format("UPDATE ?? SET ?", [
	"tbl",
	{ name: "test", active: true, score: null },
]);

// Clean up pools (no connections to close — releases internal timers)
pool.end(function () {});
cluster.end(function () {});
promisePool.end().catch(function () {});

console.log(JSON.stringify(result));
