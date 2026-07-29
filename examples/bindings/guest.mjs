import { execFile } from "node:child_process";
import { promisify } from "node:util";

const run = promisify(execFile);
const { stdout } = await run("agentos-weather", [
	"forecast",
	"--city",
	"Paris",
	"--days",
	"3",
]);

const response = JSON.parse(stdout);
console.log(response.result);
