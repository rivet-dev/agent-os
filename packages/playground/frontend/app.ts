import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
	type NodeRuntimeDriver,
	type StdioChannel,
	type StdioEvent,
} from "@rivet-dev/agent-os-browser";

type Language = "nodejs";
type TypeScriptApi = typeof import("typescript");
type TypeScriptDiagnostic = import("typescript").Diagnostic;

type MonacoTheme = {
	base: "vs-dark";
	inherit: boolean;
	rules: Array<{ token: string; foreground: string }>;
	colors: Record<string, string>;
};

type MonacoEditorOptions = {
	automaticLayout: boolean;
	fontFamily: string;
	fontLigatures: boolean;
	fontSize: number;
	language: string;
	lineHeight: number;
	minimap: { enabled: boolean };
	padding: { top: number; bottom: number };
	roundedSelection: boolean;
	scrollBeyondLastLine: boolean;
	smoothScrolling: boolean;
	tabSize: number;
	theme: string;
	value: string;
};

type MonacoModel = object;

interface MonacoEditorInstance {
	addCommand(keybinding: number, handler: () => void): void;
	getModel(): MonacoModel | null;
	getValue(): string;
	setValue(value: string): void;
	dispose(): void;
}

interface MonacoApi {
	KeyCode: { Enter: number };
	KeyMod: { CtrlCmd: number };
	editor: {
		create(
			container: HTMLElement,
			options: MonacoEditorOptions,
		): MonacoEditorInstance;
		defineTheme(name: string, theme: MonacoTheme): void;
		setModelLanguage(model: MonacoModel, language: string): void;
	};
	languages: {
		typescript: {
			typescriptDefaults: {
				setCompilerOptions(options: Record<string, unknown>): void;
				setDiagnosticsOptions(options: Record<string, unknown>): void;
			};
		};
	};
}

interface MonacoRequire {
	(
		dependencies: string[],
		onLoad: () => void,
		onError: (error: unknown) => void,
	): void;
	config(options: { paths: { vs: string } }): void;
}

type OutputLine = {
	channel: StdioChannel | "system";
	message: string;
};

type PlaygroundRunResult = {
	code: number;
	errorMessage: string | null;
	lines: OutputLine[];
};

type Example = {
	name: string;
	code: string;
};

type LanguageConfig = {
	fileName: string;
	hint: string;
	label: string;
	monacoLanguage: string;
	examples: Example[];
};

type Process = {
	id: string;
	language: Language;
	name: string;
	code: string;
	outputLines: OutputLine[];
	isRunning: boolean;
};

type PlaygroundRuntime = Pick<
	NodeRuntimeDriver,
	"exec" | "dispose" | "terminate"
>;

declare global {
	interface Window {
		monaco?: MonacoApi;
		require?: MonacoRequire;
		ts?: TypeScriptApi;
	}
}

const MONACO_VS_URL = new URL("/vendor/monaco/vs", import.meta.url).href;
const PLAYGROUND_RUNTIME_CWD = "/root";
const PLAYGROUND_RUNTIME_HOME = "/root";
const PLAYGROUND_RUNTIME_TMPDIR = "/tmp";
const playgroundWorkerUrl = new URL("/agent-os-worker.js", import.meta.url);
const playgroundRuntimeFactory = createBrowserRuntimeDriverFactory({
	workerUrl: playgroundWorkerUrl,
});
const LANGUAGE_CONFIG: Record<Language, LanguageConfig> = {
	nodejs: {
		label: "Node.js",
		monacoLanguage: "typescript",
		fileName: "/playground.ts",
		hint: "Runs through the Agent OS browser runtime",
		examples: [
			{
				name: "Counter",
				code: `globalThis.counter = (globalThis.counter ?? 0) + 1;
console.log(\`Counter: \${globalThis.counter}\`);
`,
			},
			{
				name: "File System",
				code: `import { promises as fs } from "fs";

const path = "/counter.txt";
(async () => {
  let prev = 0;
  try { prev = parseInt(await fs.readFile(path, "utf8"), 10) || 0; } catch {}
  const count = prev + 1;
  await fs.writeFile(path, String(count));
  console.log(\`File counter: \${count}\`);
})();
`,
			},
		],
	},
};

