// @generated - run pnpm --dir packages/core build:agentos-protocol
import * as bare from "@rivetkit/bare-ts"

const DEFAULT_CONFIG = /* @__PURE__ */ bare.Config({})

export type i32 = number
export type u32 = number
export type u64 = bigint

export type JsonUtf8 = string

export function readJsonUtf8(bc: bare.ByteCursor): JsonUtf8 {
    return bare.readString(bc)
}

export function writeJsonUtf8(bc: bare.ByteCursor, x: JsonUtf8): void {
    bare.writeString(bc, x)
}

export enum AcpRuntimeKind {
    JavaScript = "JavaScript",
    Python = "Python",
    WebAssembly = "WebAssembly",
}

export function readAcpRuntimeKind(bc: bare.ByteCursor): AcpRuntimeKind {
    const offset = bc.offset
    const tag = bare.readU8(bc)
    switch (tag) {
        case 0:
            return AcpRuntimeKind.JavaScript
        case 1:
            return AcpRuntimeKind.Python
        case 2:
            return AcpRuntimeKind.WebAssembly
        default: {
            bc.offset = offset
            throw new bare.BareError(offset, "invalid tag")
        }
    }
}

export function writeAcpRuntimeKind(bc: bare.ByteCursor, x: AcpRuntimeKind): void {
    switch (x) {
        case AcpRuntimeKind.JavaScript: {
            bare.writeU8(bc, 0)
            break
        }
        case AcpRuntimeKind.Python: {
            bare.writeU8(bc, 1)
            break
        }
        case AcpRuntimeKind.WebAssembly: {
            bare.writeU8(bc, 2)
            break
        }
    }
}

function read0(bc: bare.ByteCursor): readonly string[] {
    const len = bare.readUintSafe(bc)
    if (len === 0) {
        return []
    }
    const result = [bare.readString(bc)]
    for (let i = 1; i < len; i++) {
        result[i] = bare.readString(bc)
    }
    return result
}

function write0(bc: bare.ByteCursor, x: readonly string[]): void {
    bare.writeUintSafe(bc, x.length)
    for (let i = 0; i < x.length; i++) {
        bare.writeString(bc, x[i])
    }
}

function read1(bc: bare.ByteCursor): ReadonlyMap<string, string> {
    const len = bare.readUintSafe(bc)
    const result = new Map<string, string>()
    for (let i = 0; i < len; i++) {
        const offset = bc.offset
        const key = bare.readString(bc)
        if (result.has(key)) {
            bc.offset = offset
            throw new bare.BareError(offset, "duplicated key")
        }
        result.set(key, bare.readString(bc))
    }
    return result
}

function write1(bc: bare.ByteCursor, x: ReadonlyMap<string, string>): void {
    bare.writeUintSafe(bc, x.size)
    for (const kv of x) {
        bare.writeString(bc, kv[0])
        bare.writeString(bc, kv[1])
    }
}

function read2(bc: bare.ByteCursor): string | null {
    return bare.readBool(bc) ? bare.readString(bc) : null
}

function write2(bc: bare.ByteCursor, x: string | null): void {
    bare.writeBool(bc, x != null)
    if (x != null) {
        bare.writeString(bc, x)
    }
}

/**
 * Legacy connection-owned ACP messages below remain encoded only for the
 * dormant browser reference runtime. They are not part of the public AgentOS
 * session API, and the native sidecar rejects them. Native durable orchestration
 * uses the same structs internally as a private adapter-process driver until the
 * browser protocol can be split into its own schema.
 */
export type AcpCreateSessionRequest = {
    readonly agentType: string
    readonly runtime: AcpRuntimeKind
    readonly cwd: string
    readonly additionalDirectories: readonly string[]
    readonly args: readonly string[]
    readonly env: ReadonlyMap<string, string>
    readonly protocolVersion: i32
    readonly clientCapabilities: JsonUtf8
    readonly mcpServers: JsonUtf8
    readonly skipOsInstructions: boolean
    readonly additionalInstructions: string | null
}

export function readAcpCreateSessionRequest(bc: bare.ByteCursor): AcpCreateSessionRequest {
    return {
        agentType: bare.readString(bc),
        runtime: readAcpRuntimeKind(bc),
        cwd: bare.readString(bc),
        additionalDirectories: read0(bc),
        args: read0(bc),
        env: read1(bc),
        protocolVersion: bare.readI32(bc),
        clientCapabilities: readJsonUtf8(bc),
        mcpServers: readJsonUtf8(bc),
        skipOsInstructions: bare.readBool(bc),
        additionalInstructions: read2(bc),
    }
}

export function writeAcpCreateSessionRequest(bc: bare.ByteCursor, x: AcpCreateSessionRequest): void {
    bare.writeString(bc, x.agentType)
    writeAcpRuntimeKind(bc, x.runtime)
    bare.writeString(bc, x.cwd)
    write0(bc, x.additionalDirectories)
    write0(bc, x.args)
    write1(bc, x.env)
    bare.writeI32(bc, x.protocolVersion)
    writeJsonUtf8(bc, x.clientCapabilities)
    writeJsonUtf8(bc, x.mcpServers)
    bare.writeBool(bc, x.skipOsInstructions)
    write2(bc, x.additionalInstructions)
}

function read3(bc: bare.ByteCursor): JsonUtf8 | null {
    return bare.readBool(bc) ? readJsonUtf8(bc) : null
}

function write3(bc: bare.ByteCursor, x: JsonUtf8 | null): void {
    bare.writeBool(bc, x != null)
    if (x != null) {
        writeJsonUtf8(bc, x)
    }
}

export type AcpSessionRequest = {
    readonly sessionId: string
    readonly method: string
    readonly params: JsonUtf8 | null
}

export function readAcpSessionRequest(bc: bare.ByteCursor): AcpSessionRequest {
    return {
        sessionId: bare.readString(bc),
        method: bare.readString(bc),
        params: read3(bc),
    }
}

export function writeAcpSessionRequest(bc: bare.ByteCursor, x: AcpSessionRequest): void {
    bare.writeString(bc, x.sessionId)
    bare.writeString(bc, x.method)
    write3(bc, x.params)
}

/**
 * Enumerate the agents available in this VM. The sidecar answers from the already
 * projected `/opt/agentos` packages (client parses no manifests).
 */
export type AcpListAgentsRequest = {
    readonly reserved: boolean
}

export function readAcpListAgentsRequest(bc: bare.ByteCursor): AcpListAgentsRequest {
    return {
        reserved: bare.readBool(bc),
    }
}

export function writeAcpListAgentsRequest(bc: bare.ByteCursor, x: AcpListAgentsRequest): void {
    bare.writeBool(bc, x.reserved)
}

export type AcpAgentEntry = {
    readonly id: string
    readonly installed: boolean
    readonly adapterEntrypoint: string
}

export function readAcpAgentEntry(bc: bare.ByteCursor): AcpAgentEntry {
    return {
        id: bare.readString(bc),
        installed: bare.readBool(bc),
        adapterEntrypoint: bare.readString(bc),
    }
}

export function writeAcpAgentEntry(bc: bare.ByteCursor, x: AcpAgentEntry): void {
    bare.writeString(bc, x.id)
    bare.writeBool(bc, x.installed)
    bare.writeString(bc, x.adapterEntrypoint)
}

function read4(bc: bare.ByteCursor): readonly AcpAgentEntry[] {
    const len = bare.readUintSafe(bc)
    if (len === 0) {
        return []
    }
    const result = [readAcpAgentEntry(bc)]
    for (let i = 1; i < len; i++) {
        result[i] = readAcpAgentEntry(bc)
    }
    return result
}

function write4(bc: bare.ByteCursor, x: readonly AcpAgentEntry[]): void {
    bare.writeUintSafe(bc, x.length)
    for (let i = 0; i < x.length; i++) {
        writeAcpAgentEntry(bc, x[i])
    }
}

export type AcpListAgentsResponse = {
    readonly agents: readonly AcpAgentEntry[]
}

export function readAcpListAgentsResponse(bc: bare.ByteCursor): AcpListAgentsResponse {
    return {
        agents: read4(bc),
    }
}

export function writeAcpListAgentsResponse(bc: bare.ByteCursor, x: AcpListAgentsResponse): void {
    write4(bc, x.agents)
}

function read5(bc: bare.ByteCursor): boolean | null {
    return bare.readBool(bc) ? bare.readBool(bc) : null
}

function write5(bc: bare.ByteCursor, x: boolean | null): void {
    bare.writeBool(bc, x != null)
    if (x != null) {
        bare.writeBool(bc, x)
    }
}

/**
 * `openSession` is idempotent. Opening an existing durable ID restores its
 * adapter when unloaded; getSession/listSessions remain the storage-only ways to
 * inspect it without starting an adapter. The sidecar owns ACP session/new versus
 * native resume/load selection and returns only after negotiation has populated
 * the cached configuration/capability/agent-info fields. Omitted cwd resolves to
 * `/home/agentos` in the sidecar. Restoration reapplies the original cwd,
 * additionalDirectories, env, exact native ACP McpServer values, instruction
 * settings, and current configuration values. The response is intentionally
 * empty: the caller owns the requested public ID (or the documented `main`
 * default), and getSession is the only API that returns durable metadata.
 * permissionPolicy is an immutable AgentOS-side strategy for resolving native
 * ACP session/request_permission calls: allow_all (the default) selects an
 * adapter-provided allow option, reject_all selects a reject option or returns a
 * typed permission_policy_unsatisfied error when none is offered,
 * and ask durably exposes the adapter's exact options for a caller response. It
 * does not change VM permissions and is not sent to the ACP adapter as config.
 */
