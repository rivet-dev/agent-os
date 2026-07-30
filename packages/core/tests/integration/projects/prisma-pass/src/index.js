import { Prisma, PrismaClient } from "@prisma/client";
import { PrismaPg } from "@prisma/adapter-pg";

const adapter = new PrismaPg({
  connectionString: "postgresql://agentos:agentos@127.0.0.1:1/agentos",
  connectionTimeoutMillis: 100,
});
const prisma = new PrismaClient({ adapter });
let queryRejected = false;
try {
  await prisma.user.findMany();
} catch {
  queryRejected = true;
} finally {
  await prisma.$disconnect();
}

const query = Prisma.sql`SELECT ${42} AS answer`;
console.log(JSON.stringify({
  decimal: new Prisma.Decimal("1.25").plus("2.75").toString(),
  generatedModel: typeof prisma.user.findMany === "function",
  queryRejected,
  sqlValues: query.values,
}));
