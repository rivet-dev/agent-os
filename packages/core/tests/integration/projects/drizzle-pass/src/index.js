"use strict";

const { pgTable, text, integer, serial, varchar, boolean } = require("drizzle-orm/pg-core");
const { eq, and, sql } = require("drizzle-orm");

// Define a table schema without connecting to a database
const users = pgTable("users", {
	id: serial("id").primaryKey(),
	name: text("name").notNull(),
	email: varchar("email", { length: 255 }).notNull(),
	age: integer("age"),
	active: boolean("active").default(true),
});

// Inspect schema shape
const tableName = users[Symbol.for("drizzle:Name")];
const columnNames = Object.keys(users)
	.filter((k) => typeof k === "string" && !k.startsWith("_"))
	.sort();
const idIsPrimary = users.id.primary;
const nameNotNull = users.name.notNull;
const emailLength = users.email.config ? users.email.config.length : null;

// Verify operators exist
const eqExists = typeof eq === "function";
const andExists = typeof and === "function";
const sqlExists = typeof sql === "function";

// Verify sql template tag produces a fragment object
const fragment = sql`${users.id} = 1`;
const fragmentExists = fragment !== null && typeof fragment === "object";

const result = {
	tableName,
	columnNames,
	idIsPrimary,
	nameNotNull,
	emailLength,
	eqExists,
	andExists,
	sqlExists,
	fragmentExists,
};

console.log(JSON.stringify(result));