export type AcpOpenSessionRequest = {
    readonly sessionId: string | null
    readonly agent: string
    readonly cwd: string | null
    readonly additionalDirectories: JsonUtf8 | null
    readonly env: JsonUtf8 | null
    readonly mcpServers: JsonUtf8 | null
    readonly permissionPolicy: string | null
    readonly skipOsInstructions: boolean | null
    readonly additionalInstructions: string | null
}

export function readAcpOpenSessionRequest(bc: bare.ByteCursor): AcpOpenSessionRequest {
    return {
        sessionId: read2(bc),
        agent: bare.readString(bc),
        cwd: read2(bc),
        additionalDirectories: read3(bc),
        env: read3(bc),
        mcpServers: read3(bc),
        permissionPolicy: read2(bc),
        skipOsInstructions: read5(bc),
        additionalInstructions: read2(bc),
    }
}

export function writeAcpOpenSessionRequest(bc: bare.ByteCursor, x: AcpOpenSessionRequest): void {
    write2(bc, x.sessionId)
    bare.writeString(bc, x.agent)
    write2(bc, x.cwd)
    write3(bc, x.additionalDirectories)
    write3(bc, x.env)
    write3(bc, x.mcpServers)
    write2(bc, x.permissionPolicy)
    write5(bc, x.skipOsInstructions)
    write2(bc, x.additionalInstructions)
}

/**
 * This lookup never starts, restores, or queries an ACP adapter.
 */
export type AcpGetDurableSessionRequest = {
    readonly sessionId: string | null
}

export function readAcpGetDurableSessionRequest(bc: bare.ByteCursor): AcpGetDurableSessionRequest {
    return {
        sessionId: read2(bc),
    }
}

export function writeAcpGetDurableSessionRequest(bc: bare.ByteCursor, x: AcpGetDurableSessionRequest): void {
    write2(bc, x.sessionId)
}

function read6(bc: bare.ByteCursor): u32 | null {
    return bare.readBool(bc) ? bare.readU32(bc) : null
}

function write6(bc: bare.ByteCursor, x: u32 | null): void {
    bare.writeBool(bc, x != null)
    if (x != null) {
        bare.writeU32(bc, x)
    }
}

/**
 * Listing is an ordinary updatedAt/sessionId keyset traversal. It never starts,
 * restores, or queries an ACP adapter. It deliberately does not freeze a database
 * snapshot across pages: a session updated between page requests can move in the
 * ordering, which keeps cursors simple and avoids a snapshot registry.
 */
export type AcpListDurableSessionsRequest = {
    readonly cursor: string | null
    readonly limit: u32 | null
}

export function readAcpListDurableSessionsRequest(bc: bare.ByteCursor): AcpListDurableSessionsRequest {
    return {
        cursor: read2(bc),
        limit: read6(bc),
    }
}

export function writeAcpListDurableSessionsRequest(bc: bare.ByteCursor, x: AcpListDurableSessionsRequest): void {
    write2(bc, x.cursor)
    write6(bc, x.limit)
}

/**
 * Deletion is naturally idempotent by its target ID. It permanently
 * removes durable metadata and history after orderly runtime teardown.
 */
export type AcpDeleteSessionRequest = {
    readonly sessionId: string | null
}

export function readAcpDeleteSessionRequest(bc: bare.ByteCursor): AcpDeleteSessionRequest {
    return {
        sessionId: read2(bc),
    }
}

export function writeAcpDeleteSessionRequest(bc: bare.ByteCursor, x: AcpDeleteSessionRequest): void {
    write2(bc, x.sessionId)
}

/**
 * Unload cancels active work and releases the adapter but preserves the durable
 * session and all history. A later prompt transparently restores it.
 */
export type AcpUnloadSessionRequest = {
    readonly sessionId: string | null
}

export function readAcpUnloadSessionRequest(bc: bare.ByteCursor): AcpUnloadSessionRequest {
    return {
        sessionId: read2(bc),
    }
}

export function writeAcpUnloadSessionRequest(bc: bare.ByteCursor, x: AcpUnloadSessionRequest): void {
    write2(bc, x.sessionId)
}

/**
 * Prompt never creates a missing session. It transparently restores an unloaded
 * adapter, durably accepts input before dispatch, blocks through the turn, and
 * never automatically replays a prompt whose delivery became uncertain. The
 * sidecar imposes no absolute session/prompt deadline: long turns and ask-policy
 * permission waits remain active while the actor keep-awake scope is held.
 */
export type AcpPromptRequest = {
    readonly sessionId: string | null
    readonly idempotencyKey: string | null
    readonly content: JsonUtf8
}

export function readAcpPromptRequest(bc: bare.ByteCursor): AcpPromptRequest {
    return {
        sessionId: read2(bc),
        idempotencyKey: read2(bc),
        content: readJsonUtf8(bc),
    }
}

export function writeAcpPromptRequest(bc: bare.ByteCursor, x: AcpPromptRequest): void {
    write2(bc, x.sessionId)
    write2(bc, x.idempotencyKey)
    writeJsonUtf8(bc, x.content)
}

/**
 * Cancellation is first-writer-wins against prompt completion and returns its
 * typed race outcome.
 */
export type AcpCancelPromptRequest = {
    readonly sessionId: string | null
}

export function readAcpCancelPromptRequest(bc: bare.ByteCursor): AcpCancelPromptRequest {
    return {
        sessionId: read2(bc),
    }
}

export function writeAcpCancelPromptRequest(bc: bare.ByteCursor, x: AcpCancelPromptRequest): void {
    write2(bc, x.sessionId)
}

/**
 * Respond to a pending native ACP session/request_permission. sessionId is
 * always the explicit caller-owned AgentOS identity; it never defaults to main.
 * requestId is globally unique AgentOS-owned correlation and never contains the
 * adapter JSON-RPC ID. optionId is one of the exact adapter-supplied ACP option
 * identifiers. Resolution is first-writer-wins. accepted means the decision won
 * that race and was delivered to the active ACP waiter, not that the tool ran.
 * Invalid options are typed invalid_permission_option errors and do not consume
 * the request. A terminal late response is non-throwing and names its reason.
 */
export type AcpRespondPermissionRequest = {
    readonly sessionId: string
    readonly requestId: string
    readonly optionId: string
}

export function readAcpRespondPermissionRequest(bc: bare.ByteCursor): AcpRespondPermissionRequest {
    return {
        sessionId: bare.readString(bc),
        requestId: bare.readString(bc),
        optionId: bare.readString(bc),
    }
}

export function writeAcpRespondPermissionRequest(bc: bare.ByteCursor, x: AcpRespondPermissionRequest): void {
    bare.writeString(bc, x.sessionId)
    bare.writeString(bc, x.requestId)
    bare.writeString(bc, x.optionId)
}

function read7(bc: bare.ByteCursor): u64 | null {
    return bare.readBool(bc) ? bare.readU64(bc) : null
}

function write7(bc: bare.ByteCursor, x: u64 | null): void {
    bare.writeBool(bc, x != null)
    if (x != null) {
        bare.writeU64(bc, x)
    }
}

/**
 * History is SQLite-only and never starts, restores, or queries an adapter. It
 * stores the generic durable event union below; nested wire payloads retain
 * unknown extension metadata and clients mechanically expose them as one flat
 * top-level type union. Agent message/thought deltas are emitted live as ephemeral entries;
 * they enter durable history only when the message completes. Non-delta ACP
 * updates interleaved during a message remain in native arrival order.
 * before/after are exclusive and mutually exclusive. Cursor expiry is a typed
 * error, never a silent skip. History retention and response pages are bounded
 * by the VM's generous `limits.acp.*` settings; retention prunes oldest events.
 * Durable sequence values can repeat on live delivery because observers
 * deduplicate by (sessionId, sequence).
 */
export type AcpReadHistoryRequest = {
    readonly sessionId: string | null
    readonly before: u64 | null
    readonly after: u64 | null
    readonly limit: u32 | null
}

export function readAcpReadHistoryRequest(bc: bare.ByteCursor): AcpReadHistoryRequest {
    return {
        sessionId: read2(bc),
        before: read7(bc),
        after: read7(bc),
        limit: read6(bc),
    }
}

export function writeAcpReadHistoryRequest(bc: bare.ByteCursor, x: AcpReadHistoryRequest): void {
    write2(bc, x.sessionId)
    write7(bc, x.before)
    write7(bc, x.after)
    write6(bc, x.limit)
}

/**
 * Cached getters never start, restore, or query an adapter.
 */
export type AcpGetSessionConfigRequest = {
    readonly sessionId: string | null
}

export function readAcpGetSessionConfigRequest(bc: bare.ByteCursor): AcpGetSessionConfigRequest {
    return {
        sessionId: read2(bc),
    }
}

export function writeAcpGetSessionConfigRequest(bc: bare.ByteCursor, x: AcpGetSessionConfigRequest): void {
    write2(bc, x.sessionId)
}

export type AcpGetSessionCapabilitiesRequest = {
    readonly sessionId: string | null
}

