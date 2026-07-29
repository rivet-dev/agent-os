try {
  const { default: bcrypt } = await import("bcrypt");
  const hash = bcrypt.hashSync("agent", 4);
  console.log(bcrypt.compareSync("agent", hash));
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
