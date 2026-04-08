// @ts-ignore whatwg-url ships without bundled TypeScript declarations in this repo.
import {
	URL as WhatwgURL,
	URLSearchParams as WhatwgURLSearchParams,
} from "whatwg-url";

type WhatwgURLInstance = InstanceType<typeof WhatwgURL>;
type WhatwgURLSearchParamsInstance = InstanceType<typeof WhatwgURLSearchParams>;

const inspectCustomSymbol = Symbol.for("nodejs.util.inspect.custom");
const toStringTagSymbol = Symbol.toStringTag;
const ERR_INVALID_THIS = "ERR_INVALID_THIS";
const ERR_MISSING_ARGS = "ERR_MISSING_ARGS";
const ERR_INVALID_URL = "ERR_INVALID_URL";
const ERR_ARG_NOT_ITERABLE = "ERR_ARG_NOT_ITERABLE";
const ERR_INVALID_TUPLE = "ERR_INVALID_TUPLE";
const URL_SEARCH_PARAMS_TYPE = "URLSearchParams";
const kLinkedSearchParams = Symbol("secureExecLinkedURLSearchParams");
const kBlobUrlStore = Symbol.for("secureExec.blobUrlStore");
const kBlobUrlCounter = Symbol.for("secureExec.blobUrlCounter");
const SEARCH_PARAM_METHOD_NAMES = ["append", "delete", "get", "getAll", "has"];
const SEARCH_PARAM_PAIR_METHOD_NAMES = ["append", "set"];
const URL_SCHEME_TYPES: Record<string, number> = {
	"http:": 0,
	"https:": 2,
	"ws:": 4,
	"wss:": 5,
	"file:": 6,
	"ftp:": 8,
};

type SearchParamsLinkedInit = {
	[kLinkedSearchParams]: () => WhatwgURLSearchParamsInstance;
};

const searchParamsBrand = new WeakSet<URLSearchParams>();
const searchParamsState = new WeakMap<
	URLSearchParams,
	{ getImpl: () => WhatwgURLSearchParamsInstance }
>();
const searchParamsIteratorBrand = new WeakSet<URLSearchParamsIterator>();
const searchParamsIteratorState = new WeakMap<
	URLSearchParamsIterator,
	{ values: unknown[]; index: number }
>();

function createNodeTypeError(message: string, code: string): TypeError & { code: string } {
	const error = new TypeError(message) as TypeError & { code: string };
	error.code = code;
	return error;
}

function createInvalidUrlError(): TypeError & { code: string } {
	const error = new TypeError("Invalid URL") as TypeError & { code: string };
	error.code = ERR_INVALID_URL;
	return error;
}

function createUrlReceiverTypeError(): TypeError {
	return new TypeError("Receiver must be an instance of class URL");
}

function createMissingArgsError(message: string): TypeError & { code: string } {
	return createNodeTypeError(message, ERR_MISSING_ARGS);
}

function createIterableTypeError(): TypeError & { code: string } {
	return createNodeTypeError("Query pairs must be iterable", ERR_ARG_NOT_ITERABLE);
}

function createTupleTypeError(): TypeError & { code: string } {
	return createNodeTypeError(
		"Each query pair must be an iterable [name, value] tuple",
		ERR_INVALID_TUPLE,
	);
}

function createSymbolStringError(): TypeError {
	return new TypeError("Cannot convert a Symbol value to a string");
}

function toNodeString(value: unknown): string {
	if (typeof value === "symbol") {
		throw createSymbolStringError();
	}
	return String(value);
}

function toWellFormedString(value: string): string {
	let result = "";
	for (let index = 0; index < value.length; index += 1) {
		const codeUnit = value.charCodeAt(index);
		if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
			const nextIndex = index + 1;
			if (nextIndex < value.length) {
				const nextCodeUnit = value.charCodeAt(nextIndex);
				if (nextCodeUnit >= 0xdc00 && nextCodeUnit <= 0xdfff) {
					result += value[index] + value[nextIndex];
					index = nextIndex;
					continue;
				}
			}
			result += "\uFFFD";
			continue;
		}
		if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) {
			result += "\uFFFD";
			continue;
		}
		result += value[index];
	}
	return result;
}

