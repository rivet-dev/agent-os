import { actor, event, setup } from "rivetkit";

type Position = { x: number; y: number };

const room = actor({
	state: {
		players: {} as Record<string, Position>,
	},
	events: {
		changed: event(),
	},
	actions: {
		join(c, player: string) {
			c.state.players[player] ??= { x: 0, y: 0 };
			c.broadcast("changed", c.state.players);
			return c.state.players;
		},
		move(c, player: string, x: number, y: number) {
			c.state.players[player] = { x, y };
			c.broadcast("changed", c.state.players);
			return c.state.players;
		},
		inspect(c) {
			return c.state.players;
		},
	},
});

export const registry = setup({
	use: { room },
});

registry.start();

export default function fetch() {
	return Response.json({
		app: "multiplayer-room",
		message: "Use the RivetKit client to join and move in a room.",
	});
}
