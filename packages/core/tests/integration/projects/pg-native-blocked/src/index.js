try {
  const { default: PgNative } = await import("pg-native");
  const client = new PgNative();
  console.log(typeof client.query === "function");
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