function toNodeUSVString(value: unknown): string {
	return toWellFormedString(toNodeString(value));
}

function assertUrlSearchParamsReceiver(receiver: unknown): asserts receiver is URLSearchParams {
	if (!searchParamsBrand.has(receiver as URLSearchParams)) {
		throw createNodeTypeError(
			'Value of "this" must be of type URLSearchParams',
			ERR_INVALID_THIS,
		);
	}
}

function assertUrlSearchParamsIteratorReceiver(
	receiver: unknown,
): asserts receiver is URLSearchParamsIterator {
	if (!searchParamsIteratorBrand.has(receiver as URLSearchParamsIterator)) {
		throw createNodeTypeError(
			'Value of "this" must be of type URLSearchParamsIterator',
			ERR_INVALID_THIS,
		);
	}
}

function getUrlSearchParamsImpl(receiver: URLSearchParams): WhatwgURLSearchParamsInstance {
	const state = searchParamsState.get(receiver);
	if (!state) {
		throw createNodeTypeError(
			'Value of "this" must be of type URLSearchParams',
			ERR_INVALID_THIS,
		);
	}
	return state.getImpl();
}

function countSearchParams(params: WhatwgURLSearchParamsInstance): number {
	let count = 0;
	for (const _entry of params) {
		count++;
	}
	return count;
}

function normalizeSearchParamsInit(
	init: unknown,
): string | Array<[string, string]> | SearchParamsLinkedInit | undefined {
	if (
		init &&
		typeof init === "object" &&
		kLinkedSearchParams in (init as Record<PropertyKey, unknown>)
	) {
		return init as SearchParamsLinkedInit;
	}

	if (init == null) {
		return undefined;
	}

	if (typeof init === "string") {
		return toNodeUSVString(init);
	}

	if (typeof init === "object" || typeof init === "function") {
		const iterator = (init as { [Symbol.iterator]?: unknown })[Symbol.iterator];
		if (iterator !== undefined) {
			if (typeof iterator !== "function") {
				throw createIterableTypeError();
			}

			const pairs: Array<[string, string]> = [];
			for (const pair of init as Iterable<unknown>) {
				if (pair == null) {
					throw createTupleTypeError();
				}

				const pairIterator = (pair as { [Symbol.iterator]?: unknown })[Symbol.iterator];
				if (typeof pairIterator !== "function") {
					throw createTupleTypeError();
				}

				const values = Array.from(pair as Iterable<unknown>);
				if (values.length !== 2) {
					throw createTupleTypeError();
				}

				pairs.push([toNodeUSVString(values[0]), toNodeUSVString(values[1])]);
			}
			return pairs;
		}

		const pairs: Array<[string, string]> = [];
		for (const key of Reflect.ownKeys(init as object)) {
			if (!Object.prototype.propertyIsEnumerable.call(init, key)) {
				continue;
			}
			pairs.push([
				toNodeUSVString(key),
				toNodeUSVString((init as Record<PropertyKey, unknown>)[key]),
			]);
		}
		return pairs;
	}

	return toNodeUSVString(init);
}

function createStandaloneSearchParams(
	init?: string | Array<[string, string]>,
): WhatwgURLSearchParamsInstance {
	if (typeof init === "string") {
		return new WhatwgURLSearchParams(init);
	}
	return init === undefined
		? new WhatwgURLSearchParams()
		: new WhatwgURLSearchParams(init);
}

