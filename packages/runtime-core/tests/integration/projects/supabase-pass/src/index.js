import { createClient } from "@supabase/supabase-js";
import http from "node:http";

let observed;
const server = http.createServer((request, response) => {
  observed = {
    authorization: request.headers.authorization,
    path: request.url,
  };
  response.writeHead(200, { "content-type": "application/json" });
  response.end(JSON.stringify([{ id: 7, name: "AgentOS" }]));
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const address = server.address();
if (!address || typeof address === "string") throw new Error("Supabase mock did not bind");

try {
  const supabase = createClient(
    `http://127.0.0.1:${address.port}`,
    "test-anon-key",
    {
      auth: { autoRefreshToken: false, persistSession: false },
      global: { fetch },
    },
  );
  const { data, error } = await supabase
    .from("documents")
    .select("id,name")
    .eq("owner", "agent");
  if (error) throw error;

  console.log(JSON.stringify({
    data,
    authorization: observed.authorization,
    path: observed.path,
  }));
} finally {
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
}
