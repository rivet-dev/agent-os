const sdk = await import("@vercel/sdk");
console.log(JSON.stringify({ vercel: typeof sdk.Vercel === "function" }));
