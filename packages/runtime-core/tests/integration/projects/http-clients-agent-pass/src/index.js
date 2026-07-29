import http from "node:http";
import { request as undiciRequest } from "undici";
import got from "got";
import ky from "ky";
import superagent from "superagent";
import { Server as SocketServer } from "socket.io";
import { io as socketClient } from "socket.io-client";

const server = http.createServer((req, res) => {
  res.writeHead(200, { "content-type": "application/json" });
  res.end(JSON.stringify({ path: req.url }));
});
const io = new SocketServer(server, { serveClient: false });
io.on("connection", socket => socket.on("ping-agent", (_, reply) => reply("pong-agent")));
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const port = server.address().port;
const base = `http://127.0.0.1:${port}`;

try {
  const undiciResponse = await undiciRequest(base + "/undici");
  const undiciBody = await undiciResponse.body.json();
  const gotBody = await got(base + "/got").json();
  const kyBody = await ky.get(base + "/ky").json();
  const superagentBody = (await superagent.get(base + "/superagent")).body;
  async function socketReply(transport) {
    const socket = socketClient(base, { transports: [transport], forceNew: true });
    try {
      return await new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error(`Socket.IO ${transport} timeout`)), 10_000);
        socket.on("connect_error", error => {
          clearTimeout(timeout);
          const description = error?.description;
          const details = description && typeof description === "object"
            ? Object.fromEntries([
                "message",
                "code",
                "errno",
                "syscall",
                "statusCode",
              ].flatMap(key => description[key] == null ? [] : [[key, description[key]]]))
            : description;
          reject(new Error(`Socket.IO ${transport} connect error: ${error?.message ?? error}; ${JSON.stringify(details)}`));
        });
        socket.on("connect", () => socket.emit("ping-agent", "ping", reply => {
          clearTimeout(timeout);
          resolve(reply);
        }));
      });
    } finally {
      socket.close();
    }
  }
  const pollingReply = await socketReply("polling");
  const websocketReply = await socketReply("websocket");
  console.log(JSON.stringify([
    undiciBody.path,
    gotBody.path,
    kyBody.path,
    superagentBody.path,
    pollingReply,
    websocketReply,
  ]));
} finally {
  await io.close();
  await new Promise(resolve => server.close(resolve));
}
