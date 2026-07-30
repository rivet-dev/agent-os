"use strict";

const jwt = require("jsonwebtoken");

const secret = "test-secret-key-for-fixture";

// Sign a JWT with HS256 (default algorithm)
const payload = { sub: "user-123", name: "Alice", admin: true };
const token = jwt.sign(payload, secret, { algorithm: "HS256", noTimestamp: true });

// Verify the token
const decoded = jwt.verify(token, secret);

// Decode without verification
const unverified = jwt.decode(token, { complete: true });

// Verify with wrong secret fails
let verifyError = null;
try {
	jwt.verify(token, "wrong-secret");
} catch (err) {
	verifyError = { name: err.name, message: err.message };
}

const result = {
	token,
	decoded: { sub: decoded.sub, name: decoded.name, admin: decoded.admin },
	header: unverified.header,
	verifyError,
};

console.log(JSON.stringify(result));
