import {
	allowAll,
	createInMemoryFileSystem,
	createNodeDriver,
	createNodeHostCommandExecutor,
	createNodeRuntimeDriverFactory,
	NodeRuntime,
	type NodeRuntimeOptions,
} from "secure-exec";

export function createQuickstartOptions(): NodeRuntimeOptions {
	const filesystem = createInMemoryFileSystem();
	const systemDriver = createNodeDriver({
		filesystem,
		permissions: allowAll,
		commandExecutor: createNodeHostCommandExecutor(),
	});

	return {
		systemDriver,
		runtimeDriverFactory: createNodeRuntimeDriverFactory(),
	};
}

void NodeRuntime;
