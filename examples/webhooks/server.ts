import { agentOS, setup } from "@rivet-dev/agentos";
import { createClient } from "@rivet-dev/agentos/client";
import { Hono } from "hono";
import pi from "@agentos-software/pi";

const vm = agentOS({
	software: [pi],
});

export const registry = setup({ use: { vm } });
registry.start();

// Hono server to receive Slack webhooks
const app = new Hono();
const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});

app.post("/slack/events", async (c) => {
	const body = await c.req.json();

	// Handle Slack URL verification
	if (body.type === "url_verification") {
		return c.json({ challenge: body.challenge });
	}

	// Prompt calls for one session are serialized automatically by AgentOS.
	if (body.event?.type === "message" && !body.event?.bot_id) {
		const agent = client.vm.getOrCreate("slack-agent");
		await agent.sessions.open({
			agent: "pi",
			env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
			additionalInstructions: "You answer Slack messages concisely.",
		});
		const result = await agent.sessions.prompt({
			content: [
				{
					type: "text",
					text: `Slack message from ${body.event.user} in #${body.event.channel}:\n\n${body.event.text}\n\nRespond helpfully.`,
				},
			],
		});
		const text =
			result.message?.content
				.filter((block) => block.type === "text")
				.map((block) => block.text)
				.join("") ?? "";
		const response = await fetch("https://slack.com/api/chat.postMessage", {
			method: "POST",
			headers: {
				"Content-Type": "application/json",
				Authorization: `Bearer ${process.env.SLACK_BOT_TOKEN}`,
			},
			body: JSON.stringify({ channel: body.event.channel, text }),
		});
		if (!response.ok) {
			throw new Error(`Slack reply failed with status ${response.status}`);
		}
	}

	return c.json({ ok: true });
});

export default app;
