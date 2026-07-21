import { exposeCustomGlobal } from "../global-exposure.js";
import { http2 } from "./http2.js";

function normalizeDnsLookupInvocation(hostname, options, callback) {
  let normalizedOptions = {};
  let done = callback;
  if (typeof options === "function") {
    done = options;
  } else if (typeof options === "number") {
    normalizedOptions = { family: options };
  } else if (options == null) {
    normalizedOptions = {};
  } else if (typeof options === "object") {
    normalizedOptions = { ...options };
  } else {
    throw new TypeError("dns.lookup options must be a number, object, or callback");
  }
  const family = normalizedOptions.family === 4 || normalizedOptions.family === 6 ? normalizedOptions.family : void 0;
  return {
    callback: done,
    options: {
      hostname: String(hostname),
      family,
      all: normalizedOptions.all === true
    }
  };
}

function createUnsupportedDnsError(subject) {
  const error = new Error(`${subject} is not supported by the agentos dns polyfill`);
  error.code = "ERR_NOT_IMPLEMENTED";
  return error;
}

function normalizeDnsResolveInvocation(methodName, hostname, rrtype, callback) {
  let type = rrtype;
  let done = callback;
  if (typeof rrtype === "function") {
    done = rrtype;
    type = void 0;
  }
  const normalizedType = String(type ?? "A").toUpperCase();
  if (![
    "A",
    "AAAA",
    "MX",
    "TXT",
    "SRV",
    "CNAME",
    "PTR",
    "NS",
    "SOA",
    "NAPTR",
    "CAA",
    "ANY"
  ].includes(normalizedType)) {
    throw createUnsupportedDnsError(`${methodName}(${normalizedType})`);
  }
  return {
    callback: done,
    options: {
      hostname: String(hostname),
      rrtype: normalizedType
    }
  };
}

function parseDnsLookupRecords(resultJson) {
  let parsed = resultJson;
  if (typeof parsed === "string") {
    parsed = JSON.parse(parsed);
  }
  if (parsed && typeof parsed === "object" && Array.isArray(parsed.records)) {
    parsed = parsed.records;
  } else if (parsed && typeof parsed === "object" && typeof parsed.address === "string") {
    parsed = [parsed];
  }
  if (!Array.isArray(parsed)) {
    return [];
  }
  return parsed.filter((record) => record && typeof record.address === "string").map((record) => ({
    address: record.address,
    family: record.family === 6 ? 6 : 4
  }));
}

function parseDnsResolveRecords(resultJson) {
  let parsed = resultJson;
  if (typeof parsed === "string") {
    parsed = JSON.parse(parsed);
  }
  return parsed;
}

function createInvalidDnsServersError(subject) {
  const error = new TypeError(`${subject} expects an array of non-empty server strings`);
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
}

function normalizeDnsServers(subject, servers) {
  if (!Array.isArray(servers)) {
    throw createInvalidDnsServersError(subject);
  }
  return servers.map((server) => {
    if (typeof server !== "string" || server.length === 0) {
      throw createInvalidDnsServersError(subject);
    }
    return server;
  });
}

function lookupDnsRecords(hostname, options, callback) {
  const invocation = normalizeDnsLookupInvocation(hostname, options, callback);
  return _networkDnsLookupRaw.apply(
    void 0,
    [invocation.options],
    { result: { promise: true } }
  ).then((resultJson) => {
    const records = parseDnsLookupRecords(resultJson);
    if (typeof invocation.callback === "function") {
      if (invocation.options.all) {
        invocation.callback(null, records);
      } else {
        const first = records[0] ?? {
          address: null,
          family: invocation.options.family ?? 0
        };
        invocation.callback(null, first.address, first.family);
      }
    }
    return invocation.options.all ? records : records[0] ?? {
      address: "",
      family: invocation.options.family ?? 0
    };
  });
}

function resolveDnsRecords(methodName, hostname, rrtype, callback) {
  const invocation = normalizeDnsResolveInvocation(methodName, hostname, rrtype, callback);
  return _networkDnsResolveRaw.apply(
    void 0,
    [invocation.options],
    { result: { promise: true } }
  ).then((resultJson) => {
    const records = parseDnsResolveRecords(resultJson);
    if (typeof invocation.callback === "function") {
      queueMicrotask(() => invocation.callback(null, records));
    }
    return records;
  }).catch((err) => {
    if (typeof invocation.callback === "function") {
      queueMicrotask(() => invocation.callback(err));
    }
    throw err;
  });
}

