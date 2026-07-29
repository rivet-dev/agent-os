import { createHttpRequestSocket, socketReadyEventNameForProtocol } from "./network.js";
import { undiciAgentModule, undiciApiModule, undiciClientModule, undiciFetchModule, undiciGlobalModule, undiciHeadersModule, undiciRequestModule, undiciResponseModule } from "../prelude.js";

var UndiciAgent = undiciAgentModule?.default ?? undiciAgentModule;
var UndiciClient = undiciClientModule?.default ?? undiciClientModule;
var undiciRequest = undiciApiModule?.request ?? undiciApiModule?.default?.request ?? undiciApiModule?.default ?? undiciApiModule;
var undiciFetch = undiciFetchModule?.fetch ?? undiciFetchModule?.default ?? undiciFetchModule;
var UndiciHeaders = undiciHeadersModule?.Headers ?? undiciHeadersModule?.default ?? undiciHeadersModule;
var UndiciRequest = undiciRequestModule?.Request ?? undiciRequestModule?.default ?? undiciRequestModule;
var UndiciResponse = undiciResponseModule?.Response ?? undiciResponseModule?.default ?? undiciResponseModule;
var setUndiciGlobalDispatcher = undiciGlobalModule?.setGlobalDispatcher;
var getUndiciGlobalDispatcher = undiciGlobalModule?.getGlobalDispatcher;
var agentOsUndiciDispatcher = null;
function createAgentOsUndiciDispatcher() {
  const dispatcher = new UndiciAgent({
    // Bound the per-origin connection pool. With an unbounded pool, requests that
    // overlap while the pool's clients are still connecting each find every client
    // marked kNeedDrain and spawn a brand-new Client+socket (HTTP/2: a whole new
    // session) instead of reusing one -- and the bridge's synchronous socket reads
    // widen that connect window. Over a long, many-call turn (e.g. an LLM agent flow)
    // those abandoned clients accumulate their listener sets (connect/close/drain/
    // error/finish/readable/end/terminated) without bound, tripping
    // MaxListenersExceededWarning and degrading the HTTP/2 path until requests abort.
    // Capping connections makes excess requests queue on existing clients (HTTP/2
    // multiplexes within one), so sockets/sessions/listeners stay bounded. 6 mirrors
    // the browser per-origin connection limit; HTTP/2 multiplexes within each.
    connections: 6,
    connect(options, callback) {
      try {
        let protocol = options?.protocol === "https:" || options?.protocol === "https" ? "https:" : "http:";
        let hostname = options?.hostname || options?.host || options?.servername || "localhost";
        let port = options?.port;
        if (options?.origin) {
          const origin = new URL(String(options.origin));
          protocol = origin.protocol === "https:" ? "https:" : "http:";
          hostname = origin.hostname || hostname;
          port = origin.port || port;
        }
        if (typeof hostname === "string" && hostname.startsWith("[") && hostname.endsWith("]")) {
          hostname = hostname.slice(1, -1);
        }
        const socket = createHttpRequestSocket({
          protocol,
          hostname,
          host: hostname,
          port: port ? Number(port) : protocol === "https:" ? 443 : 80,
          servername: options?.servername || hostname,
          rejectUnauthorized: options?.rejectUnauthorized
        });
        const readyEvent = socketReadyEventNameForProtocol(protocol);
        let settled = false;
        const cleanup = () => {
          socket.off?.(readyEvent, onReady);
          socket.removeListener?.(readyEvent, onReady);
          socket.off?.("error", onError);
          socket.removeListener?.("error", onError);
        };
        const onReady = () => {
          if (settled) return;
          settled = true;
          cleanup();
          callback(null, socket);
        };
        const onError = (error) => {
          if (settled) return;
          settled = true;
          cleanup();
          callback(error instanceof Error ? error : new Error(String(error)));
        };
        socket.once(readyEvent, onReady);
        socket.once("error", onError);
        return socket;
      } catch (error) {
        callback(error instanceof Error ? error : new Error(String(error)));
        return null;
      }
    }
  });
  return dispatcher;
}
function getAgentOsUndiciDispatcher() {
  if (!agentOsUndiciDispatcher) {
    agentOsUndiciDispatcher = createAgentOsUndiciDispatcher();
  }
  return agentOsUndiciDispatcher;
}
if (typeof setUndiciGlobalDispatcher === "function" && typeof UndiciAgent === "function") {
  const currentDispatcher = typeof getUndiciGlobalDispatcher === "function" ? getUndiciGlobalDispatcher() : null;
  if (currentDispatcher == null) {
    setUndiciGlobalDispatcher(getAgentOsUndiciDispatcher());
  }
}
export { UndiciAgent, UndiciClient, undiciRequest, undiciFetch, UndiciHeaders, UndiciRequest, UndiciResponse, setUndiciGlobalDispatcher, getUndiciGlobalDispatcher, agentOsUndiciDispatcher, createAgentOsUndiciDispatcher, getAgentOsUndiciDispatcher };
