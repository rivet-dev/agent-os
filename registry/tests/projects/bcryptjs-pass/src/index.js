"use strict";

const bcrypt = require("bcryptjs");

// Hash a password with explicit salt rounds
const password = "testPassword123";
const salt = bcrypt.genSaltSync(4);
const hash = bcrypt.hashSync(password, salt);

// Verify correct password
const correctMatch = bcrypt.compareSync(password, hash);

// Verify wrong password
const wrongMatch = bcrypt.compareSync("wrongPassword", hash);

// Hash format validation
const isValidHash = hash.startsWith("$2a$04$") && hash.length === 60;

const result = {
	hashLength: hash.length,
	correctMatch,
	wrongMatch,
	isValidHash,
};

console.log(JSON.stringify(result));
