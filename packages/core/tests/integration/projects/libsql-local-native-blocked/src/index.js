try {
  const { createClient } = await import("@libsql/client");
  const client = createClient({ url: "file::memory:" });
  await client.execute("select 7 as value");
  client.close();
  console.log(7);
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