const LANGUAGE_ICONS: Record<Language, string> = {
	nodejs: `<svg viewBox="0 0 24 24" fill="currentColor"><path d="M11.998.006a1.27 1.27 0 0 0-.63.174L2.032 5.49a1.27 1.27 0 0 0-.634 1.1v10.818a1.27 1.27 0 0 0 .634 1.1l9.336 5.312a1.27 1.27 0 0 0 1.26 0l9.336-5.312a1.27 1.27 0 0 0 .634-1.1V6.59a1.27 1.27 0 0 0-.634-1.1L12.628.18a1.27 1.27 0 0 0-.63-.174z"/></svg>`,
};

function getElement<T extends Element>(selector: string): T {
	const element = document.querySelector<T>(selector);
	if (!element) {
		throw new Error(`Missing required element: ${selector}`);
	}
	return element;
}

function getChildElement<T extends Element>(
	root: ParentNode,
	selector: string,
): T {
	const element = root.querySelector<T>(selector);
	if (!element) {
		throw new Error(`Missing required element: ${selector}`);
	}
	return element;
}

/* DOM references */
const runtimeStatus = getElement<HTMLElement>("#runtime-status");
const processListEl = getElement<HTMLElement>("#process-list");
const workspaceEl = getElement<HTMLElement>("#workspace");
let emptyStateEl = getElement<HTMLElement>("#empty-state");
const addProcessButton = getElement<HTMLButtonElement>("#add-process-button");
const addProcessMenu = getElement<HTMLElement>("#add-process-menu");

/* State */
let monaco: MonacoApi | null = null;
let editor: MonacoEditorInstance | null = null;
let nodejsRuntimePromise: Promise<PlaygroundRuntime> | null = null;
let nextProcessId = 1;
const processes: Process[] = [];
let activeProcessId: string | null = null;
let prewarming = false;

/* Workspace DOM (created when first process is added) */
let editorPanel: HTMLElement | null = null;
let editorContainer: HTMLElement | null = null;
let editorLabelEl: HTMLElement | null = null;
let editorHintEl: HTMLElement | null = null;
let outputEl: HTMLElement | null = null;
let runButtonEl: HTMLButtonElement | null = null;

/* Status */
function setStatus(
	text: string,
	tone: "pending" | "ready" | "error" = "pending",
): void {
	if (prewarming && tone === "ready") return;
	if (prewarming && tone === "pending") text = "Warming up runtime...";
	runtimeStatus.textContent = text;
	runtimeStatus.classList.remove("ready", "error");
	if (tone === "ready") runtimeStatus.classList.add("ready");
	if (tone === "error") runtimeStatus.classList.add("error");
}

/* Output helpers */
function appendOutputToProcess(
	proc: Process,
	channel: OutputLine["channel"],
	message: string,
): void {
	proc.outputLines.push({ channel, message });
	if (proc.id === activeProcessId && outputEl) {
		const line = document.createElement("div");
		line.className = `output-line ${channel}`;
		line.textContent = message;
		outputEl.appendChild(line);
		outputEl.scrollTop = outputEl.scrollHeight;
	}
}

function renderProcessOutput(proc: Process): void {
	if (!outputEl) return;
	outputEl.innerHTML = "";
	for (const entry of proc.outputLines) {
		const line = document.createElement("div");
		line.className = `output-line ${entry.channel}`;
		line.textContent = entry.message;
		outputEl.appendChild(line);
	}
	outputEl.scrollTop = outputEl.scrollHeight;
}

