try {
  const { transform } = await import("@swc/core");
  const result = await transform("const answer = () => 42;", {
    jsc: { target: "es5" },
  });
  console.log(result.code.includes("function"));
} catch (error) {
  console.error(`SWC_NATIVE_UNSUPPORTED: ${error?.message ?? error}`);
  process.exit(1);
}
