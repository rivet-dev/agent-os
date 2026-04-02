const path = require("node:path");
const dotenv = require("dotenv");

const result = dotenv.config({
	path: path.join(__dirname, "..", ".env"),
});

if (result.error) {
	throw result.error;
}

console.log(`GREETING=${process.env.GREETING}`);