/* Shared helpers */
function stripAnsi(text: string): string {
	return text.replace(/\u001b\[[0-9;]*m/g, "");
}

function waitForGlobal<T>(
	checker: () => T | null | undefined,
	label: string,
): Promise<NonNullable<T>> {
	return new Promise((resolve, reject) => {
		const startedAt = Date.now();
		const tick = (): void => {
			const value = checker();
			if (value) {
				resolve(value as NonNullable<T>);
				return;
			}
			if (Date.now() - startedAt > 15_000) {
				reject(new Error(`Timed out while loading ${label}`));
				return;
			}
			window.setTimeout(tick, 30);
		};
		tick();
	});
}

/* Monaco */
async function loadMonaco(): Promise<MonacoApi> {
	if (window.monaco?.editor) return window.monaco;
	const monacoRequire = await waitForGlobal(
		() => window.require,
		"Monaco loader",
	);
	monacoRequire.config({ paths: { vs: MONACO_VS_URL } });
	return new Promise((resolve, reject) => {
		monacoRequire(
			["vs/editor/editor.main"],
			() => {
				if (!window.monaco) {
					reject(new Error("Monaco did not initialize"));
					return;
				}
				resolve(window.monaco);
			},
			reject,
		);
	});
}

function applyMonacoTheme(monacoInstance: MonacoApi): void {
	monacoInstance.editor.defineTheme("sandbox-agent-dark", {
		base: "vs-dark",
		inherit: true,
		rules: [
			{ token: "comment", foreground: "5c6773" },
			{ token: "keyword", foreground: "ff8f40" },
			{ token: "keyword.control", foreground: "ff8f40" },
			{ token: "string", foreground: "aad94c" },
			{ token: "number", foreground: "e6b450" },
			{ token: "type.identifier", foreground: "59c2ff" },
			{ token: "identifier", foreground: "bfbdb6" },
			{ token: "delimiter", foreground: "bfbdb6" },
			{ token: "operator", foreground: "f29668" },
			{ token: "function", foreground: "ffb454" },
			{ token: "variable", foreground: "bfbdb6" },
			{ token: "constant", foreground: "d2a6ff" },
			{ token: "tag", foreground: "39bae6" },
			{ token: "attribute.name", foreground: "ffb454" },
			{ token: "attribute.value", foreground: "aad94c" },
			{ token: "regexp", foreground: "95e6cb" },
		],
		colors: {
			"editor.background": "#0b0e14",
			"editor.foreground": "#bfbdb6",
			"editorLineNumber.foreground": "#5c6773",
			"editorLineNumber.activeForeground": "#bfbdb6",
			"editorCursor.foreground": "#e6b450",
			"editor.selectionBackground": "#409fff33",
			"editor.inactiveSelectionBackground": "#409fff1a",
			"editorIndentGuide.background1": "#1e222a",
			"editorIndentGuide.activeBackground1": "#3d424d",
			"editorGutter.background": "#0b0e14",
			"editorWidget.background": "#0d1017",
			"editorWidget.border": "#1e222a",
		},
	});
}

/* Runtimes */
async function ensureNodejsRuntime(): Promise<PlaygroundRuntime> {
	if (!nodejsRuntimePromise) {
		nodejsRuntimePromise = (async () => {
			setStatus("Booting Node.js runtime...");
			const systemDriver = await createBrowserDriver({
				filesystem: "memory",
				permissions: allowAll,
				useDefaultNetwork: true,
			});
			const runtime = playgroundRuntimeFactory.createRuntimeDriver({
				system: systemDriver,
				runtime: {
					process: {
						...systemDriver.runtime.process,
						cwd: systemDriver.runtime.process.cwd ?? PLAYGROUND_RUNTIME_CWD,
					},
					os: {
						...systemDriver.runtime.os,
						homedir: systemDriver.runtime.os.homedir ?? PLAYGROUND_RUNTIME_HOME,
						tmpdir: systemDriver.runtime.os.tmpdir ?? PLAYGROUND_RUNTIME_TMPDIR,
					},
				},
			});
			setStatus("Node.js runtime ready", "ready");
			return runtime;
		})().catch((error) => {
			nodejsRuntimePromise = null;
			setStatus("Node.js runtime failed", "error");
			throw error;
		});
	}
	return nodejsRuntimePromise;
}

/* TypeScript transpilation */
function formatTypeScriptDiagnostics(
	diagnostics: readonly TypeScriptDiagnostic[],
): string {
	if (diagnostics.length === 0) return "";
	const tsApi = window.ts;
	if (!tsApi) return "TypeScript transpiler is not available";
	const host = {
		getCanonicalFileName: (fileName: string) => fileName,
		getCurrentDirectory: () => "/",
		getNewLine: () => "\n",
	};
	return stripAnsi(
		tsApi.formatDiagnosticsWithColorAndContext(diagnostics, host),
	);
}

function transpileTypeScript(source: string): string {
	const tsApi = window.ts;
	if (!tsApi) throw new Error("TypeScript transpiler is not available");
	const transpileResult = tsApi.transpileModule(source, {
		fileName: LANGUAGE_CONFIG.nodejs.fileName,
		reportDiagnostics: true,
		compilerOptions: {
			target: tsApi.ScriptTarget.ES2022,
			module: tsApi.ModuleKind.CommonJS,
			strict: true,
			esModuleInterop: true,
		},
	});
	const diagnostics =
		transpileResult.diagnostics?.filter(
			(diagnostic) => diagnostic.category === tsApi.DiagnosticCategory.Error,
		) ?? [];
	if (diagnostics.length > 0)
		throw new Error(formatTypeScriptDiagnostics(diagnostics));
	return transpileResult.outputText;
}

/* Execution */
async function runNodejs(source: string): Promise<PlaygroundRunResult> {
	const runtime = await ensureNodejsRuntime();
	const outputLines: OutputLine[] = [];
	const compiledSource = transpileTypeScript(source);
	const result = await runtime.exec(compiledSource, {
		filePath: LANGUAGE_CONFIG.nodejs.fileName,
		onStdio: (event: StdioEvent) => {
			outputLines.push({ channel: event.channel, message: event.message });
		},
	});
	return {
		code: result.code,
		errorMessage: result.errorMessage ?? null,
		lines: outputLines,
	};
}

async function executeProcess(proc: Process): Promise<void> {
	if (proc.isRunning) return;

	/* Save current editor content */
	if (editor && proc.id === activeProcessId) {
		proc.code = editor.getValue();
	}

	proc.isRunning = true;
	proc.outputLines = [];
	updateRunButton(proc);
	renderProcessOutput(proc);
	appendOutputToProcess(
		proc,
		"system",
		`Running ${LANGUAGE_CONFIG[proc.language].label}...`,
	);

	try {
		const result = await runNodejs(proc.code);

		for (const line of result.lines) {
			appendOutputToProcess(proc, line.channel, line.message);
		}
		if (result.errorMessage && result.lines.length === 0) {
			appendOutputToProcess(proc, "stderr", result.errorMessage);
		}
		appendOutputToProcess(
			proc,
			"system",
			result.code === 0 ? "Exit code 0" : `Exit code ${result.code}`,
		);
		if (result.code === 0) {
			setStatus(
				`${LANGUAGE_CONFIG[proc.language].label} run completed`,
				"ready",
			);
		} else {
			setStatus(`${LANGUAGE_CONFIG[proc.language].label} run failed`, "error");
		}
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		appendOutputToProcess(proc, "stderr", message);
		appendOutputToProcess(proc, "system", "Exit code 1");
		setStatus(`${LANGUAGE_CONFIG[proc.language].label} run failed`, "error");
	} finally {
		proc.isRunning = false;
		updateRunButton(proc);
	}
}

function updateRunButton(proc: Process): void {
	if (proc.id !== activeProcessId || !runButtonEl) return;
	runButtonEl.disabled = proc.isRunning;
	runButtonEl.textContent = proc.isRunning ? "Running..." : "Run";
}

/* Process list rendering */
function renderProcessList(): void {
	processListEl.innerHTML = "";
	for (const proc of processes) {
		const item = document.createElement("button");
		item.className = `process-item${proc.id === activeProcessId ? " active" : ""}`;
		item.type = "button";

		const icon = document.createElement("span");
		icon.className = "process-item-icon";
		icon.innerHTML = LANGUAGE_ICONS[proc.language];

		const nameSpan = document.createElement("span");
		nameSpan.className = "process-item-name";
		nameSpan.textContent = proc.name;

		const closeBtn = document.createElement("button");
		closeBtn.className = "process-item-close";
		closeBtn.type = "button";
		closeBtn.textContent = "\u00d7";
		closeBtn.addEventListener("click", (e) => {
			e.stopPropagation();
			removeProcess(proc.id);
		});

		item.appendChild(icon);
		item.appendChild(nameSpan);
		item.appendChild(closeBtn);

		item.addEventListener("click", () => switchToProcess(proc.id));
		processListEl.appendChild(item);
	}
}

/* Workspace panel creation */
function ensureWorkspacePanels(): void {
	if (editorPanel) return;

	emptyStateEl.style.display = "none";
	workspaceEl.style.display = "grid";

	/* Editor panel */
	editorPanel = document.createElement("section");
	editorPanel.className = "panel editor-shell";
	editorPanel.innerHTML = `
		<div class="panel-header">
			<div class="panel-title" id="editor-label">Editor</div>
			<div class="editor-header-right">
				<div class="hint" id="editor-hint"></div>
				<div class="controls">
					<button id="run-button" class="button primary" type="button">Run</button>
				</div>
			</div>
		</div>
		<div id="editor" class="editor"></div>
	`;
	workspaceEl.appendChild(editorPanel);

	/* Right sidebar (output + notes) */
	const rightSidebar = document.createElement("aside");
	rightSidebar.className = "right-sidebar";
	rightSidebar.innerHTML = `
		<section class="panel output-shell">
			<div class="panel-header">
				<div class="panel-title">Output</div>
				<div class="controls">
					<button id="clear-button" class="button" type="button">Clear</button>
				</div>
			</div>
			<div id="output" class="output" aria-live="polite"></div>
		</section>
		<section class="panel snippet-panel">
			<div class="panel-header">
				<div class="panel-title">SDK Usage</div>
			</div>
			<div class="snippet-code"><span class="sk">import</span> { <span class="sv">allowAll</span>, <span class="sf">createBrowserDriver</span>, <span class="sf">createBrowserRuntimeDriverFactory</span> } <span class="sk">from</span> <span class="ss">"@rivet-dev/agent-os-browser"</span>;

<span class="sc">// Create a browser-backed runtime</span>
<span class="sk">const</span> <span class="sv">system</span> = <span class="sk">await</span> <span class="sf">createBrowserDriver</span>({
  <span class="sv">filesystem</span>: <span class="ss">"memory"</span>,
  <span class="sv">permissions</span>: <span class="sv">allowAll</span>,
  <span class="sv">useDefaultNetwork</span>: <span class="sk">true</span>,
});

<span class="sk">const</span> <span class="sv">runtime</span> = <span class="sf">createBrowserRuntimeDriverFactory</span>().<span class="sf">createRuntimeDriver</span>({
  <span class="sv">system</span>,
  <span class="sv">runtime</span>: {
    <span class="sv">process</span>: { <span class="sv">cwd</span>: <span class="ss">"/root"</span> },
    <span class="sv">os</span>: { <span class="sv">homedir</span>: <span class="ss">"/root"</span>, <span class="sv">tmpdir</span>: <span class="ss">"/tmp"</span> },
  },
});

<span class="sc">// Execute code with streaming output</span>
<span class="sk">const</span> <span class="sv">result</span> = <span class="sk">await</span> <span class="sv">runtime</span>.<span class="sf">exec</span>(<span class="ss">\`console.log("hello")\`</span>, {
  <span class="sf">onStdio</span>: (<span class="sv">event</span>) =&gt; <span class="sv">console</span>.<span class="sf">log</span>(<span class="sv">event</span>.<span class="sv">message</span>),
});

<span class="sv">console</span>.<span class="sf">log</span>(<span class="sv">result</span>.<span class="sv">code</span>); <span class="sc">// 0</span></div>
		</section>
	`;
	workspaceEl.appendChild(rightSidebar);

	/* Grab references */
	const nextEditorContainer = getChildElement<HTMLElement>(
		workspaceEl,
		"#editor",
	);
	const nextEditorLabel = getChildElement<HTMLElement>(
		workspaceEl,
		"#editor-label",
	);
	const nextEditorHint = getChildElement<HTMLElement>(
		workspaceEl,
		"#editor-hint",
	);
	const nextOutput = getChildElement<HTMLElement>(workspaceEl, "#output");
	const nextRunButton = getChildElement<HTMLButtonElement>(
		workspaceEl,
		"#run-button",
	);
	const nextClearButton = getChildElement<HTMLButtonElement>(
		workspaceEl,
		"#clear-button",
	);

	editorContainer = nextEditorContainer;
	editorLabelEl = nextEditorLabel;
	editorHintEl = nextEditorHint;
	outputEl = nextOutput;
	runButtonEl = nextRunButton;

	nextRunButton.addEventListener("click", () => {
		const proc = getActiveProcess();
		if (proc) void executeProcess(proc);
	});
	nextClearButton.addEventListener("click", () => {
		const proc = getActiveProcess();
		if (!proc) return;
		proc.outputLines = [];
		renderProcessOutput(proc);
		appendOutputToProcess(proc, "system", "Output cleared");
	});
}

function hideWorkspacePanels(): void {
	if (!editorPanel) return;
	if (editor) {
		editor.dispose();
		editor = null;
	}
	workspaceEl.innerHTML = "";
	emptyStateEl = document.createElement("div");
	emptyStateEl.className = "empty-state";
	emptyStateEl.id = "empty-state";
	emptyStateEl.innerHTML = `<span>No runtime instances</span><div class="empty-state-wrapper"><button id="empty-state-new" class="empty-state-button" type="button">+ New Instance</button><div id="empty-state-menu" class="add-process-menu empty-state-menu">${addProcessMenu.innerHTML}</div></div>`;
	workspaceEl.appendChild(emptyStateEl);
	wireEmptyStateButton();
	workspaceEl.style.display = "";

	editorPanel = null;
	editorContainer = null;
	editorLabelEl = null;
	editorHintEl = null;
	outputEl = null;
	runButtonEl = null;
}

/* Process management */
function getActiveProcess(): Process | undefined {
	return processes.find((p) => p.id === activeProcessId);
}

function createProcess(language: Language, exampleIndex = 0): Process {
	const config = LANGUAGE_CONFIG[language];
	const example = config.examples[exampleIndex] ?? config.examples[0];
	const count = processes.filter((p) => p.language === language).length + 1;
	const proc: Process = {
		id: `proc_${nextProcessId++}`,
		language,
		name: `${config.label} ${count}`,
		code: example.code,
		outputLines: [],
		isRunning: false,
	};
	processes.push(proc);
	return proc;
}

function removeProcess(id: string): void {
	const index = processes.findIndex((p) => p.id === id);
	if (index === -1) return;
	processes.splice(index, 1);

	if (activeProcessId === id) {
		if (processes.length > 0) {
			const newIndex = Math.min(index, processes.length - 1);
			switchToProcess(processes[newIndex].id);
		} else {
			activeProcessId = null;
			hideWorkspacePanels();
		}
	}

	renderProcessList();
}

function switchToProcess(id: string): void {
	if (!monaco) return;

	/* Save current editor content */
	const current = getActiveProcess();
	if (current && editor) {
		current.code = editor.getValue();
	}

	activeProcessId = id;
	const proc = getActiveProcess();
	if (!proc) return;

	ensureWorkspacePanels();
	renderProcessList();

	/* Update editor */
	if (editor && editorContainer) {
		editor.setValue(proc.code);
		const model = editor.getModel();
		if (model) {
			monaco.editor.setModelLanguage(
				model,
				LANGUAGE_CONFIG[proc.language].monacoLanguage,
			);
		}
	} else if (editorContainer) {
		createEditor(proc);
	}

	updateEditorLabels(proc);
	renderProcessOutput(proc);
	updateRunButton(proc);
}

function createEditor(proc: Process): void {
	if (!monaco || !editorContainer) return;
	editor = monaco.editor.create(editorContainer, {
		automaticLayout: true,
		fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Consolas, monospace',
		fontLigatures: true,
		fontSize: 13,
		lineHeight: 20,
		minimap: { enabled: false },
		padding: { top: 16, bottom: 16 },
		roundedSelection: true,
		scrollBeyondLastLine: false,
		smoothScrolling: true,
		tabSize: 2,
		theme: "sandbox-agent-dark",
		value: proc.code,
		language: LANGUAGE_CONFIG[proc.language].monacoLanguage,
	});
	editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter, () => {
		const active = getActiveProcess();
		if (active) void executeProcess(active);
	});
}