export function readAcpGetSessionCapabilitiesRequest(bc: bare.ByteCursor): AcpGetSessionCapabilitiesRequest {
    return {
        sessionId: read2(bc),
    }
}

export function writeAcpGetSessionCapabilitiesRequest(bc: bare.ByteCursor, x: AcpGetSessionCapabilitiesRequest): void {
    write2(bc, x.sessionId)
}

export type AcpGetSessionAgentInfoRequest = {
    readonly sessionId: string | null
}

export function readAcpGetSessionAgentInfoRequest(bc: bare.ByteCursor): AcpGetSessionAgentInfoRequest {
    return {
        sessionId: read2(bc),
    }
}

export function writeAcpGetSessionAgentInfoRequest(bc: bare.ByteCursor, x: AcpGetSessionAgentInfoRequest): void {
    write2(bc, x.sessionId)
}

/**
 * Setting configuration may transparently restore the adapter; ACP owns
 * validation and the response replaces the complete cached option collection.
 * `value` encodes exactly one native ACP string or boolean config value.
 */
export type AcpSetSessionConfigOptionRequest = {
    readonly sessionId: string | null
    readonly configId: string
    readonly value: JsonUtf8
}

export function readAcpSetSessionConfigOptionRequest(bc: bare.ByteCursor): AcpSetSessionConfigOptionRequest {
    return {
        sessionId: read2(bc),
        configId: bare.readString(bc),
        value: readJsonUtf8(bc),
    }
}

export function writeAcpSetSessionConfigOptionRequest(bc: bare.ByteCursor, x: AcpSetSessionConfigOptionRequest): void {
    write2(bc, x.sessionId)
    bare.writeString(bc, x.configId)
    writeJsonUtf8(bc, x.value)
}

export type AcpGetSessionStateRequest = {
    readonly sessionId: string
}

export function readAcpGetSessionStateRequest(bc: bare.ByteCursor): AcpGetSessionStateRequest {
    return {
        sessionId: bare.readString(bc),
    }
}

export function writeAcpGetSessionStateRequest(bc: bare.ByteCursor, x: AcpGetSessionStateRequest): void {
    bare.writeString(bc, x.sessionId)
}

export type AcpCloseSessionRequest = {
    readonly sessionId: string
}

export function readAcpCloseSessionRequest(bc: bare.ByteCursor): AcpCloseSessionRequest {
    return {
        sessionId: bare.readString(bc),
    }
}

export function writeAcpCloseSessionRequest(bc: bare.ByteCursor, x: AcpCloseSessionRequest): void {
    bare.writeString(bc, x.sessionId)
}

/**
 * Resume a session that exists in durable storage but is not live in the current
 * VM (e.g. after a Rivet actor slept and woke with a fresh VM). The sidecar runs
 * the stateless resume state machine (native session/load when the agent supports
 * it, else a fresh session/new + transcript continuation preamble). `runtime`,
 * `cwd`, and `env` describe the fresh adapter launch used by the fallback tier.
 * `transcriptPath`, when present, is a guest-readable path the fallback preamble
 * points the agent at.
 */
export type AcpResumeSessionRequest = {
    readonly sessionId: string
    readonly agentType: string
    readonly runtime: AcpRuntimeKind
    readonly transcriptPath: string | null
    readonly cwd: string
    readonly additionalDirectories: readonly string[]
    readonly mcpServers: JsonUtf8
    readonly env: ReadonlyMap<string, string>
}

export function readAcpResumeSessionRequest(bc: bare.ByteCursor): AcpResumeSessionRequest {
    return {
        sessionId: bare.readString(bc),
        agentType: bare.readString(bc),
        runtime: readAcpRuntimeKind(bc),
        transcriptPath: read2(bc),
        cwd: bare.readString(bc),
        additionalDirectories: read0(bc),
        mcpServers: readJsonUtf8(bc),
        env: read1(bc),
    }
}

export function writeAcpResumeSessionRequest(bc: bare.ByteCursor, x: AcpResumeSessionRequest): void {
    bare.writeString(bc, x.sessionId)
    bare.writeString(bc, x.agentType)
    writeAcpRuntimeKind(bc, x.runtime)
    write2(bc, x.transcriptPath)
    bare.writeString(bc, x.cwd)
    write0(bc, x.additionalDirectories)
    writeJsonUtf8(bc, x.mcpServers)
    write1(bc, x.env)
}

/**
 * Browser RESUMABLE path only (AGENTOS-WEB-ASYNC-AGENTS.md §3.2.1): the kernel
 * worker feeds a chunk of the agent's stdout into the in-flight create_session /
 * session/prompt handshake. The synchronous sidecar would block inside one
 * pushFrame; the resumable browser path returns between steps so the worker can
 * service the agent's own syscalls (incl. pi's net call for inference) on fresh,
 * non-nested pushFrames. `processId` is the handshake handle returned in the
 * AcpPendingResponse for the originating create/prompt request.
 */
export type AcpDeliverAgentOutputRequest = {
    readonly processId: string
    readonly chunk: ArrayBuffer
}

export function readAcpDeliverAgentOutputRequest(bc: bare.ByteCursor): AcpDeliverAgentOutputRequest {
    return {
        processId: bare.readString(bc),
        chunk: bare.readData(bc),
    }
}

export function writeAcpDeliverAgentOutputRequest(bc: bare.ByteCursor, x: AcpDeliverAgentOutputRequest): void {
    bare.writeString(bc, x.processId)
    bare.writeData(bc, x.chunk)
}

export type AcpRequest =
    | { readonly tag: "AcpOpenSessionRequest"; readonly val: AcpOpenSessionRequest }
    | { readonly tag: "AcpGetDurableSessionRequest"; readonly val: AcpGetDurableSessionRequest }
    | { readonly tag: "AcpListDurableSessionsRequest"; readonly val: AcpListDurableSessionsRequest }
    | { readonly tag: "AcpDeleteSessionRequest"; readonly val: AcpDeleteSessionRequest }
    | { readonly tag: "AcpUnloadSessionRequest"; readonly val: AcpUnloadSessionRequest }
    | { readonly tag: "AcpPromptRequest"; readonly val: AcpPromptRequest }
    | { readonly tag: "AcpCancelPromptRequest"; readonly val: AcpCancelPromptRequest }
    | { readonly tag: "AcpRespondPermissionRequest"; readonly val: AcpRespondPermissionRequest }
    | { readonly tag: "AcpReadHistoryRequest"; readonly val: AcpReadHistoryRequest }
    | { readonly tag: "AcpGetSessionConfigRequest"; readonly val: AcpGetSessionConfigRequest }
    | { readonly tag: "AcpSetSessionConfigOptionRequest"; readonly val: AcpSetSessionConfigOptionRequest }
    | { readonly tag: "AcpGetSessionCapabilitiesRequest"; readonly val: AcpGetSessionCapabilitiesRequest }
    | { readonly tag: "AcpGetSessionAgentInfoRequest"; readonly val: AcpGetSessionAgentInfoRequest }
    | { readonly tag: "AcpCreateSessionRequest"; readonly val: AcpCreateSessionRequest }
    | { readonly tag: "AcpSessionRequest"; readonly val: AcpSessionRequest }
    | { readonly tag: "AcpGetSessionStateRequest"; readonly val: AcpGetSessionStateRequest }
    | { readonly tag: "AcpCloseSessionRequest"; readonly val: AcpCloseSessionRequest }
    | { readonly tag: "AcpResumeSessionRequest"; readonly val: AcpResumeSessionRequest }
    | { readonly tag: "AcpDeliverAgentOutputRequest"; readonly val: AcpDeliverAgentOutputRequest }
    | { readonly tag: "AcpListAgentsRequest"; readonly val: AcpListAgentsRequest }

export function readAcpRequest(bc: bare.ByteCursor): AcpRequest {
    const offset = bc.offset
    const tag = bare.readU8(bc)
    switch (tag) {
        case 0:
            return { tag: "AcpOpenSessionRequest", val: readAcpOpenSessionRequest(bc) }
        case 1:
            return { tag: "AcpGetDurableSessionRequest", val: readAcpGetDurableSessionRequest(bc) }
        case 2:
            return { tag: "AcpListDurableSessionsRequest", val: readAcpListDurableSessionsRequest(bc) }
        case 3:
            return { tag: "AcpDeleteSessionRequest", val: readAcpDeleteSessionRequest(bc) }
        case 4:
            return { tag: "AcpUnloadSessionRequest", val: readAcpUnloadSessionRequest(bc) }
        case 5:
            return { tag: "AcpPromptRequest", val: readAcpPromptRequest(bc) }
        case 6:
            return { tag: "AcpCancelPromptRequest", val: readAcpCancelPromptRequest(bc) }
        case 7:
            return { tag: "AcpRespondPermissionRequest", val: readAcpRespondPermissionRequest(bc) }
        case 8:
            return { tag: "AcpReadHistoryRequest", val: readAcpReadHistoryRequest(bc) }
        case 9:
            return { tag: "AcpGetSessionConfigRequest", val: readAcpGetSessionConfigRequest(bc) }
        case 10:
            return { tag: "AcpSetSessionConfigOptionRequest", val: readAcpSetSessionConfigOptionRequest(bc) }
        case 11:
            return { tag: "AcpGetSessionCapabilitiesRequest", val: readAcpGetSessionCapabilitiesRequest(bc) }
        case 12:
            return { tag: "AcpGetSessionAgentInfoRequest", val: readAcpGetSessionAgentInfoRequest(bc) }
        case 13:
            return { tag: "AcpCreateSessionRequest", val: readAcpCreateSessionRequest(bc) }
        case 14:
            return { tag: "AcpSessionRequest", val: readAcpSessionRequest(bc) }
        case 15:
            return { tag: "AcpGetSessionStateRequest", val: readAcpGetSessionStateRequest(bc) }
        case 16:
            return { tag: "AcpCloseSessionRequest", val: readAcpCloseSessionRequest(bc) }
        case 17:
            return { tag: "AcpResumeSessionRequest", val: readAcpResumeSessionRequest(bc) }
        case 18:
            return { tag: "AcpDeliverAgentOutputRequest", val: readAcpDeliverAgentOutputRequest(bc) }
        case 19:
            return { tag: "AcpListAgentsRequest", val: readAcpListAgentsRequest(bc) }
        default: {
            bc.offset = offset
            throw new bare.BareError(offset, "invalid tag")
        }
    }
}

