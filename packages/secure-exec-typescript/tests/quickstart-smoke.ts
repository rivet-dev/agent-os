import {
	createTypeScriptTools,
	type ProjectCompileResult,
	type TypeCheckResult,
	type TypeScriptTools,
} from "@secure-exec/typescript";
import { createNodeDriver, createNodeRuntimeDriverFactory } from "secure-exec";

export function createQuickstartTools(): TypeScriptTools {
	return createTypeScriptTools({
		systemDriver: createNodeDriver(),
		runtimeDriverFactory: createNodeRuntimeDriverFactory(),
	});
}

void createQuickstartTools;
void (null as ProjectCompileResult | null);
void (null as TypeCheckResult | null);
