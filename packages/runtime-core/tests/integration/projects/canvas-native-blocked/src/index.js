try {
  const { createCanvas } = await import("canvas");
  const canvas = createCanvas(2, 2);
  canvas.getContext("2d").fillRect(0, 0, 2, 2);
  console.log(canvas.toBuffer("image/png").length > 20);
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