export function writeAcpRequest(bc: bare.ByteCursor, x: AcpRequest): void {
    switch (x.tag) {
        case "AcpOpenSessionRequest": {
            bare.writeU8(bc, 0)
            writeAcpOpenSessionRequest(bc, x.val)
            break
        }
        case "AcpGetDurableSessionRequest": {
            bare.writeU8(bc, 1)
            writeAcpGetDurableSessionRequest(bc, x.val)
            break
        }
        case "AcpListDurableSessionsRequest": {
            bare.writeU8(bc, 2)
            writeAcpListDurableSessionsRequest(bc, x.val)
            break
        }
        case "AcpDeleteSessionRequest": {
            bare.writeU8(bc, 3)
            writeAcpDeleteSessionRequest(bc, x.val)
            break
        }
        case "AcpUnloadSessionRequest": {
            bare.writeU8(bc, 4)
            writeAcpUnloadSessionRequest(bc, x.val)
            break
        }
        case "AcpPromptRequest": {
            bare.writeU8(bc, 5)
            writeAcpPromptRequest(bc, x.val)
            break
        }
        case "AcpCancelPromptRequest": {
            bare.writeU8(bc, 6)
            writeAcpCancelPromptRequest(bc, x.val)
            break
        }
        case "AcpRespondPermissionRequest": {
            bare.writeU8(bc, 7)
            writeAcpRespondPermissionRequest(bc, x.val)
            break
        }
        case "AcpReadHistoryRequest": {
            bare.writeU8(bc, 8)
            writeAcpReadHistoryRequest(bc, x.val)
            break
        }
        case "AcpGetSessionConfigRequest": {
            bare.writeU8(bc, 9)
            writeAcpGetSessionConfigRequest(bc, x.val)
            break
        }
        case "AcpSetSessionConfigOptionRequest": {
            bare.writeU8(bc, 10)
            writeAcpSetSessionConfigOptionRequest(bc, x.val)
            break
        }
        case "AcpGetSessionCapabilitiesRequest": {
            bare.writeU8(bc, 11)
            writeAcpGetSessionCapabilitiesRequest(bc, x.val)
            break
        }
        case "AcpGetSessionAgentInfoRequest": {
            bare.writeU8(bc, 12)
            writeAcpGetSessionAgentInfoRequest(bc, x.val)
            break
        }
        case "AcpCreateSessionRequest": {
            bare.writeU8(bc, 13)
            writeAcpCreateSessionRequest(bc, x.val)
            break
        }
        case "AcpSessionRequest": {
            bare.writeU8(bc, 14)
            writeAcpSessionRequest(bc, x.val)
            break
        }
        case "AcpGetSessionStateRequest": {
            bare.writeU8(bc, 15)
            writeAcpGetSessionStateRequest(bc, x.val)
            break
        }
        case "AcpCloseSessionRequest": {
            bare.writeU8(bc, 16)
            writeAcpCloseSessionRequest(bc, x.val)
            break
        }
        case "AcpResumeSessionRequest": {
            bare.writeU8(bc, 17)
            writeAcpResumeSessionRequest(bc, x.val)
            break
        }
        case "AcpDeliverAgentOutputRequest": {
            bare.writeU8(bc, 18)
            writeAcpDeliverAgentOutputRequest(bc, x.val)
            break
        }
        case "AcpListAgentsRequest": {
            bare.writeU8(bc, 19)
            writeAcpListAgentsRequest(bc, x.val)
            break
        }
    }
}

export function encodeAcpRequest(x: AcpRequest, config?: Partial<bare.Config>): Uint8Array {
    const fullConfig = config != null ? bare.Config(config) : DEFAULT_CONFIG
    const bc = new bare.ByteCursor(
        new Uint8Array(fullConfig.initialBufferLength),
        fullConfig,
    )
    writeAcpRequest(bc, x)
    return new Uint8Array(bc.view.buffer, bc.view.byteOffset, bc.offset)
}

export function decodeAcpRequest(bytes: Uint8Array): AcpRequest {
    const bc = new bare.ByteCursor(bytes, DEFAULT_CONFIG)
    const result = readAcpRequest(bc)
    if (bc.offset < bc.view.byteLength) {
        throw new bare.BareError(bc.offset, "remaining bytes")
    }
    return result
}

export type AcpDurableSessionInfo = {
    readonly sessionId: string
    readonly agent: string
    readonly cwd: string
    readonly additionalDirectories: JsonUtf8
    readonly state: JsonUtf8
    readonly latestSequence: u64
    readonly title: string | null
    readonly metadata: JsonUtf8 | null
    readonly createdAt: string
    readonly updatedAt: string
}

export function readAcpDurableSessionInfo(bc: bare.ByteCursor): AcpDurableSessionInfo {
    return {
        sessionId: bare.readString(bc),
        agent: bare.readString(bc),
        cwd: bare.readString(bc),
        additionalDirectories: readJsonUtf8(bc),
        state: readJsonUtf8(bc),
        latestSequence: bare.readU64(bc),
        title: read2(bc),
        metadata: read3(bc),
        createdAt: bare.readString(bc),
        updatedAt: bare.readString(bc),
    }
}

export function writeAcpDurableSessionInfo(bc: bare.ByteCursor, x: AcpDurableSessionInfo): void {
    bare.writeString(bc, x.sessionId)
    bare.writeString(bc, x.agent)
    bare.writeString(bc, x.cwd)
    writeJsonUtf8(bc, x.additionalDirectories)
    writeJsonUtf8(bc, x.state)
    bare.writeU64(bc, x.latestSequence)
    write2(bc, x.title)
    write3(bc, x.metadata)
    bare.writeString(bc, x.createdAt)
    bare.writeString(bc, x.updatedAt)
}

export type AcpOpenSessionResponse = {
    readonly reserved: boolean
}

export function readAcpOpenSessionResponse(bc: bare.ByteCursor): AcpOpenSessionResponse {
    return {
        reserved: bare.readBool(bc),
    }
}

export function writeAcpOpenSessionResponse(bc: bare.ByteCursor, x: AcpOpenSessionResponse): void {
    bare.writeBool(bc, x.reserved)
}

export type AcpGetDurableSessionResponse = {
    readonly session: AcpDurableSessionInfo
}

export function readAcpGetDurableSessionResponse(bc: bare.ByteCursor): AcpGetDurableSessionResponse {
    return {
        session: readAcpDurableSessionInfo(bc),
    }
}

export function writeAcpGetDurableSessionResponse(bc: bare.ByteCursor, x: AcpGetDurableSessionResponse): void {
    writeAcpDurableSessionInfo(bc, x.session)
}

function read8(bc: bare.ByteCursor): readonly AcpDurableSessionInfo[] {
    const len = bare.readUintSafe(bc)
    if (len === 0) {
        return []
    }
    const result = [readAcpDurableSessionInfo(bc)]
    for (let i = 1; i < len; i++) {
        result[i] = readAcpDurableSessionInfo(bc)
    }
    return result
}

function write8(bc: bare.ByteCursor, x: readonly AcpDurableSessionInfo[]): void {
    bare.writeUintSafe(bc, x.length)
    for (let i = 0; i < x.length; i++) {
        writeAcpDurableSessionInfo(bc, x[i])
    }
}

export type AcpListDurableSessionsResponse = {
    readonly sessions: readonly AcpDurableSessionInfo[]
    readonly nextCursor: string | null
}

export function readAcpListDurableSessionsResponse(bc: bare.ByteCursor): AcpListDurableSessionsResponse {
    return {
        sessions: read8(bc),
        nextCursor: read2(bc),
    }
}

export function writeAcpListDurableSessionsResponse(bc: bare.ByteCursor, x: AcpListDurableSessionsResponse): void {
    write8(bc, x.sessions)
    write2(bc, x.nextCursor)
}

export type AcpDeleteSessionResponse = {
    readonly reserved: boolean
}

export function readAcpDeleteSessionResponse(bc: bare.ByteCursor): AcpDeleteSessionResponse {
    return {
        reserved: bare.readBool(bc),
    }
}

export function writeAcpDeleteSessionResponse(bc: bare.ByteCursor, x: AcpDeleteSessionResponse): void {
    bare.writeBool(bc, x.reserved)
}

export type AcpUnloadSessionResponse = {
    readonly reserved: boolean
}

export function readAcpUnloadSessionResponse(bc: bare.ByteCursor): AcpUnloadSessionResponse {
    return {
        reserved: bare.readBool(bc),
    }
}

