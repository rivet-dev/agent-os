"use strict";

const { z } = require("zod");

// Define schemas
const userSchema = z.object({
	name: z.string().min(1),
	age: z.number().int().positive(),
	email: z.string().email(),
	tags: z.array(z.string()).optional(),
});

const statusSchema = z.enum(["active", "inactive", "pending"]);

// Successful validation
const validUser = userSchema.parse({
	name: "Alice",
	age: 30,
	email: "alice@example.com",
	tags: ["admin"],
});

// Failed validation
let validationError = null;
try {
	userSchema.parse({ name: "", age: -1, email: "bad" });
} catch (err) {
	validationError = {
		issueCount: err.issues.length,
		codes: err.issues.map((i) => i.code).sort(),
	};
}

// Safe parse
const safeResult = userSchema.safeParse({ name: "Bob", age: 25, email: "bob@test.com" });
const safeFail = userSchema.safeParse({ name: 123 });

// Enum
const enumResult = statusSchema.safeParse("active");
const enumFail = statusSchema.safeParse("unknown");

// Transform and refine
const doubled = z.number().transform((n) => n * 2).parse(5);

const result = {
	validUser: { name: validUser.name, age: validUser.age, hasTags: Array.isArray(validUser.tags) },
	validationError,
	safeParseSuccess: safeResult.success,
	safeParseFail: safeFail.success,
	enumSuccess: enumResult.success,
	enumFail: enumFail.success,
	transformed: doubled,
};

console.log(JSON.stringify(result));
