import { Chalk } from "chalk";

// Force color level 1 (basic ANSI) for deterministic output across environments
const c = new Chalk({ level: 1 });

const red = c.red("red");
const green = c.green("green");
const blue = c.blue("blue");
const bold = c.bold("bold");
const underline = c.underline("underline");
const nested = c.red.bold.underline("nested");
const bg = c.bgYellow.black("highlight");
const combined = c.italic(c.cyan("italic-cyan"));

const result = {
	red,
	green,
	blue,
	bold,
	underline,
	nested,
	bg,
	combined,
	supportsLevel: typeof c.level,
};

console.log(JSON.stringify(result));