export function writeAcpUnloadSessionResponse(bc: bare.ByteCursor, x: AcpUnloadSessionResponse): void {
    bare.writeBool(bc, x.reserved)
}

export type AcpPromptResponse = {
    readonly sessionId: string
    readonly message: JsonUtf8 | null
    readonly stopReason: string
}

export function readAcpPromptResponse(bc: bare.ByteCursor): AcpPromptResponse {
    return {
        sessionId: bare.readString(bc),
        message: read3(bc),
        stopReason: bare.readString(bc),
    }
}

export function writeAcpPromptResponse(bc: bare.ByteCursor, x: AcpPromptResponse): void {
    bare.writeString(bc, x.sessionId)
    write3(bc, x.message)
    bare.writeString(bc, x.stopReason)
}

export type AcpCancelPromptResponse = {
    readonly status: string
}

export function readAcpCancelPromptResponse(bc: bare.ByteCursor): AcpCancelPromptResponse {
    return {
        status: bare.readString(bc),
    }
}

export function writeAcpCancelPromptResponse(bc: bare.ByteCursor, x: AcpCancelPromptResponse): void {
    bare.writeString(bc, x.status)
}

export type AcpRespondPermissionResponse = {
    readonly status: string
    readonly reason: string | null
}

export function readAcpRespondPermissionResponse(bc: bare.ByteCursor): AcpRespondPermissionResponse {
    return {
        status: bare.readString(bc),
        reason: read2(bc),
    }
}

export function writeAcpRespondPermissionResponse(bc: bare.ByteCursor, x: AcpRespondPermissionResponse): void {
    bare.writeString(bc, x.status)
    write2(bc, x.reason)
}

/**
 * The generic internal wire/storage union. Native payloads retain their
 * negotiated ACP field names, optional values, and opaque _meta. Public clients
 * flatten SessionUpdate.sessionUpdate into top-level type and flatten permission
 * request/response fields beside the durability envelope; they do not expose
 * these internal update/request/response wrappers. Native adapter request IDs
 * and private ACP session IDs never enter the public union. Automatically
 * resolved permission requests are never persisted or emitted. An ask request
 * has no permission timeout: the active prompt remains awake until a response
 * or explicit adapter/session/VM lifecycle transition wins.
 */
export type AcpDurableSessionUpdate = {
    readonly update: JsonUtf8
}

export function readAcpDurableSessionUpdate(bc: bare.ByteCursor): AcpDurableSessionUpdate {
    return {
        update: readJsonUtf8(bc),
    }
}

export function writeAcpDurableSessionUpdate(bc: bare.ByteCursor, x: AcpDurableSessionUpdate): void {
    writeJsonUtf8(bc, x.update)
}

export type AcpDurablePermissionRequest = {
    readonly requestId: string
    readonly request: JsonUtf8
}

export function readAcpDurablePermissionRequest(bc: bare.ByteCursor): AcpDurablePermissionRequest {
    return {
        requestId: bare.readString(bc),
        request: readJsonUtf8(bc),
    }
}

export function writeAcpDurablePermissionRequest(bc: bare.ByteCursor, x: AcpDurablePermissionRequest): void {
    bare.writeString(bc, x.requestId)
    writeJsonUtf8(bc, x.request)
}

export type AcpDurablePermissionResponse = {
    readonly requestId: string
    readonly response: JsonUtf8
    readonly status: string
    readonly reason: string | null
}

export function readAcpDurablePermissionResponse(bc: bare.ByteCursor): AcpDurablePermissionResponse {
    return {
        requestId: bare.readString(bc),
        response: readJsonUtf8(bc),
        status: bare.readString(bc),
        reason: read2(bc),
    }
}

export function writeAcpDurablePermissionResponse(bc: bare.ByteCursor, x: AcpDurablePermissionResponse): void {
    bare.writeString(bc, x.requestId)
    writeJsonUtf8(bc, x.response)
    bare.writeString(bc, x.status)
    write2(bc, x.reason)
}

export type AcpDurableEvent =
    | { readonly tag: "AcpDurableSessionUpdate"; readonly val: AcpDurableSessionUpdate }
    | { readonly tag: "AcpDurablePermissionRequest"; readonly val: AcpDurablePermissionRequest }
    | { readonly tag: "AcpDurablePermissionResponse"; readonly val: AcpDurablePermissionResponse }

export function readAcpDurableEvent(bc: bare.ByteCursor): AcpDurableEvent {
    const offset = bc.offset
    const tag = bare.readU8(bc)
    switch (tag) {
        case 0:
            return { tag: "AcpDurableSessionUpdate", val: readAcpDurableSessionUpdate(bc) }
        case 1:
            return { tag: "AcpDurablePermissionRequest", val: readAcpDurablePermissionRequest(bc) }
        case 2:
            return { tag: "AcpDurablePermissionResponse", val: readAcpDurablePermissionResponse(bc) }
        default: {
            bc.offset = offset
            throw new bare.BareError(offset, "invalid tag")
        }
    }
}

export function writeAcpDurableEvent(bc: bare.ByteCursor, x: AcpDurableEvent): void {
    switch (x.tag) {
        case "AcpDurableSessionUpdate": {
            bare.writeU8(bc, 0)
            writeAcpDurableSessionUpdate(bc, x.val)
            break
        }
        case "AcpDurablePermissionRequest": {
            bare.writeU8(bc, 1)
            writeAcpDurablePermissionRequest(bc, x.val)
            break
        }
        case "AcpDurablePermissionResponse": {
            bare.writeU8(bc, 2)
            writeAcpDurablePermissionResponse(bc, x.val)
            break
        }
    }
}

/**
 * AgentOS adds only this public identity, sequence, and timestamp envelope.
 */
export type AcpDurableHistoryEntry = {
    readonly sessionId: string
    readonly sequence: u64
    readonly timestamp: string
    readonly event: AcpDurableEvent
}

export function readAcpDurableHistoryEntry(bc: bare.ByteCursor): AcpDurableHistoryEntry {
    return {
        sessionId: bare.readString(bc),
        sequence: bare.readU64(bc),
        timestamp: bare.readString(bc),
        event: readAcpDurableEvent(bc),
    }
}

export function writeAcpDurableHistoryEntry(bc: bare.ByteCursor, x: AcpDurableHistoryEntry): void {
    bare.writeString(bc, x.sessionId)
    bare.writeU64(bc, x.sequence)
    bare.writeString(bc, x.timestamp)
    writeAcpDurableEvent(bc, x.event)
}

function read9(bc: bare.ByteCursor): readonly AcpDurableHistoryEntry[] {
    const len = bare.readUintSafe(bc)
    if (len === 0) {
        return []
    }
    const result = [readAcpDurableHistoryEntry(bc)]
    for (let i = 1; i < len; i++) {
        result[i] = readAcpDurableHistoryEntry(bc)
    }
    return result
}

function write9(bc: bare.ByteCursor, x: readonly AcpDurableHistoryEntry[]): void {
    bare.writeUintSafe(bc, x.length)
    for (let i = 0; i < x.length; i++) {
        writeAcpDurableHistoryEntry(bc, x[i])
    }
}

export type AcpHistoryPageResponse = {
    readonly events: readonly AcpDurableHistoryEntry[]
    readonly hasMoreBefore: boolean
    readonly hasMoreAfter: boolean
}

export function readAcpHistoryPageResponse(bc: bare.ByteCursor): AcpHistoryPageResponse {
    return {
        events: read9(bc),
        hasMoreBefore: bare.readBool(bc),
        hasMoreAfter: bare.readBool(bc),
    }
}

export function writeAcpHistoryPageResponse(bc: bare.ByteCursor, x: AcpHistoryPageResponse): void {
    write9(bc, x.events)
    bare.writeBool(bc, x.hasMoreBefore)
    bare.writeBool(bc, x.hasMoreAfter)
}

export type AcpSessionConfigResponse = {
    readonly revision: u64
    readonly options: JsonUtf8
}

export function readAcpSessionConfigResponse(bc: bare.ByteCursor): AcpSessionConfigResponse {
    return {
        revision: bare.readU64(bc),
        options: readJsonUtf8(bc),
    }
}

export function writeAcpSessionConfigResponse(bc: bare.ByteCursor, x: AcpSessionConfigResponse): void {
    bare.writeU64(bc, x.revision)
    writeJsonUtf8(bc, x.options)
}

export type AcpSessionCapabilitiesResponse = {
    readonly capabilities: JsonUtf8 | null
}

export function readAcpSessionCapabilitiesResponse(bc: bare.ByteCursor): AcpSessionCapabilitiesResponse {
    return {
        capabilities: read3(bc),
    }
}

export function writeAcpSessionCapabilitiesResponse(bc: bare.ByteCursor, x: AcpSessionCapabilitiesResponse): void {
    write3(bc, x.capabilities)
}

export type AcpSessionAgentInfoResponse = {
    readonly agentInfo: JsonUtf8 | null
}

export function readAcpSessionAgentInfoResponse(bc: bare.ByteCursor): AcpSessionAgentInfoResponse {
    return {
        agentInfo: read3(bc),
    }
}

export function writeAcpSessionAgentInfoResponse(bc: bare.ByteCursor, x: AcpSessionAgentInfoResponse): void {
    write3(bc, x.agentInfo)
}

