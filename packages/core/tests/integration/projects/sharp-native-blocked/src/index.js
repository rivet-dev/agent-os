try {
  const { default: sharp } = await import("sharp");
  const value = await sharp({ create: { width: 1, height: 1, channels: 4, background: "red" } }).png().toBuffer();
  console.log(value.length > 20);
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
