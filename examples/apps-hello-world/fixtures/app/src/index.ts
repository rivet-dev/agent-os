import { Hono } from "hono";

const app = new Hono();

// Serve the application's frontend.
app.get("/", (c) => {
	return c.html(`<!doctype html>
<html lang="en">
	<head>
		<meta charset="utf-8">
		<meta name="viewport" content="width=device-width, initial-scale=1">
		<title>Hello from agentOS Apps</title>
	</head>
	<body>
		<main>
			<h1>Hello from agentOS Apps</h1>
			<p>This HTML is served by an HTTP app running inside an agentOS VM.</p>
			<p><a href="./api/hello">Call the JSON API</a></p>
		</main>
	</body>
</html>`);
});

// Serve a REST API request from the same application.
app.get("/api/hello", (c) => {
	return c.json({ message: "Hello from agentOS Apps" });
});

export default app;
