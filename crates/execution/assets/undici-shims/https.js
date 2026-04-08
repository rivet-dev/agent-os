"use strict";

function getHttpsModule() {
	if (!globalThis._httpsModule) {
		throw new Error("node:https bridge module is not available");
	}
	return globalThis._httpsModule;
}

class AgentPlaceholder {}
class ClientRequestPlaceholder {}
class IncomingMessagePlaceholder {}
class ServerPlaceholder {}
class ServerResponsePlaceholder {}

const METHODS = [
	"CHECKOUT",
	"CONNECT",
	"COPY",
	"DELETE",
	"GET",
	"HEAD",
	"LOCK",
	"M-SEARCH",
	"MERGE",
	"MKACTIVITY",
	"MKCOL",
	"MOVE",
	"NOTIFY",
	"OPTIONS",
	"PATCH",
	"POST",
	"PROPFIND",
	"PROPPATCH",
	"PURGE",
	"PUT",
	"REPORT",
	"SEARCH",
	"SUBSCRIBE",
	"TRACE",
	"UNLOCK",
	"UNSUBSCRIBE",
];

module.exports = {
	Agent: AgentPlaceholder,
	ClientRequest: ClientRequestPlaceholder,
	IncomingMessage: IncomingMessagePlaceholder,
	METHODS,
	STATUS_CODES: {},
	Server: ServerPlaceholder,
	ServerResponse: ServerResponsePlaceholder,
	_checkInvalidHeaderChar(value) {
		return getHttpsModule()._checkInvalidHeaderChar(value);
	},
	_checkIsHttpToken(value) {
		return getHttpsModule()._checkIsHttpToken(value);
	},
	createServer(...args) {
		return getHttpsModule().createServer(...args);
	},
	get(...args) {
		return getHttpsModule().get(...args);
	},
	globalAgent: new AgentPlaceholder(),
	maxHeaderSize: 65535,
	request(...args) {
		return getHttpsModule().request(...args);
	},
	validateHeaderName(name, label) {
		return getHttpsModule().validateHeaderName(name, label);
	},
	validateHeaderValue(name, value) {
		return getHttpsModule().validateHeaderValue(name, value);
	},
};