function createCollectionBody(
	items: string[],
	options: { breakLength?: number } | undefined,
	emptyBody: "{}" | "{  }",
): string {
	if (items.length === 0) {
		return emptyBody;
	}

	const oneLine = `{ ${items.join(", ")} }`;
	const breakLength = options?.breakLength ?? Infinity;
	if (oneLine.length <= breakLength) {
		return oneLine;
	}
	return `{\n  ${items.join(",\n  ")} }`;
}

function createUrlContext(url: URL) {
	const href = url.href;
	const protocolEnd = href.indexOf(":") + 1;
	const authIndex = href.indexOf("@");
	const pathnameStart = href.indexOf("/", protocolEnd + 2);
	const searchStart = href.indexOf("?");
	const hashStart = href.indexOf("#");
	const usernameEnd =
		url.username.length > 0
			? href.indexOf(":", protocolEnd + 2)
			: protocolEnd + 2;
	const hostStart = authIndex === -1 ? protocolEnd + 2 : authIndex;
	const hostEnd =
		pathnameStart === -1
			? href.length
			: pathnameStart - (url.port.length > 0 ? url.port.length + 1 : 0);
	const port = url.port.length > 0 ? Number(url.port) : null;

	return {
		href,
		protocol_end: protocolEnd,
		username_end: usernameEnd,
		host_start: hostStart,
		host_end: hostEnd,
		pathname_start: pathnameStart === -1 ? href.length : pathnameStart,
		search_start: searchStart === -1 ? href.length : searchStart,
		hash_start: hashStart === -1 ? href.length : hashStart,
		port,
		scheme_type: URL_SCHEME_TYPES[url.protocol] ?? 1,
		hasPort: url.port.length > 0,
		hasSearch: url.search.length > 0,
		hasHash: url.hash.length > 0,
	};
}

function formatUrlContext(
	url: URL,
	inspect: ((value: unknown, options?: unknown) => string) | undefined,
	options: unknown,
): string {
	const context = createUrlContext(url);
	const formatValue =
		typeof inspect === "function"
			? (value: unknown) => inspect(value, options)
			: (value: unknown) => JSON.stringify(value);
	const portValue = context.port === null ? "null" : String(context.port);

	return [
		"URLContext {",
		`    href: ${formatValue(context.href)},`,
		`    protocol_end: ${context.protocol_end},`,
		`    username_end: ${context.username_end},`,
		`    host_start: ${context.host_start},`,
		`    host_end: ${context.host_end},`,
		`    pathname_start: ${context.pathname_start},`,
		`    search_start: ${context.search_start},`,
		`    hash_start: ${context.hash_start},`,
		`    port: ${portValue},`,
		`    scheme_type: ${context.scheme_type},`,
		"    [hasPort]: [Getter],",
		"    [hasSearch]: [Getter],",
		"    [hasHash]: [Getter]",
		"  }",
	].join("\n");
}

function getBlobUrlStore(): Map<string, unknown> {
	const globalRecord = globalThis as Record<PropertyKey, unknown>;
	const existing = globalRecord[kBlobUrlStore];
	if (existing instanceof Map) {
		return existing;
	}
	const store = new Map<string, unknown>();
	globalRecord[kBlobUrlStore] = store;
	return store;
}

function nextBlobUrlId(): number {
	const globalRecord = globalThis as Record<PropertyKey, unknown>;
	const nextId = typeof globalRecord[kBlobUrlCounter] === "number" ? globalRecord[kBlobUrlCounter] : 1;
	globalRecord[kBlobUrlCounter] = (nextId as number) + 1;
	return nextId as number;
}

export class URLSearchParamsIterator {
	constructor(values: unknown[]) {
		searchParamsIteratorBrand.add(this);
		searchParamsIteratorState.set(this, { values, index: 0 });
	}

	next(): IteratorResult<unknown> {
		assertUrlSearchParamsIteratorReceiver(this);
		const state = searchParamsIteratorState.get(this)!;
		if (state.index >= state.values.length) {
			return { value: undefined, done: true };
		}
		const value = state.values[state.index];
		state.index += 1;
		return { value, done: false };
	}

