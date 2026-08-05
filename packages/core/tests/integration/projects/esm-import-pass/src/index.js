import { Hono } from "hono";

const app = new Hono();

const result = {
	fetchType: typeof app.fetch,
};

console.log(JSON.stringify(result));