function updateEditorLabels(proc: Process): void {
	const config = LANGUAGE_CONFIG[proc.language];
	if (editorLabelEl) editorLabelEl.textContent = config.label;
	if (editorHintEl) editorHintEl.textContent = config.hint;
}

function buildMenuHTML(): string {
	const languages: Language[] = ["nodejs"];
	return languages
		.map((lang) => {
			const config = LANGUAGE_CONFIG[lang];
			const submenuItems = config.examples
				.map(
					(ex, i) =>
						`<button class="add-process-submenu-item" data-language="${lang}" data-example="${i}" type="button">${ex.name}</button>`,
				)
				.join("");
			return `<div class="add-process-menu-item" data-language="${lang}">
				<span class="lang-icon">${LANGUAGE_ICONS[lang]}</span>
				<span>${config.label}</span>
				<span class="submenu-arrow">\u203a</span>
				<div class="add-process-submenu">${submenuItems}</div>
			</div>`;
		})
		.join("");
}

function wireMenuClicks(menu: HTMLElement, onDone: () => void): void {
	menu.addEventListener("click", (e) => {
		const target = (e.target as HTMLElement).closest<HTMLButtonElement>(
			".add-process-submenu-item",
		);
		if (!target) return;
		e.stopPropagation();
		const language = target.dataset.language as Language;
		const exampleIndex = Number(target.dataset.example ?? "0");
		onDone();
		const proc = createProcess(language, exampleIndex);
		switchToProcess(proc.id);
	});
}