// Resolver instances keep guest-owned server lists for API compatibility.
// Queries still use the VM-wide kernel resolver until the sync RPC grows
// per-request nameserver overrides.
class AgentOsResolver {
  constructor() {
    this._servers = [];
  }
  cancel() {
  }
  getServers() {
    return this._servers.slice();
  }
  lookup(hostname, options, callback) {
    return lookupDnsRecords(hostname, options, callback);
  }
  resolve(hostname, rrtype, callback) {
    return resolveDnsRecords("dns.resolve", hostname, rrtype, callback);
  }
  resolve4(hostname, callback) {
    return resolveDnsRecords("dns.resolve4", hostname, "A", callback);
  }
  resolve6(hostname, callback) {
    return resolveDnsRecords("dns.resolve6", hostname, "AAAA", callback);
  }
  resolveAny(hostname, callback) {
    return resolveDnsRecords("dns.resolveAny", hostname, "ANY", callback);
  }
  resolveMx(hostname, callback) {
    return resolveDnsRecords("dns.resolveMx", hostname, "MX", callback);
  }
  resolveTxt(hostname, callback) {
    return resolveDnsRecords("dns.resolveTxt", hostname, "TXT", callback);
  }
  resolveSrv(hostname, callback) {
    return resolveDnsRecords("dns.resolveSrv", hostname, "SRV", callback);
  }
  resolveCname(hostname, callback) {
    return resolveDnsRecords("dns.resolveCname", hostname, "CNAME", callback);
  }
  resolvePtr(hostname, callback) {
    return resolveDnsRecords("dns.resolvePtr", hostname, "PTR", callback);
  }
  resolveNs(hostname, callback) {
    return resolveDnsRecords("dns.resolveNs", hostname, "NS", callback);
  }
  resolveSoa(hostname, callback) {
    return resolveDnsRecords("dns.resolveSoa", hostname, "SOA", callback);
  }
  resolveNaptr(hostname, callback) {
    return resolveDnsRecords("dns.resolveNaptr", hostname, "NAPTR", callback);
  }
  resolveCaa(hostname, callback) {
    return resolveDnsRecords("dns.resolveCaa", hostname, "CAA", callback);
  }
  setServers(servers) {
    this._servers = normalizeDnsServers("dns.Resolver.setServers", servers);
  }
}

class AgentOsPromisesResolver {
  constructor() {
    this._servers = [];
  }
  cancel() {
  }
  getServers() {
    return this._servers.slice();
  }
  lookup(hostname, options) {
    return lookupDnsRecords(hostname, options);
  }
  resolve(hostname, rrtype) {
    return resolveDnsRecords("dns.resolve", hostname, rrtype);
  }
  resolve4(hostname) {
    return resolveDnsRecords("dns.resolve4", hostname, "A");
  }
  resolve6(hostname) {
    return resolveDnsRecords("dns.resolve6", hostname, "AAAA");
  }
  resolveAny(hostname) {
    return resolveDnsRecords("dns.resolveAny", hostname, "ANY");
  }
  resolveMx(hostname) {
    return resolveDnsRecords("dns.resolveMx", hostname, "MX");
  }
  resolveTxt(hostname) {
    return resolveDnsRecords("dns.resolveTxt", hostname, "TXT");
  }
  resolveSrv(hostname) {
    return resolveDnsRecords("dns.resolveSrv", hostname, "SRV");
  }
  resolveCname(hostname) {
    return resolveDnsRecords("dns.resolveCname", hostname, "CNAME");
  }
  resolvePtr(hostname) {
    return resolveDnsRecords("dns.resolvePtr", hostname, "PTR");
  }
  resolveNs(hostname) {
    return resolveDnsRecords("dns.resolveNs", hostname, "NS");
  }
  resolveSoa(hostname) {
    return resolveDnsRecords("dns.resolveSoa", hostname, "SOA");
  }
  resolveNaptr(hostname) {
    return resolveDnsRecords("dns.resolveNaptr", hostname, "NAPTR");
  }
  resolveCaa(hostname) {
    return resolveDnsRecords("dns.resolveCaa", hostname, "CAA");
  }
  setServers(servers) {
    this._servers = normalizeDnsServers("dns.promises.Resolver.setServers", servers);
  }
}

