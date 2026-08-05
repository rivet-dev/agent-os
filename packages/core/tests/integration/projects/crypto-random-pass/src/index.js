const bytes = new Uint8Array(16);
crypto.getRandomValues(bytes);

const uuid = crypto.randomUUID();
const uuidV4Pattern =
	/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

console.log(
	JSON.stringify({
		uuidV4: uuidV4Pattern.test(uuid),
		uuidLength: uuid.length,
		randomValuesLength: bytes.length,
		arrayTag: Object.prototype.toString.call(bytes),
	}),
);