function wireEmptyStateButton(): void {
	const btn = document.getElementById("empty-state-new");
	const menu = document.getElementById("empty-state-menu");
	if (!btn || !menu) return;
	if (!menu.innerHTML.trim()) menu.innerHTML = buildMenuHTML();
	btn.addEventListener("click", (e) => {
		e.stopPropagation();
		menu.classList.toggle("open");
	});
	wireMenuClicks(menu, () => menu.classList.remove("open"));
}

/* Add-process menu */
function setupAddProcessMenu(): void {
	addProcessMenu.innerHTML = buildMenuHTML();

	addProcessButton.addEventListener("click", (e) => {
		e.stopPropagation();
		addProcessMenu.classList.toggle("open");
	});

	document.addEventListener("click", () => {
		addProcessMenu.classList.remove("open");
		const esMenu = document.getElementById("empty-state-menu");
		if (esMenu) esMenu.classList.remove("open");
	});

	wireMenuClicks(addProcessMenu, () => addProcessMenu.classList.remove("open"));
}

/* Init */
async function init(): Promise<void> {
	setStatus("Loading Monaco...");
	await waitForGlobal(() => window.ts, "TypeScript transpiler");
	monaco = await loadMonaco();
	applyMonacoTheme(monaco);

	/* Configure TypeScript language service for Node.js module resolution */
	monaco.languages.typescript.typescriptDefaults.setCompilerOptions({
		target: 99 /* ScriptTarget.ESNext */,
		module: 1 /* ModuleKind.CommonJS */,
		moduleResolution: 2 /* ModuleResolutionKind.Node */,
		strict: true,
		esModuleInterop: true,
		allowSyntheticDefaultImports: true,
		allowJs: true,
	});
	monaco.languages.typescript.typescriptDefaults.setDiagnosticsOptions({
		noSemanticValidation: true,
	});

	setupAddProcessMenu();
	wireEmptyStateButton();

	setStatus("Editor ready", "ready");

	/* Pre-initialize the runtime in the background */
	prewarming = true;
	void Promise.all([ensureNodejsRuntime().catch(() => {})]).then(() => {
		prewarming = false;
		setStatus("Ready", "ready");
	});
}

window.addEventListener("beforeunload", () => {
	void Promise.resolve(nodejsRuntimePromise)
		.then((runtime) => runtime?.terminate?.())
		.catch(() => {});
});

init().catch((error) => {
	setStatus("Editor failed to load", "error");
	const proc = getActiveProcess();
	if (proc) {
		appendOutputToProcess(
			proc,
			"stderr",
			error instanceof Error ? error.message : String(error),
		);
	}
});
