"use strict";

const pino = require("pino");

// Use process.stdout as destination for sandbox compatibility
// Disable variable fields (timestamp, pid, hostname) for deterministic output
const logger = pino(
  {
    timestamp: false,
    base: undefined,
  },
  process.stdout
);

// Basic logging at different levels
logger.info("hello from pino");
logger.warn("this is a warning");
logger.error("something went wrong");

// Structured data
logger.info({ user: "alice", action: "login" }, "user event");

// Child logger with bound properties
const child = logger.child({ module: "auth" });
child.info("child logger message");
child.info({ detail: "extra" }, "child with data");

// Custom serializers
const custom = pino(
  {
    timestamp: false,
    base: undefined,
    serializers: {
      req: (val) => ({ method: val.method, url: val.url }),
    },
  },
  process.stdout
);
custom.info(
  { req: { method: "GET", url: "/api", headers: { host: "localhost" } } },
  "request received"
);

// Silent level (should not output)
const silent = pino(
  {
    timestamp: false,
    base: undefined,
    level: "error",
  },
  process.stdout
);
silent.info("this should not appear");
silent.error("only errors visible");

// Log levels are numeric
console.log(
  JSON.stringify({
    levels: {
      trace: logger.levels.values.trace,
      debug: logger.levels.values.debug,
      info: logger.levels.values.info,
      warn: logger.levels.values.warn,
      error: logger.levels.values.error,
      fatal: logger.levels.values.fatal,
    },
  })
);