var dns = {
  ADDRCONFIG: 32,
  ALL: 16,
  V4MAPPED: 8,
  lookup(hostname, options, callback) {
    lookupDnsRecords(hostname, options, callback).catch((err) => {
      const done = typeof options === "function" ? options : callback;
      done?.(err);
    });
  },
  resolve(hostname, rrtype, callback) {
    resolveDnsRecords("dns.resolve", hostname, rrtype, callback).catch(() => {
    });
  },
  resolve4(hostname, callback) {
    resolveDnsRecords("dns.resolve4", hostname, "A", callback).catch(() => {
    });
  },
  resolve6(hostname, callback) {
    resolveDnsRecords("dns.resolve6", hostname, "AAAA", callback).catch(() => {
    });
  },
  resolveAny(hostname, callback) {
    resolveDnsRecords("dns.resolveAny", hostname, "ANY", callback).catch(() => {
    });
  },
  resolveMx(hostname, callback) {
    resolveDnsRecords("dns.resolveMx", hostname, "MX", callback).catch(() => {
    });
  },
  resolveTxt(hostname, callback) {
    resolveDnsRecords("dns.resolveTxt", hostname, "TXT", callback).catch(() => {
    });
  },
  resolveSrv(hostname, callback) {
    resolveDnsRecords("dns.resolveSrv", hostname, "SRV", callback).catch(() => {
    });
  },
  resolveCname(hostname, callback) {
    resolveDnsRecords("dns.resolveCname", hostname, "CNAME", callback).catch(() => {
    });
  },
  resolvePtr(hostname, callback) {
    resolveDnsRecords("dns.resolvePtr", hostname, "PTR", callback).catch(() => {
    });
  },
  resolveNs(hostname, callback) {
    resolveDnsRecords("dns.resolveNs", hostname, "NS", callback).catch(() => {
    });
  },
  resolveSoa(hostname, callback) {
    resolveDnsRecords("dns.resolveSoa", hostname, "SOA", callback).catch(() => {
    });
  },
  resolveNaptr(hostname, callback) {
    resolveDnsRecords("dns.resolveNaptr", hostname, "NAPTR", callback).catch(() => {
    });
  },
  resolveCaa(hostname, callback) {
    resolveDnsRecords("dns.resolveCaa", hostname, "CAA", callback).catch(() => {
    });
  },
  promises: {
    Resolver: AgentOsPromisesResolver,
    lookup(hostname, _options) {
      return lookupDnsRecords(hostname, _options);
    },
    resolve(hostname, rrtype) {
      return resolveDnsRecords("dns.resolve", hostname, rrtype || "A");
    },
    resolve4(hostname) {
      return resolveDnsRecords("dns.resolve4", hostname, "A");
    },
    resolve6(hostname) {
      return resolveDnsRecords("dns.resolve6", hostname, "AAAA");
    },
    resolveAny(hostname) {
      return resolveDnsRecords("dns.resolveAny", hostname, "ANY");
    },
    resolveMx(hostname) {
      return resolveDnsRecords("dns.resolveMx", hostname, "MX");
    },
    resolveTxt(hostname) {
      return resolveDnsRecords("dns.resolveTxt", hostname, "TXT");
    },
    resolveSrv(hostname) {
      return resolveDnsRecords("dns.resolveSrv", hostname, "SRV");
    },
    resolveCname(hostname) {
      return resolveDnsRecords("dns.resolveCname", hostname, "CNAME");
    },
    resolvePtr(hostname) {
      return resolveDnsRecords("dns.resolvePtr", hostname, "PTR");
    },
    resolveNs(hostname) {
      return resolveDnsRecords("dns.resolveNs", hostname, "NS");
    },
    resolveSoa(hostname) {
      return resolveDnsRecords("dns.resolveSoa", hostname, "SOA");
    },
    resolveNaptr(hostname) {
      return resolveDnsRecords("dns.resolveNaptr", hostname, "NAPTR");
    },
    resolveCaa(hostname) {
      return resolveDnsRecords("dns.resolveCaa", hostname, "CAA");
    }
  },
  Resolver: AgentOsResolver,
  getServers() {
    return [];
  },
  lookupService() {
    throw createUnsupportedDnsError("dns.lookupService");
  },
  reverse() {
    throw createUnsupportedDnsError("dns.reverse");
  },
  setServers() {
    throw createUnsupportedDnsError("dns.setServers");
  }
};

exposeCustomGlobal("_http2Module", http2);
export { AgentOsPromisesResolver, AgentOsResolver, createInvalidDnsServersError, createUnsupportedDnsError, dns, lookupDnsRecords, normalizeDnsLookupInvocation, normalizeDnsResolveInvocation, normalizeDnsServers, parseDnsLookupRecords, parseDnsResolveRecords, resolveDnsRecords };