	[inspectCustomSymbol](
		depth: number,
		options?: { breakLength?: number },
		inspect?: (value: unknown, options?: unknown) => string,
	): string {
		assertUrlSearchParamsIteratorReceiver(this);
		if (depth < 0) {
			return "[Object]";
		}
		const state = searchParamsIteratorState.get(this)!;
		const formatValue =
			typeof inspect === "function"
				? (value: unknown) => inspect(value, options)
				: (value: unknown) => JSON.stringify(value);
		const remaining = state.values.slice(state.index).map((value) => formatValue(value));
		return `URLSearchParams Iterator ${createCollectionBody(remaining, options, "{  }")}`;
	}

	get [toStringTagSymbol](): string {
		if (this !== URLSearchParamsIterator.prototype) {
			assertUrlSearchParamsIteratorReceiver(this);
		}
		return "URLSearchParams Iterator";
	}
}

Object.defineProperties(URLSearchParamsIterator.prototype, {
	next: {
		value: URLSearchParamsIterator.prototype.next,
		writable: true,
		configurable: true,
		enumerable: true,
	},
	[Symbol.iterator]: {
		value: function iterator(this: URLSearchParamsIterator) {
			assertUrlSearchParamsIteratorReceiver(this);
			return this;
		},
		writable: true,
		configurable: true,
		enumerable: false,
	},
	[inspectCustomSymbol]: {
		value: URLSearchParamsIterator.prototype[inspectCustomSymbol],
		writable: true,
		configurable: true,
		enumerable: false,
	},
	[toStringTagSymbol]: {
		get: Object.getOwnPropertyDescriptor(URLSearchParamsIterator.prototype, toStringTagSymbol)?.get,
		configurable: true,
		enumerable: false,
	},
});

Object.defineProperty(
	Object.getOwnPropertyDescriptor(URLSearchParamsIterator.prototype, Symbol.iterator)?.value,
	"name",
	{
		value: "entries",
		configurable: true,
	},
);

export class URLSearchParams {
	constructor(init?: unknown) {
		searchParamsBrand.add(this);
		const normalized = normalizeSearchParamsInit(init);
		if (
			normalized &&
			typeof normalized === "object" &&
			kLinkedSearchParams in normalized
		) {
			searchParamsState.set(this, {
				getImpl: (normalized as SearchParamsLinkedInit)[kLinkedSearchParams],
			});
			return;
		}
		const impl = createStandaloneSearchParams(
			normalized as string | Array<[string, string]> | undefined,
		);
		searchParamsState.set(this, { getImpl: () => impl });
	}

	append(name?: unknown, value?: unknown): void {
		assertUrlSearchParamsReceiver(this);
		if (arguments.length < 2) {
			throw createMissingArgsError('The "name" and "value" arguments must be specified');
		}
		getUrlSearchParamsImpl(this).append(toNodeUSVString(name), toNodeUSVString(value));
	}

	delete(name?: unknown): void {
		assertUrlSearchParamsReceiver(this);
		if (arguments.length < 1) {
			throw createMissingArgsError('The "name" argument must be specified');
		}
		getUrlSearchParamsImpl(this).delete(toNodeUSVString(name));
	}

	get(name?: unknown): string | null {
		assertUrlSearchParamsReceiver(this);
		if (arguments.length < 1) {
			throw createMissingArgsError('The "name" argument must be specified');
		}
		return getUrlSearchParamsImpl(this).get(toNodeUSVString(name));
	}

	getAll(name?: unknown): string[] {
		assertUrlSearchParamsReceiver(this);
		if (arguments.length < 1) {
			throw createMissingArgsError('The "name" argument must be specified');
		}
		return getUrlSearchParamsImpl(this).getAll(toNodeUSVString(name));
	}

	has(name?: unknown): boolean {
		assertUrlSearchParamsReceiver(this);
		if (arguments.length < 1) {
			throw createMissingArgsError('The "name" argument must be specified');
		}
		return getUrlSearchParamsImpl(this).has(toNodeUSVString(name));
	}

