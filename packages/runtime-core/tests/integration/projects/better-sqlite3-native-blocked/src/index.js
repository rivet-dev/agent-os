try {
  const { default: Database } = await import("better-sqlite3");
  const db = new Database(":memory:");
  console.log(db.prepare("select 7 as value").get().value);
  db.close();
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
