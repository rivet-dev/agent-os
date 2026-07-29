try {
  const { rollup } = await import("rollup");
  const bundle = await rollup({ input: new URL("./input.js", import.meta.url).pathname });
  const output = await bundle.generate({ format: "esm" });
  console.log(output.output[0].code.includes("42"));
} catch (error) {
  console.error(`ROLLUP_NATIVE_UNSUPPORTED: ${error?.message ?? error}`);
  process.exit(1);
}