	set(name?: unknown, value?: unknown): void {
		assertUrlSearchParamsReceiver(this);
		if (arguments.length < 2) {
			throw createMissingArgsError('The "name" and "value" arguments must be specified');
		}
		getUrlSearchParamsImpl(this).set(toNodeUSVString(name), toNodeUSVString(value));
	}

	sort(): void {
		assertUrlSearchParamsReceiver(this);
		getUrlSearchParamsImpl(this).sort();
	}

	entries(): URLSearchParamsIterator {
		assertUrlSearchParamsReceiver(this);
		return new URLSearchParamsIterator(Array.from(getUrlSearchParamsImpl(this)));
	}

	keys(): URLSearchParamsIterator {
		assertUrlSearchParamsReceiver(this);
		return new URLSearchParamsIterator(Array.from(getUrlSearchParamsImpl(this).keys()));
	}

	values(): URLSearchParamsIterator {
		assertUrlSearchParamsReceiver(this);
		return new URLSearchParamsIterator(Array.from(getUrlSearchParamsImpl(this).values()));
	}

	forEach(
		callback?: (value: string, key: string, obj: URLSearchParams) => void,
		thisArg?: unknown,
	): void {
		assertUrlSearchParamsReceiver(this);
		if (typeof callback !== "function") {
			throw createNodeTypeError(
				'The "callback" argument must be of type function. Received ' +
					(callback === undefined ? "undefined" : typeof callback),
				"ERR_INVALID_ARG_TYPE",
			);
		}

		for (const [key, value] of getUrlSearchParamsImpl(this)) {
			callback.call(thisArg, value, key, this);
		}
	}

	toString(): string {
		assertUrlSearchParamsReceiver(this);
		return getUrlSearchParamsImpl(this).toString();
	}

	get size(): number {
		assertUrlSearchParamsReceiver(this);
		return countSearchParams(getUrlSearchParamsImpl(this));
	}

	[inspectCustomSymbol](
		depth: number,
		options?: { breakLength?: number },
		inspect?: (value: unknown, options?: unknown) => string,
	): string {
		assertUrlSearchParamsReceiver(this);
		if (depth < 0) {
			return "[Object]";
		}
		const formatValue =
			typeof inspect === "function"
				? (value: unknown) => inspect(value, options)
				: (value: unknown) => JSON.stringify(value);
		const items = Array.from(
			getUrlSearchParamsImpl(this) as Iterable<[string, string]>,
		).map(
			([key, value]) => `${formatValue(key)} => ${formatValue(value)}`,
		);
		return `URLSearchParams ${createCollectionBody(items, options, "{}")}`;
	}

	get [toStringTagSymbol](): string {
		if (this !== URLSearchParams.prototype) {
			assertUrlSearchParamsReceiver(this);
		}
		return URL_SEARCH_PARAMS_TYPE;
	}
}

for (const name of SEARCH_PARAM_METHOD_NAMES) {
	Object.defineProperty(URLSearchParams.prototype, name, {
		value: (URLSearchParams.prototype as unknown as Record<string, unknown>)[name],
		writable: true,
		configurable: true,
		enumerable: true,
	});
}

for (const name of SEARCH_PARAM_PAIR_METHOD_NAMES) {
	Object.defineProperty(URLSearchParams.prototype, name, {
		value: (URLSearchParams.prototype as unknown as Record<string, unknown>)[name],
		writable: true,
		configurable: true,
		enumerable: true,
	});
}

for (const name of ["sort", "entries", "forEach", "keys", "values", "toString"] as const) {
	Object.defineProperty(URLSearchParams.prototype, name, {
		value: URLSearchParams.prototype[name],
		writable: true,
		configurable: true,
		enumerable: true,
	});
}

