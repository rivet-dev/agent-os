import { actor, setup } from "rivetkit";

const generatedState = actor({
	state: { requests: 0 },
	actions: {
		record(c) {
			c.state.requests += 1;
			return c.state.requests;
		},
	},
});

export const registry = setup({
	use: { generatedState },
});

registry.start();

export default function fetch() {
	return Response.json({
		message: "Replace this seed with the generated application.",
	});
}