function read10(bc: bare.ByteCursor): readonly JsonUtf8[] {
    const len = bare.readUintSafe(bc)
    if (len === 0) {
        return []
    }
    const result = [readJsonUtf8(bc)]
    for (let i = 1; i < len; i++) {
        result[i] = readJsonUtf8(bc)
    }
    return result
}

function write10(bc: bare.ByteCursor, x: readonly JsonUtf8[]): void {
    bare.writeUintSafe(bc, x.length)
    for (let i = 0; i < x.length; i++) {
        writeJsonUtf8(bc, x[i])
    }
}

export type AcpSessionCreatedResponse = {
    readonly sessionId: string
    readonly pid: u32 | null
    readonly modes: JsonUtf8 | null
    readonly configOptions: readonly JsonUtf8[]
    readonly agentCapabilities: JsonUtf8 | null
    readonly agentInfo: JsonUtf8 | null
}

export function readAcpSessionCreatedResponse(bc: bare.ByteCursor): AcpSessionCreatedResponse {
    return {
        sessionId: bare.readString(bc),
        pid: read6(bc),
        modes: read3(bc),
        configOptions: read10(bc),
        agentCapabilities: read3(bc),
        agentInfo: read3(bc),
    }
}

export function writeAcpSessionCreatedResponse(bc: bare.ByteCursor, x: AcpSessionCreatedResponse): void {
    bare.writeString(bc, x.sessionId)
    write6(bc, x.pid)
    write3(bc, x.modes)
    write10(bc, x.configOptions)
    write3(bc, x.agentCapabilities)
    write3(bc, x.agentInfo)
}

export type AcpSessionRpcResponse = {
    readonly sessionId: string
    readonly response: JsonUtf8
    /**
     * Number of request-scoped AcpSessionEvent frames emitted before this
     * terminal response. Clients use this as an event-delivery barrier because
     * response and event frames travel on separate priority lanes.
     */
    readonly eventCount: u32
}

export function readAcpSessionRpcResponse(bc: bare.ByteCursor): AcpSessionRpcResponse {
    return {
        sessionId: bare.readString(bc),
        response: readJsonUtf8(bc),
        eventCount: bare.readU32(bc),
    }
}

export function writeAcpSessionRpcResponse(bc: bare.ByteCursor, x: AcpSessionRpcResponse): void {
    bare.writeString(bc, x.sessionId)
    writeJsonUtf8(bc, x.response)
    bare.writeU32(bc, x.eventCount)
}

function read11(bc: bare.ByteCursor): i32 | null {
    return bare.readBool(bc) ? bare.readI32(bc) : null
}

function write11(bc: bare.ByteCursor, x: i32 | null): void {
    bare.writeBool(bc, x != null)
    if (x != null) {
        bare.writeI32(bc, x)
    }
}

export type AcpSessionStateResponse = {
    readonly sessionId: string
    readonly agentType: string
    readonly processId: string
    readonly pid: u32 | null
    readonly closed: boolean
    readonly exitCode: i32 | null
    readonly modes: JsonUtf8 | null
    readonly configOptions: readonly JsonUtf8[]
    readonly agentCapabilities: JsonUtf8 | null
    readonly agentInfo: JsonUtf8 | null
}

export function readAcpSessionStateResponse(bc: bare.ByteCursor): AcpSessionStateResponse {
    return {
        sessionId: bare.readString(bc),
        agentType: bare.readString(bc),
        processId: bare.readString(bc),
        pid: read6(bc),
        closed: bare.readBool(bc),
        exitCode: read11(bc),
        modes: read3(bc),
        configOptions: read10(bc),
        agentCapabilities: read3(bc),
        agentInfo: read3(bc),
    }
}

export function writeAcpSessionStateResponse(bc: bare.ByteCursor, x: AcpSessionStateResponse): void {
    bare.writeString(bc, x.sessionId)
    bare.writeString(bc, x.agentType)
    bare.writeString(bc, x.processId)
    write6(bc, x.pid)
    bare.writeBool(bc, x.closed)
    write11(bc, x.exitCode)
    write3(bc, x.modes)
    write10(bc, x.configOptions)
    write3(bc, x.agentCapabilities)
    write3(bc, x.agentInfo)
}

export type AcpSessionClosedResponse = {
    readonly sessionId: string
}

export function readAcpSessionClosedResponse(bc: bare.ByteCursor): AcpSessionClosedResponse {
    return {
        sessionId: bare.readString(bc),
    }
}

export function writeAcpSessionClosedResponse(bc: bare.ByteCursor, x: AcpSessionClosedResponse): void {
    bare.writeString(bc, x.sessionId)
}

/**
 * Result of AcpResumeSessionRequest. `sessionId` is the live ACP session id after
 * resume: equal to the requested id for native loads, or the freshly assigned id
 * for the fallback tier (the caller remaps external -> live). `mode` is "native"
 * (session/load|resume succeeded) or "fallback" (a new session was created and the
 * transcript-continuation preamble was armed for the next prompt).
 */
export type AcpSessionResumedResponse = {
    readonly sessionId: string
    readonly mode: string
}

export function readAcpSessionResumedResponse(bc: bare.ByteCursor): AcpSessionResumedResponse {
    return {
        sessionId: bare.readString(bc),
        mode: bare.readString(bc),
    }
}

export function writeAcpSessionResumedResponse(bc: bare.ByteCursor, x: AcpSessionResumedResponse): void {
    bare.writeString(bc, x.sessionId)
    bare.writeString(bc, x.mode)
}

export type AcpErrorResponse = {
    readonly code: string
    readonly message: string
}

export function readAcpErrorResponse(bc: bare.ByteCursor): AcpErrorResponse {
    return {
        code: bare.readString(bc),
        message: bare.readString(bc),
    }
}

export function writeAcpErrorResponse(bc: bare.ByteCursor, x: AcpErrorResponse): void {
    bare.writeString(bc, x.code)
    bare.writeString(bc, x.message)
}

/**
 * Browser RESUMABLE path: the create_session / session/prompt request (and each
 * AcpDeliverAgentOutputRequest that has not yet completed the handshake) returns
 * this, carrying the `processId` handle the kernel worker drives the interaction
 * with. The real result (AcpSessionCreatedResponse / AcpSessionRpcResponse) is
 * delivered as the response to the AcpDeliverAgentOutputRequest that completes it.
 */
export type AcpPendingResponse = {
    readonly processId: string
}

export function readAcpPendingResponse(bc: bare.ByteCursor): AcpPendingResponse {
    return {
        processId: bare.readString(bc),
    }
}

export function writeAcpPendingResponse(bc: bare.ByteCursor, x: AcpPendingResponse): void {
    bare.writeString(bc, x.processId)
}

export type AcpResponse =
    | { readonly tag: "AcpOpenSessionResponse"; readonly val: AcpOpenSessionResponse }
    | { readonly tag: "AcpGetDurableSessionResponse"; readonly val: AcpGetDurableSessionResponse }
    | { readonly tag: "AcpListDurableSessionsResponse"; readonly val: AcpListDurableSessionsResponse }
    | { readonly tag: "AcpDeleteSessionResponse"; readonly val: AcpDeleteSessionResponse }
    | { readonly tag: "AcpUnloadSessionResponse"; readonly val: AcpUnloadSessionResponse }
    | { readonly tag: "AcpPromptResponse"; readonly val: AcpPromptResponse }
    | { readonly tag: "AcpCancelPromptResponse"; readonly val: AcpCancelPromptResponse }
    | { readonly tag: "AcpRespondPermissionResponse"; readonly val: AcpRespondPermissionResponse }
    | { readonly tag: "AcpHistoryPageResponse"; readonly val: AcpHistoryPageResponse }
    | { readonly tag: "AcpSessionConfigResponse"; readonly val: AcpSessionConfigResponse }
    | { readonly tag: "AcpSessionCapabilitiesResponse"; readonly val: AcpSessionCapabilitiesResponse }
    | { readonly tag: "AcpSessionAgentInfoResponse"; readonly val: AcpSessionAgentInfoResponse }
    | { readonly tag: "AcpSessionCreatedResponse"; readonly val: AcpSessionCreatedResponse }
    | { readonly tag: "AcpSessionRpcResponse"; readonly val: AcpSessionRpcResponse }
    | { readonly tag: "AcpSessionStateResponse"; readonly val: AcpSessionStateResponse }
    | { readonly tag: "AcpSessionClosedResponse"; readonly val: AcpSessionClosedResponse }
    | { readonly tag: "AcpSessionResumedResponse"; readonly val: AcpSessionResumedResponse }
    | { readonly tag: "AcpErrorResponse"; readonly val: AcpErrorResponse }
    | { readonly tag: "AcpPendingResponse"; readonly val: AcpPendingResponse }
    | { readonly tag: "AcpListAgentsResponse"; readonly val: AcpListAgentsResponse }