Object.defineProperties(URLSearchParams.prototype, {
	size: {
		get: Object.getOwnPropertyDescriptor(URLSearchParams.prototype, "size")?.get,
		configurable: true,
		enumerable: true,
	},
	[Symbol.iterator]: {
		value: URLSearchParams.prototype.entries,
		writable: true,
		configurable: true,
		enumerable: false,
	},
	[inspectCustomSymbol]: {
		value: URLSearchParams.prototype[inspectCustomSymbol],
		writable: true,
		configurable: true,
		enumerable: false,
	},
	[toStringTagSymbol]: {
		get: Object.getOwnPropertyDescriptor(URLSearchParams.prototype, toStringTagSymbol)?.get,
		configurable: true,
		enumerable: false,
	},
});

export class URL {
	#impl: WhatwgURLInstance;
	#searchParams: URLSearchParams | undefined;

	constructor(input?: unknown, base?: unknown) {
		if (arguments.length < 1) {
			throw createMissingArgsError('The "url" argument must be specified');
		}

		try {
			this.#impl =
				arguments.length >= 2
					? new WhatwgURL(toNodeUSVString(input), toNodeUSVString(base))
					: new WhatwgURL(toNodeUSVString(input));
		} catch {
			throw createInvalidUrlError();
		}
	}

	static canParse(input?: unknown, base?: unknown): boolean {
		if (arguments.length < 1) {
			throw createMissingArgsError('The "url" argument must be specified');
		}

		try {
			if (arguments.length >= 2) {
				new URL(input, base);
			} else {
				new URL(input);
			}
			return true;
		} catch {
			return false;
		}
	}

	static createObjectURL(obj?: unknown): string {
		if (
			typeof Blob === "undefined" ||
			!(obj instanceof Blob)
		) {
			throw createNodeTypeError(
				'The "obj" argument must be an instance of Blob. Received ' +
					(obj === null ? "null" : typeof obj),
				"ERR_INVALID_ARG_TYPE",
			);
		}
		const id = `blob:nodedata:${nextBlobUrlId()}`;
		getBlobUrlStore().set(id, obj);
		return id;
	}

	static revokeObjectURL(url?: unknown): void {
		if (arguments.length < 1) {
			throw createMissingArgsError('The "url" argument must be specified');
		}
		if (typeof url !== "string") {
			return;
		}
		getBlobUrlStore().delete(url);
	}

	get href(): string {
		if (!(this instanceof URL)) {
			throw createUrlReceiverTypeError();
		}
		return this.#impl.href;
	}

	set href(value: unknown) {
		this.#impl.href = toNodeUSVString(value);
	}

	get origin(): string {
		return this.#impl.origin;
	}

	get protocol(): string {
		return this.#impl.protocol;
	}

	set protocol(value: unknown) {
		this.#impl.protocol = toNodeUSVString(value);
	}

	get username(): string {
		return this.#impl.username;
	}

	set username(value: unknown) {
		this.#impl.username = toNodeUSVString(value);
	}

	get password(): string {
		return this.#impl.password;
	}

	set password(value: unknown) {
		this.#impl.password = toNodeUSVString(value);
	}

	get host(): string {
		return this.#impl.host;
	}

	set host(value: unknown) {
		this.#impl.host = toNodeUSVString(value);
	}

	get hostname(): string {
		return this.#impl.hostname;
	}

	set hostname(value: unknown) {
		this.#impl.hostname = toNodeUSVString(value);
	}

	get port(): string {
		return this.#impl.port;
	}

	set port(value: unknown) {
		this.#impl.port = toNodeUSVString(value);
	}

	get pathname(): string {
		return this.#impl.pathname;
	}

	set pathname(value: unknown) {
		this.#impl.pathname = toNodeUSVString(value);
	}

	get search(): string {
		if (!(this instanceof URL)) {
			throw createUrlReceiverTypeError();
		}
		return this.#impl.search;
	}

	set search(value: unknown) {
		this.#impl.search = toNodeUSVString(value);
	}

