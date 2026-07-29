try {
  const { default: argon2 } = await import("argon2");
  const hash = await argon2.hash("agent");
  console.log(await argon2.verify(hash, "agent"));
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
