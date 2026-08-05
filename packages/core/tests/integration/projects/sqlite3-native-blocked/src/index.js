try {
  const { default: sqlite3 } = await import("sqlite3");
  const db = new sqlite3.Database(":memory:");
  const row = await new Promise((resolve, reject) => db.get("select 7 as value", (error, value) => error ? reject(error) : resolve(value)));
  console.log(row.value);
  db.close();
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
