"use strict";

function getHttpModule() {
	if (!globalThis._httpModule) {
		throw new Error("node:http bridge module is not available");
	}
	return globalThis._httpModule;
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
		return getHttpModule()._checkInvalidHeaderChar(value);
	},
	_checkIsHttpToken(value) {
		return getHttpModule()._checkIsHttpToken(value);
	},
	createServer(...args) {
		return getHttpModule().createServer(...args);
	},
	get(...args) {
		return getHttpModule().get(...args);
	},
	globalAgent: new AgentPlaceholder(),
	maxHeaderSize: 65535,
	request(...args) {
		return getHttpModule().request(...args);
	},
	validateHeaderName(name, label) {
		return getHttpModule().validateHeaderName(name, label);
	},
	validateHeaderValue(name, value) {
		return getHttpModule().validateHeaderValue(name, value);
	},
};
