const rivetkit = require("rivetkit");

if (typeof rivetkit.actor !== "function") {
  throw new Error("expected rivetkit.actor to be a function");
}

const definition = rivetkit.actor({ actions: {} });
if (!definition || typeof definition !== "object") {
  throw new Error("expected actor() to return a definition object");
}

console.log("rivetkit fixture ok");
