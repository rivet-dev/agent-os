import { __export } from "../vendor/esbuild-runtime.js";
import { Blob, File, Headers, MAX_HTTP_BODY_BYTES, MAX_HTTP_REQUEST_HEADERS, MAX_HTTP_REQUEST_HEADER_BYTES, Request, Response, _fetchHandleCounter, createFetchHeaders, ensureFetchAcceptEncoding, fetch, normalizeFetchRequestInit, serializeFetchHeaders } from "./fetch.js";
import { AgentOsPromisesResolver, AgentOsResolver, createInvalidDnsServersError, createUnsupportedDnsError, dns, lookupDnsRecords, normalizeDnsLookupInvocation, normalizeDnsResolveInvocation, normalizeDnsServers, parseDnsLookupRecords, parseDnsResolveRecords, resolveDnsRecords } from "./dns.js";
import { Agent, ClientRequest, DirectTunnelSocket, FakeSocket, HTTP_METHODS, HTTP_STATUS_TEXT, HTTP_TOKEN_EXTRA_CHARS, INVALID_REQUEST_PATH_REGEXP, IncomingMessage, Server, ServerCallable, ServerIncomingMessage, ServerResponseBridge, ServerResponseCallable, UpgradeSocket, appendNormalizedHeader, attachHttpServerSocket, buildHostHeader, buildRawHttpHeaderPairs, buildUndiciOrigin, checkInvalidHeaderChar, checkIsHttpToken, cloneStoredHeaderValue, createAbortError2, createBadRequestResponseBuffer, createConnResetError, createErrorWithCode, createHttpModule, createHttpRequestSocket, createInvalidArgTypeError2, createTypeErrorWithCode, createUnsupportedHttpSocketWriteError, debugBridgeNetwork, dispatchConnectRequest, dispatchHttp2CompatibilityRequest, dispatchLoopbackServerRequest, dispatchServerRequest, dispatchSocketBackedServerRequest, dispatchSocketRequest, dispatchUpgradeRequest, finalizeRawHeaderPairs, flattenHeaderPairs, formatReceivedType, getUndiciClientForSocket, hasResponseBody, hasUpgradeRequestHeaders, http, https, isFlatHeaderList, isLoopbackRequestHost, isRawSocketRequest, isSocketReadyForProtocol, joinHeaderValue, nextServerId, normalizeRequestHeaders, normalizeSocketChunk, onHttpServerRequest, onUpgradeSocketData, onUpgradeSocketEnd, parseChunkedBody, parseContentLengthHeader, parseLoopbackRequestBuffer, parseRawHttpResponse, readUndiciReadableBody, serializeHeaderValue, serializeLoopbackResponse, serializeRawHeaderPairs, serializeRawHttpRequest, serverInstances, socketReadyEventNameForProtocol, splitTransferEncodingTokens, upgradeSocketInstances, validateHeaderName, validateHeaderValue, validateRequestMethod, validateRequestPath, waitForRawHttpResponse, waitForRawHttpResponseHead, waitForSocketReadyForProtocol } from "./http.js";
import { ClientHttp2Stream, DEFAULT_HTTP2_SESSION_STATE, DEFAULT_HTTP2_SETTINGS, HTTP2_INTERNAL_BINDING_CONSTANTS, HTTP2_K_SOCKET, HTTP2_NGHTTP2_ERROR_MESSAGES, HTTP2_OPTIONS, Http2EventEmitter, Http2Server, Http2ServerRequest, Http2ServerResponse, Http2Session, Http2SocketProxy, Http2Stream, NghttpError, S_IFDIR, S_IFIFO, S_IFLNK, S_IFMT, S_IFREG, S_IFSOCK, ServerHttp2Stream, applyHttp2SessionState, cloneHttp2SessionRuntimeState, cloneHttp2Settings, connectHttp2, createHttp2ArgTypeError, createHttp2BridgeStat, createHttp2Error, createHttp2InvalidArgValueError, createHttp2PayloadForbiddenError, createHttp2Server, createHttp2SettingRangeError, createHttp2SettingTypeError, flushPendingHttp2ClientStreamEvents, formatHttp2InvalidValue, getCompleteUtf8PrefixLength, getOrCreateHttp2Session, http2, http2Dispatch, http2Servers, http2Sessions, http2Streams, nextHttp2ServerId, nghttp2ErrorString, normalizeHttp2Authority, normalizeHttp2ConnectArgs, normalizeHttp2FileResponseOptions, normalizeHttp2Headers, onHttp2Dispatch, parseHttp2ErrorPayload, parseHttp2Headers, parseHttp2SessionRuntimeState, parseHttp2SessionState, parseHttp2SocketState, pendingHttp2ClientStreamEvents, pendingHttp2CompatRequests, queuePendingHttp2ClientStreamEvent, queuedHttp2DispatchEvents, resolveHttp2SocketId, schedulePendingHttp2ClientStreamEventsFlush, scheduleQueuedHttp2DispatchDrain, scheduledHttp2ClientStreamFlushes, scheduledHttp2DispatchDrain, serializeHttp2Headers, sliceHttp2FileBody, validateHttp2ConnectOptions, validateHttp2RequestOptions, validateHttp2Settings } from "./http2.js";
import { BlockList, NET_BRIDGE_MAX_RAW_WRITE_BYTES, NET_BRIDGE_TIMEOUT_SENTINEL, NET_SERVER_HANDLE_PREFIX, NET_SOCKET_REGISTRY_PREFIX, NetServer, NetServerCallable, NetSocket, SocketAddress, buildSerializedTlsOptions, classifyIpAddress, coerceIpInput, countAcceptFirstPumpOrigin, countIPv6Parts, countNetBridgeMetric, countReadFirstPumpOrigin, createAcceptedClientHandle, createBridgedTlsError, createConnectedSocketHandle, createFunctionArgTypeError, createListenArgValueError, createNetBridgeMetrics, createSocketBadPortError, createTimeoutArgTypeError, createTimeoutRangeError, defaultAutoSelectFamily, defaultAutoSelectFamilyAttemptTimeout, deserializeTlsBridgeValue, expandIpv6Address, finalizeTlsUpgrade, formatBlockListRule, getRegisteredNetSocket, ipAddressToBigInt, ipv4ToBigInt, ipv6ToBigInt, isDecimalIntegerString, isIPv4String, isIPv6String, isNetBridgeMetricsEnabled, isNetBridgeTraceEnabled, isNetRetainOwnedWriteBufferEnabled, isTlsSecureContextWrapper, isTruthySocketOption, isValidIPv4Segment, isValidIPv6Zone, isValidTcpPort, maxNetBridgeMetric, netBridgeMetrics, netBridgeNowUs, netBridgeTraceForced, netConnect, netModule, netSocketDispatch, normalizeConnectArgs, normalizeIpFamilyLabel, normalizeKeepAliveDelay, normalizeListenArgs, normalizeListenPortValue, normalizeNetSocketHandle, normalizeSocketTimeout, parseNetSocketInfo, parseTlsClientHello, parseTlsState, queryTlsSocket, registerNetServer, registerNetSocket, registeredNetServersByPort, registeredNetSockets, serializeTlsValue, unregisterNetServer, unregisterNetSocket, wakeNetServerAccept, wakeNetServerAcceptForSocket, wakePeerBridgeReads, wakeSocketBridgeReads, yieldBridgeMacrotask } from "./net.js";
import { TLSServer, TLSServerCallable, TLSSocket, adoptRawTlsSocket, createSecureContextWrapper, matchesServername, tlsConnect, tlsModule } from "./tls.js";
import { DGRAM_HANDLE_PREFIX, DgramSocket, createBadDgramSocketTypeError, createDgramAddressError, createDgramAlreadyBoundError, createDgramArgTypeError, createDgramBufferSizeSystemError, createDgramBufferSizeTypeError, createDgramMessageBuffer, createDgramMessageListBuffer, createDgramMissingArgError, createDgramNotRunningError, createDgramSyscallError, createDgramTtlArgTypeError, decodeDgramBridgeBytes, dgramModule, getDgramErrno, getPlatformDgramBufferSize, isIPv4MulticastAddress, isIPv4UnicastAddress, isIPv6MulticastAddress, normalizeDgramAddressValue, normalizeDgramBindArgs, normalizeDgramBridgeResult, normalizeDgramPortValue, normalizeDgramSendArgs, normalizeDgramSocketOptions, normalizeDgramSocketType, normalizeDgramTtlValue, validateDgramMulticastAddress, validateDgramSourceAddress } from "./dgram.js";
import { DatabaseSync, StatementSync, _sqliteConstants, _sqliteDatabaseCheckpoint, _sqliteDatabaseClose, _sqliteDatabaseExec, _sqliteDatabaseLocation, _sqliteDatabaseOpen, _sqliteDatabasePrepare, _sqliteDatabaseQuery, _sqliteStatementAll, _sqliteStatementColumns, _sqliteStatementFinalize, _sqliteStatementGet, _sqliteStatementRun, _sqliteStatementSetAllowBareNamedParameters, _sqliteStatementSetAllowUnknownNamedParameters, _sqliteStatementSetReadBigInts, _sqliteStatementSetReturnArrays, decodeSqliteValue, encodeSqliteValue, getSqliteConstants, isSqlitePlainObject, normalizeSqliteParams, sqliteBridgeCall, sqliteConstants, sqliteModule } from "./sqlite.js";

var network_exports = {};
__export(network_exports, {
  ClientRequest: () => ClientRequest,
  Headers: () => Headers,
  IncomingMessage: () => IncomingMessage,
  Request: () => Request,
  Response: () => Response,
  default: () => network_default,
  dns: () => dns,
  fetch: () => fetch,
  http: () => http,
  http2: () => http2,
  https: () => https
});
var network_default = {
  fetch,
  Headers,
  Request,
  Response,
  dns,
  http,
  https,
  http2,
  IncomingMessage,
  ClientRequest,
  net: netModule,
  tls: tlsModule,
  dgram: dgramModule
};
export * from "./fetch.js";
export * from "./dns.js";
export * from "./http.js";
export * from "./http2.js";
export * from "./net.js";
export * from "./tls.js";
export * from "./dgram.js";
export * from "./sqlite.js";
export { network_exports, network_default };
