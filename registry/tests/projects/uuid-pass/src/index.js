import { v4, v5, validate, version, NIL } from "uuid";

// Generate a random v4 UUID and validate its format
const id4 = v4();
const isValid4 = validate(id4);
const ver4 = version(id4);

// Deterministic v5 UUID with DNS namespace
const DNS_NAMESPACE = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
const id5 = v5("agent-os.test", DNS_NAMESPACE);
const isValid5 = validate(id5);
const ver5 = version(id5);

// Validate the nil UUID
const nilValid = validate(NIL);

const result = {
	v4: { valid: isValid4, version: ver4 },
	v5: { value: id5, valid: isValid5, version: ver5 },
	nil: { value: NIL, valid: nilValid },
};

console.log(JSON.stringify(result));