export function readAcpResponse(bc: bare.ByteCursor): AcpResponse {
    const offset = bc.offset
    const tag = bare.readU8(bc)
    switch (tag) {
        case 0:
            return { tag: "AcpOpenSessionResponse", val: readAcpOpenSessionResponse(bc) }
        case 1:
            return { tag: "AcpGetDurableSessionResponse", val: readAcpGetDurableSessionResponse(bc) }
        case 2:
            return { tag: "AcpListDurableSessionsResponse", val: readAcpListDurableSessionsResponse(bc) }
        case 3:
            return { tag: "AcpDeleteSessionResponse", val: readAcpDeleteSessionResponse(bc) }
        case 4:
            return { tag: "AcpUnloadSessionResponse", val: readAcpUnloadSessionResponse(bc) }
        case 5:
            return { tag: "AcpPromptResponse", val: readAcpPromptResponse(bc) }
        case 6:
            return { tag: "AcpCancelPromptResponse", val: readAcpCancelPromptResponse(bc) }
        case 7:
            return { tag: "AcpRespondPermissionResponse", val: readAcpRespondPermissionResponse(bc) }
        case 8:
            return { tag: "AcpHistoryPageResponse", val: readAcpHistoryPageResponse(bc) }
        case 9:
            return { tag: "AcpSessionConfigResponse", val: readAcpSessionConfigResponse(bc) }
        case 10:
            return { tag: "AcpSessionCapabilitiesResponse", val: readAcpSessionCapabilitiesResponse(bc) }
        case 11:
            return { tag: "AcpSessionAgentInfoResponse", val: readAcpSessionAgentInfoResponse(bc) }
        case 12:
            return { tag: "AcpSessionCreatedResponse", val: readAcpSessionCreatedResponse(bc) }
        case 13:
            return { tag: "AcpSessionRpcResponse", val: readAcpSessionRpcResponse(bc) }
        case 14:
            return { tag: "AcpSessionStateResponse", val: readAcpSessionStateResponse(bc) }
        case 15:
            return { tag: "AcpSessionClosedResponse", val: readAcpSessionClosedResponse(bc) }
        case 16:
            return { tag: "AcpSessionResumedResponse", val: readAcpSessionResumedResponse(bc) }
        case 17:
            return { tag: "AcpErrorResponse", val: readAcpErrorResponse(bc) }
        case 18:
            return { tag: "AcpPendingResponse", val: readAcpPendingResponse(bc) }
        case 19:
            return { tag: "AcpListAgentsResponse", val: readAcpListAgentsResponse(bc) }
        default: {
            bc.offset = offset
            throw new bare.BareError(offset, "invalid tag")
        }
    }
}

export function writeAcpResponse(bc: bare.ByteCursor, x: AcpResponse): void {
    switch (x.tag) {
        case "AcpOpenSessionResponse": {
            bare.writeU8(bc, 0)
            writeAcpOpenSessionResponse(bc, x.val)
            break
        }
        case "AcpGetDurableSessionResponse": {
            bare.writeU8(bc, 1)
            writeAcpGetDurableSessionResponse(bc, x.val)
            break
        }
        case "AcpListDurableSessionsResponse": {
            bare.writeU8(bc, 2)
            writeAcpListDurableSessionsResponse(bc, x.val)
            break
        }
        case "AcpDeleteSessionResponse": {
            bare.writeU8(bc, 3)
            writeAcpDeleteSessionResponse(bc, x.val)
            break
        }
        case "AcpUnloadSessionResponse": {
            bare.writeU8(bc, 4)
            writeAcpUnloadSessionResponse(bc, x.val)
            break
        }
        case "AcpPromptResponse": {
            bare.writeU8(bc, 5)
            writeAcpPromptResponse(bc, x.val)
            break
        }
        case "AcpCancelPromptResponse": {
            bare.writeU8(bc, 6)
            writeAcpCancelPromptResponse(bc, x.val)
            break
        }
        case "AcpRespondPermissionResponse": {
            bare.writeU8(bc, 7)
            writeAcpRespondPermissionResponse(bc, x.val)
            break
        }
        case "AcpHistoryPageResponse": {
            bare.writeU8(bc, 8)
            writeAcpHistoryPageResponse(bc, x.val)
            break
        }
        case "AcpSessionConfigResponse": {
            bare.writeU8(bc, 9)
            writeAcpSessionConfigResponse(bc, x.val)
            break
        }
        case "AcpSessionCapabilitiesResponse": {
            bare.writeU8(bc, 10)
            writeAcpSessionCapabilitiesResponse(bc, x.val)
            break
        }
        case "AcpSessionAgentInfoResponse": {
            bare.writeU8(bc, 11)
            writeAcpSessionAgentInfoResponse(bc, x.val)
            break
        }
        case "AcpSessionCreatedResponse": {
            bare.writeU8(bc, 12)
            writeAcpSessionCreatedResponse(bc, x.val)
            break
        }
        case "AcpSessionRpcResponse": {
            bare.writeU8(bc, 13)
            writeAcpSessionRpcResponse(bc, x.val)
            break
        }
        case "AcpSessionStateResponse": {
            bare.writeU8(bc, 14)
            writeAcpSessionStateResponse(bc, x.val)
            break
        }
        case "AcpSessionClosedResponse": {
            bare.writeU8(bc, 15)
            writeAcpSessionClosedResponse(bc, x.val)
            break
        }
        case "AcpSessionResumedResponse": {
            bare.writeU8(bc, 16)
            writeAcpSessionResumedResponse(bc, x.val)
            break
        }
        case "AcpErrorResponse": {
            bare.writeU8(bc, 17)
            writeAcpErrorResponse(bc, x.val)
            break
        }
        case "AcpPendingResponse": {
            bare.writeU8(bc, 18)
            writeAcpPendingResponse(bc, x.val)
            break
        }
        case "AcpListAgentsResponse": {
            bare.writeU8(bc, 19)
            writeAcpListAgentsResponse(bc, x.val)
            break
        }
    }
}

export function encodeAcpResponse(x: AcpResponse, config?: Partial<bare.Config>): Uint8Array {
    const fullConfig = config != null ? bare.Config(config) : DEFAULT_CONFIG
    const bc = new bare.ByteCursor(
        new Uint8Array(fullConfig.initialBufferLength),
        fullConfig,
    )
    writeAcpResponse(bc, x)
    return new Uint8Array(bc.view.buffer, bc.view.byteOffset, bc.offset)
}

export function decodeAcpResponse(bytes: Uint8Array): AcpResponse {
    const bc = new bare.ByteCursor(bytes, DEFAULT_CONFIG)
    const result = readAcpResponse(bc)
    if (bc.offset < bc.view.byteLength) {
        throw new bare.BareError(bc.offset, "remaining bytes")
    }
    return result
}

/**
 * Live delivery uses the identical durable union/history envelope. Consumers
 * reconnect by subscribing, fetching after their last sequence, then
 * deduplicating by (sessionId, sequence); there is no second replay protocol.
 */
export type AcpDurableSessionEvent = {
    readonly sessionId: string
    readonly sequence: u64
    readonly timestamp: string
    readonly event: AcpDurableEvent
}

export function readAcpDurableSessionEvent(bc: bare.ByteCursor): AcpDurableSessionEvent {
    return {
        sessionId: bare.readString(bc),
        sequence: bare.readU64(bc),
        timestamp: bare.readString(bc),
        event: readAcpDurableEvent(bc),
    }
}

export function writeAcpDurableSessionEvent(bc: bare.ByteCursor, x: AcpDurableSessionEvent): void {
    bare.writeString(bc, x.sessionId)
    bare.writeU64(bc, x.sequence)
    bare.writeString(bc, x.timestamp)
    writeAcpDurableEvent(bc, x.event)
}

/**
 * Ephemeral entries are live-only ACP agent_message_chunk or
 * agent_thought_chunk updates. They are never persisted or sequenced; the
 * durable completed message is authoritative after reconnect.
 */
export type AcpEphemeralSessionUpdateEvent = {
    readonly sessionId: string
    readonly afterSequence: u64
    readonly update: JsonUtf8
}

export function readAcpEphemeralSessionUpdateEvent(bc: bare.ByteCursor): AcpEphemeralSessionUpdateEvent {
    return {
        sessionId: bare.readString(bc),
        afterSequence: bare.readU64(bc),
        update: readJsonUtf8(bc),
    }
}

export function writeAcpEphemeralSessionUpdateEvent(bc: bare.ByteCursor, x: AcpEphemeralSessionUpdateEvent): void {
    bare.writeString(bc, x.sessionId)
    bare.writeU64(bc, x.afterSequence)
    writeJsonUtf8(bc, x.update)
}

/**
 * Legacy browser-reference live event. Native durable clients ignore it and
 * consume AcpDurableSessionEvent/AcpEphemeralSessionUpdateEvent instead.
 */
export type AcpSessionEvent = {
    readonly sessionId: string
    readonly notification: JsonUtf8
}

export function readAcpSessionEvent(bc: bare.ByteCursor): AcpSessionEvent {
    return {
        sessionId: bare.readString(bc),
        notification: readJsonUtf8(bc),
    }
}

export function writeAcpSessionEvent(bc: bare.ByteCursor, x: AcpSessionEvent): void {
    bare.writeString(bc, x.sessionId)
    writeJsonUtf8(bc, x.notification)
}

export type AcpAgentStderrEvent = {
    readonly sessionId: string
    readonly agentType: string
    readonly processId: string
    readonly chunk: ArrayBuffer
}

export function readAcpAgentStderrEvent(bc: bare.ByteCursor): AcpAgentStderrEvent {
    return {
        sessionId: bare.readString(bc),
        agentType: bare.readString(bc),
        processId: bare.readString(bc),
        chunk: bare.readData(bc),
    }
}

export function writeAcpAgentStderrEvent(bc: bare.ByteCursor, x: AcpAgentStderrEvent): void {
    bare.writeString(bc, x.sessionId)
    bare.writeString(bc, x.agentType)
    bare.writeString(bc, x.processId)
    bare.writeData(bc, x.chunk)
}

