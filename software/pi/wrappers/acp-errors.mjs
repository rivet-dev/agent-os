export function normalizeAcpResponse(line) {
	let message;
	try {
		message = JSON.parse(line);
	} catch {
		return line;
	}
	const invalidSession = /unknown sessionid:|cwd does not match persisted session:/i.test(
		message?.error?.message ?? "",
	);
	if (message?.error?.code !== -32602 || !invalidSession) return line;

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

export function acpRequestCwd(line) {
	try {
		const message = JSON.parse(line);
		const cwd = message?.params?.cwd;
		return typeof cwd === "string" && cwd.startsWith("/") ? cwd : null;
	} catch {
		return null;
	}
}
