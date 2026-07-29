import { actor, setup } from "rivetkit";
import { db } from "rivetkit/db";

const notes = actor({
	db: db({
		async onMigrate(database) {
			await database.execute(`
				CREATE TABLE IF NOT EXISTS notes (
					id INTEGER PRIMARY KEY AUTOINCREMENT,
					body TEXT NOT NULL
				)
			`);
		},
	}),
	actions: {
		async add(c, body: string) {
			await c.db.execute("INSERT INTO notes (body) VALUES (?)", body);
		},
		async list(c) {
			return c.db.execute("SELECT id, body FROM notes ORDER BY id");
		},
	},
});

export const registry = setup({
	use: { notes },
});

registry.start();

export default function fetch() {
	return Response.json({
		app: "sqlite-notes",
		message: "Use the RivetKit client to add and list notes.",
	});
}
