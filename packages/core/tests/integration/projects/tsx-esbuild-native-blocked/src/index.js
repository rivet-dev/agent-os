try {
  const { tsImport } = await import("tsx/esm/api");
  const module = await tsImport("./input.ts", import.meta.url);
  console.log(module.answer);
} catch (error) {
  console.error(`TSX_ESBUILD_NATIVE_UNSUPPORTED: ${error?.message ?? error}`);
  process.exit(1);
}