/**
 * Emitted when the ACP adapter process exits unexpectedly — a crash from the
 * host's perspective (any spontaneous exit, including code 0).
 * `restart` is "not_attempted": the sidecar never respawns an adapter or
 * replays an interrupted request implicitly. `restartCount` and `maxRestarts`
 * are therefore both zero. Explicit session restoration is a separate caller
 * operation.
 * `exitCode` is absent when the exit was observed indirectly (e.g. a write to
 * the adapter's stdin failed because the process was already gone).
 * `pid` is the host pid reported when the adapter process was launched.
 */
export type AcpAgentExitedEvent = {
    readonly sessionId: string
    readonly agentType: string
    readonly processId: string
    readonly pid: u32 | null
    readonly exitCode: i32 | null
    readonly restart: string
    readonly restartCount: u32
    readonly maxRestarts: u32
}

export function readAcpAgentExitedEvent(bc: bare.ByteCursor): AcpAgentExitedEvent {
    return {
        sessionId: bare.readString(bc),
        agentType: bare.readString(bc),
        processId: bare.readString(bc),
        pid: read6(bc),
        exitCode: read11(bc),
        restart: bare.readString(bc),
        restartCount: bare.readU32(bc),
        maxRestarts: bare.readU32(bc),
    }
}

export function writeAcpAgentExitedEvent(bc: bare.ByteCursor, x: AcpAgentExitedEvent): void {
    bare.writeString(bc, x.sessionId)
    bare.writeString(bc, x.agentType)
    bare.writeString(bc, x.processId)
    write6(bc, x.pid)
    write11(bc, x.exitCode)
    bare.writeString(bc, x.restart)
    bare.writeU32(bc, x.restartCount)
    bare.writeU32(bc, x.maxRestarts)
}

export type AcpEvent =
    | { readonly tag: "AcpDurableSessionEvent"; readonly val: AcpDurableSessionEvent }
    | { readonly tag: "AcpEphemeralSessionUpdateEvent"; readonly val: AcpEphemeralSessionUpdateEvent }
    | { readonly tag: "AcpSessionEvent"; readonly val: AcpSessionEvent }
    | { readonly tag: "AcpAgentStderrEvent"; readonly val: AcpAgentStderrEvent }
    | { readonly tag: "AcpAgentExitedEvent"; readonly val: AcpAgentExitedEvent }

export function readAcpEvent(bc: bare.ByteCursor): AcpEvent {
    const offset = bc.offset
    const tag = bare.readU8(bc)
    switch (tag) {
        case 0:
            return { tag: "AcpDurableSessionEvent", val: readAcpDurableSessionEvent(bc) }
        case 1:
            return { tag: "AcpEphemeralSessionUpdateEvent", val: readAcpEphemeralSessionUpdateEvent(bc) }
        case 2:
            return { tag: "AcpSessionEvent", val: readAcpSessionEvent(bc) }
        case 3:
            return { tag: "AcpAgentStderrEvent", val: readAcpAgentStderrEvent(bc) }
        case 4:
            return { tag: "AcpAgentExitedEvent", val: readAcpAgentExitedEvent(bc) }
        default: {
            bc.offset = offset
            throw new bare.BareError(offset, "invalid tag")
        }
    }
}

export function writeAcpEvent(bc: bare.ByteCursor, x: AcpEvent): void {
    switch (x.tag) {
        case "AcpDurableSessionEvent": {
            bare.writeU8(bc, 0)
            writeAcpDurableSessionEvent(bc, x.val)
            break
        }
        case "AcpEphemeralSessionUpdateEvent": {
            bare.writeU8(bc, 1)
            writeAcpEphemeralSessionUpdateEvent(bc, x.val)
            break
        }
        case "AcpSessionEvent": {
            bare.writeU8(bc, 2)
            writeAcpSessionEvent(bc, x.val)
            break
        }
        case "AcpAgentStderrEvent": {
            bare.writeU8(bc, 3)
            writeAcpAgentStderrEvent(bc, x.val)
            break
        }
        case "AcpAgentExitedEvent": {
            bare.writeU8(bc, 4)
            writeAcpAgentExitedEvent(bc, x.val)
            break
        }
    }
}

export function encodeAcpEvent(x: AcpEvent, config?: Partial<bare.Config>): Uint8Array {
    const fullConfig = config != null ? bare.Config(config) : DEFAULT_CONFIG
    const bc = new bare.ByteCursor(
        new Uint8Array(fullConfig.initialBufferLength),
        fullConfig,
    )
    writeAcpEvent(bc, x)
    return new Uint8Array(bc.view.buffer, bc.view.byteOffset, bc.offset)
}

export function decodeAcpEvent(bytes: Uint8Array): AcpEvent {
    const bc = new bare.ByteCursor(bytes, DEFAULT_CONFIG)
    const result = readAcpEvent(bc)
    if (bc.offset < bc.view.byteLength) {
        throw new bare.BareError(bc.offset, "remaining bytes")
    }
    return result
}

export type AcpHostRequestCallback = {
    readonly sessionId: string
    readonly request: JsonUtf8
}

export function readAcpHostRequestCallback(bc: bare.ByteCursor): AcpHostRequestCallback {
    return {
        sessionId: bare.readString(bc),
        request: readJsonUtf8(bc),
    }
}

export function writeAcpHostRequestCallback(bc: bare.ByteCursor, x: AcpHostRequestCallback): void {
    bare.writeString(bc, x.sessionId)
    writeJsonUtf8(bc, x.request)
}

export type AcpCallback =
    | { readonly tag: "AcpHostRequestCallback"; readonly val: AcpHostRequestCallback }

export function readAcpCallback(bc: bare.ByteCursor): AcpCallback {
    const offset = bc.offset
    const tag = bare.readU8(bc)
    switch (tag) {
        case 0:
            return { tag: "AcpHostRequestCallback", val: readAcpHostRequestCallback(bc) }
        default: {
            bc.offset = offset
            throw new bare.BareError(offset, "invalid tag")
        }
    }
}

export function writeAcpCallback(bc: bare.ByteCursor, x: AcpCallback): void {
    switch (x.tag) {
        case "AcpHostRequestCallback": {
            bare.writeU8(bc, 0)
            writeAcpHostRequestCallback(bc, x.val)
            break
        }
    }
}

export function encodeAcpCallback(x: AcpCallback, config?: Partial<bare.Config>): Uint8Array {
    const fullConfig = config != null ? bare.Config(config) : DEFAULT_CONFIG
    const bc = new bare.ByteCursor(
        new Uint8Array(fullConfig.initialBufferLength),
        fullConfig,
    )
    writeAcpCallback(bc, x)
    return new Uint8Array(bc.view.buffer, bc.view.byteOffset, bc.offset)
}

export function decodeAcpCallback(bytes: Uint8Array): AcpCallback {
    const bc = new bare.ByteCursor(bytes, DEFAULT_CONFIG)
    const result = readAcpCallback(bc)
    if (bc.offset < bc.view.byteLength) {
        throw new bare.BareError(bc.offset, "remaining bytes")
    }
    return result
}

export type AcpHostRequestCallbackResponse = {
    readonly response: JsonUtf8 | null
}

export function readAcpHostRequestCallbackResponse(bc: bare.ByteCursor): AcpHostRequestCallbackResponse {
    return {
        response: read3(bc),
    }
}

export function writeAcpHostRequestCallbackResponse(bc: bare.ByteCursor, x: AcpHostRequestCallbackResponse): void {
    write3(bc, x.response)
}

export type AcpCallbackResponse =
    | { readonly tag: "AcpHostRequestCallbackResponse"; readonly val: AcpHostRequestCallbackResponse }

export function readAcpCallbackResponse(bc: bare.ByteCursor): AcpCallbackResponse {
    const offset = bc.offset
    const tag = bare.readU8(bc)
    switch (tag) {
        case 0:
            return { tag: "AcpHostRequestCallbackResponse", val: readAcpHostRequestCallbackResponse(bc) }
        default: {
            bc.offset = offset
            throw new bare.BareError(offset, "invalid tag")
        }
    }
}

export function writeAcpCallbackResponse(bc: bare.ByteCursor, x: AcpCallbackResponse): void {
    switch (x.tag) {
        case "AcpHostRequestCallbackResponse": {
            bare.writeU8(bc, 0)
            writeAcpHostRequestCallbackResponse(bc, x.val)
            break
        }
    }
}

export function encodeAcpCallbackResponse(x: AcpCallbackResponse, config?: Partial<bare.Config>): Uint8Array {
    const fullConfig = config != null ? bare.Config(config) : DEFAULT_CONFIG
    const bc = new bare.ByteCursor(
        new Uint8Array(fullConfig.initialBufferLength),
        fullConfig,
    )
    writeAcpCallbackResponse(bc, x)
    return new Uint8Array(bc.view.buffer, bc.view.byteOffset, bc.offset)
}

export function decodeAcpCallbackResponse(bytes: Uint8Array): AcpCallbackResponse {
    const bc = new bare.ByteCursor(bytes, DEFAULT_CONFIG)
    const result = readAcpCallbackResponse(bc)
    if (bc.offset < bc.view.byteLength) {
        throw new bare.BareError(bc.offset, "remaining bytes")
    }
    return result
}
