const notImplemented = (name) => {
	throw new Error(`undici.${name} is not available in the browser VM`);
};

const optionalGlobal = (name) => {
	try {
		return globalThis[name];
	} catch {
		return undefined;
	}
};

class NoopDispatcher {
	dispatch() {
		notImplemented("dispatcher.dispatch");
	}

	close() {
		return Promise.resolve();
	}

	destroy() {}
}

class NoopAgent extends NoopDispatcher {}

let globalDispatcher = new NoopAgent();

module.exports = {
	fetch: (...args) => globalThis.fetch(...args),
	Headers: globalThis.Headers,
	Request: globalThis.Request,
	Response: globalThis.Response,
	FormData: globalThis.FormData,
	WebSocket: optionalGlobal("WebSocket"),
	CloseEvent: optionalGlobal("CloseEvent"),
	ErrorEvent: optionalGlobal("ErrorEvent"),
	MessageEvent: optionalGlobal("MessageEvent"),
	EventSource: optionalGlobal("EventSource"),
	install() {},
	setGlobalDispatcher(dispatcher) {
		globalDispatcher = dispatcher;
	},
	getGlobalDispatcher() {
		return globalDispatcher;
	},
	request: () => notImplemented("request"),
	stream: () => notImplemented("stream"),
	pipeline: () => notImplemented("pipeline"),
	connect: () => notImplemented("connect"),
	upgrade: () => notImplemented("upgrade"),
	Dispatcher: NoopDispatcher,
	Client: NoopAgent,
	Pool: NoopAgent,
	BalancedPool: NoopAgent,
	Agent: NoopAgent,
	ProxyAgent: NoopAgent,
	EnvHttpProxyAgent: NoopAgent,
	RetryAgent: NoopAgent,
	MockAgent: NoopAgent,
	MockClient: NoopAgent,
	MockPool: NoopAgent,
	RedirectHandler: class {},
	RetryHandler: class {},
	errors: {},
};
