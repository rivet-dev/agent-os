import postgres from "postgres";
import { BSON, ObjectId } from "mongodb";
import { createClient as createRedisClient } from "redis";
import { createClient as createLibsqlClient } from "@libsql/client/web";
import { PutObjectCommand, S3Client } from "@aws-sdk/client-s3";

const sql = postgres({ host: "127.0.0.1", port: 1, connect_timeout: 1 });
const pendingQuery = sql`select ${42} as value`;
const postgresShape = typeof pendingQuery.execute === "function";
await sql.end({ timeout: 0 });

const id = new ObjectId("64b7abdecf2160b649ab6085");
const bson = BSON.deserialize(BSON.serialize({ id, n: 7 }));

const redis = createRedisClient({ url: "redis://127.0.0.1:6379" });
const redisShape = typeof redis.connect === "function" && redis.options.url.includes("6379");

const libsql = createLibsqlClient({ url: "https://database.invalid" });
const libsqlShape = typeof libsql.execute === "function";
libsql.close();

const s3 = new S3Client({
  region: "us-east-1",
  endpoint: "http://127.0.0.1:9000",
  credentials: { accessKeyId: "test", secretAccessKey: "test" },
});
const command = new PutObjectCommand({ Bucket: "agents", Key: "a.txt", Body: "hello" });
s3.destroy();

console.log(JSON.stringify({
  postgresShape,
  mongo: [bson.id.toHexString(), bson.n],
  redisShape,
  libsql: libsqlShape,
  s3: [command.input.Bucket, command.input.Key],
}));
