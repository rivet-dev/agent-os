export function normalizeAcpResponse(line) {
	let message;
	try {
		message = JSON.parse(line);
	} catch {
		return line;
	}
	if (
		message?.error?.code !== -32602
		|| !/unknown sessionid:/i.test(message.error.message ?? "")
	) return line;

	return JSON.stringify({
		...message,
		error: {
			...message.error,
			code: -32002,
			message: "Resource not found",
			data: { kind: "unknown_session" },
		},
	});
}