	get searchParams(): URLSearchParams {
		if (!this.#searchParams) {
			this.#searchParams = new URLSearchParams({
				[kLinkedSearchParams]: () => this.#impl.searchParams,
			});
		}
		return this.#searchParams;
	}

	get hash(): string {
		return this.#impl.hash;
	}

	set hash(value: unknown) {
		this.#impl.hash = toNodeUSVString(value);
	}

	toString(): string {
		if (!(this instanceof URL)) {
			throw createUrlReceiverTypeError();
		}
		return this.#impl.href;
	}

	toJSON(): string {
		if (!(this instanceof URL)) {
			throw createUrlReceiverTypeError();
		}
		return this.#impl.href;
	}

	[inspectCustomSymbol](
		depth: number,
		options?: { showHidden?: boolean },
		inspect?: (value: unknown, options?: unknown) => string,
	): string {
		const inspectName = this.constructor === URL ? "URL" : this.constructor.name;
		if (depth < 0) {
			return `${inspectName} {}`;
		}

		const formatValue =
			typeof inspect === "function"
				? (value: unknown) => inspect(value, options)
				: (value: unknown) => JSON.stringify(value);
		const lines = [
			`${inspectName} {`,
			`  href: ${formatValue(this.href)},`,
			`  origin: ${formatValue(this.origin)},`,
			`  protocol: ${formatValue(this.protocol)},`,
			`  username: ${formatValue(this.username)},`,
			`  password: ${formatValue(this.password)},`,
			`  host: ${formatValue(this.host)},`,
			`  hostname: ${formatValue(this.hostname)},`,
			`  port: ${formatValue(this.port)},`,
			`  pathname: ${formatValue(this.pathname)},`,
			`  search: ${formatValue(this.search)},`,
			`  searchParams: ${this.searchParams[inspectCustomSymbol](depth - 1, undefined, inspect)},`,
			`  hash: ${formatValue(this.hash)}`,
		];

		if (options?.showHidden) {
			lines[lines.length - 1] += ",";
			lines.push(`  [Symbol(context)]: ${formatUrlContext(this, inspect, options)}`);
		}

		lines.push("}");
		return lines.join("\n");
	}

	get [toStringTagSymbol](): string {
		return "URL";
	}
}

for (const name of ["toString", "toJSON"] as const) {
	Object.defineProperty(URL.prototype, name, {
		value: URL.prototype[name],
		writable: true,
		configurable: true,
		enumerable: true,
	});
}

for (const name of [
	"href",
	"protocol",
	"username",
	"password",
	"host",
	"hostname",
	"port",
	"pathname",
	"search",
	"hash",
	"origin",
	"searchParams",
] as const) {
	const descriptor = Object.getOwnPropertyDescriptor(URL.prototype, name);
	if (!descriptor) {
		continue;
	}
	descriptor.enumerable = true;
	Object.defineProperty(URL.prototype, name, descriptor);
}

Object.defineProperties(URL.prototype, {
	[inspectCustomSymbol]: {
		value: URL.prototype[inspectCustomSymbol],
		writable: true,
		configurable: true,
		enumerable: false,
	},
	[toStringTagSymbol]: {
		get: Object.getOwnPropertyDescriptor(URL.prototype, toStringTagSymbol)?.get,
		configurable: true,
		enumerable: false,
	},
});

for (const name of ["canParse", "createObjectURL", "revokeObjectURL"] as const) {
	Object.defineProperty(URL, name, {
		value: URL[name],
		writable: true,
		configurable: true,
		enumerable: true,
	});
}

export function installWhatwgUrlGlobals(target: typeof globalThis = globalThis): void {
	Object.defineProperty(target, "URL", {
		value: URL,
		writable: true,
		configurable: true,
		enumerable: false,
	});
	Object.defineProperty(target, "URLSearchParams", {
		value: URLSearchParams,
		writable: true,
		configurable: true,
		enumerable: false,
	});
}
