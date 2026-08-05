import {
	createTypeScriptTools,
	type ProjectCompileResult,
	type TypeCheckResult,
	type TypeScriptTools,
} from "@rivet-dev/agentos-core/internal/typescript-tools";
import {
	createNodeDriver,
	createNodeRuntimeDriverFactory,
} from "@rivet-dev/agentos-core/internal/runtime-compat";

export function createQuickstartTools(): TypeScriptTools {
	return createTypeScriptTools({
		systemDriver: createNodeDriver(),
		runtimeDriverFactory: createNodeRuntimeDriverFactory(),
	});
}

void createQuickstartTools;
void (null as ProjectCompileResult | null);
void (null as TypeCheckResult | null);
