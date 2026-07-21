'use client';

import { useEffect, useId, useMemo, useLayoutEffect, useRef, useState } from 'react';
import { Code2, Play, RotateCcw, SquarePen } from 'lucide-react';
import { motion, useReducedMotion } from 'framer-motion';
import { InkPanel } from '../editorial/InkPanel';

// ---------------------------------------------------------------------------
// Recorded agent coding sessions, played back line by line inside an ink
// terminal. A segmented control picks the runtime and replays the same task
// with the agent writing that language; the active runtime's description and
// docs link sit under the control and swap with it. The Python session runs
// the interpreter natively (no shell lines); Node and bash keep their commands.
// ---------------------------------------------------------------------------

type SessionLineKind = 'user' | 'agent' | 'cmd' | 'run' | 'out' | 'script';

interface SessionLine {
	kind: SessionLineKind;
	text: string;
}

type ScriptLang = 'js' | 'python' | 'bash';

interface SessionTab {
	key: string;
	title: string;
	description: string;
	docsHref: string;
	docsLabel: string;
	iconSrc: string;
	script: { fileName: string; lang: ScriptLang; code: string };
	codeView: { fileName: string; code: string };
	session: SessionLine[];
}

// One task, three languages: fetch last week's issues and roll them up. The
// label counts sum to the reported total (9+6+5+3 = 23), and the bash tab
// lists them alphabetically because its jq pipeline sorts.
const REPORT_JS = `const since = new Date(Date.now() - 7 * 864e5).toISOString();
const url = \`https://api.github.com/repos/acme/shop/issues?since=\${since}\`;
const issues = await (await fetch(url)).json();

const counts = {};
for (const { labels } of issues)
  for (const { name } of labels) counts[name] = (counts[name] ?? 0) + 1;

const rows = Object.entries(counts).sort((a, b) => b[1] - a[1]);
const report = [
  \`# Issues, last 7 days: \${issues.length}\`,
  ...rows.map(([label, n]) => \`- \${label}: \${n}\`),
].join("\\n");
console.log(report);`;

const REPORT_PY = `import json, datetime, urllib.request
from collections import Counter

since = (datetime.date.today() - datetime.timedelta(days=7)).isoformat()
url = f"https://api.github.com/repos/acme/shop/issues?since={since}"
issues = json.load(urllib.request.urlopen(url))

counts = Counter(l["name"] for i in issues for l in i["labels"])

lines = [f"# Issues, last 7 days: {len(issues)}"]
lines += [f"- {label}: {n}" for label, n in counts.most_common()]
report = "\\n".join(lines)
print(report)`;

const REPORT_SH = `#!/bin/bash
set -euo pipefail

since=$(date -u -d '7 days ago' +%Y-%m-%d)
url="https://api.github.com/repos/acme/shop/issues?since=$since"
curl -s "$url" | jq -r '
  "# Issues, last 7 days: \\(length)",
  ([.[].labels[].name] | sort | group_by(.)
   | .[] | "- \\(.[0]): \\(length)")'`;

const REPORT_OUT = ['- bug: 9', '- api: 6', '- ui: 5', '- docs: 3'];

const GITHUB_BINDING_SOURCE = `import { AgentOs, binding, bindings } from "@rivet-dev/agentos-core";
import { z } from "zod";

const github = bindings({
  name: "github",
  description: "Read GitHub data with credentials held by the host.",
  bindings: {
    "list-issues": binding({
      description: "List issues updated in the last number of days.",
      inputSchema: z.object({ days: z.number().int().min(1) }),
      execute: async ({ days }) => {
        const since = new Date(Date.now() - days * 864e5).toISOString();
        const response = await fetch(
          \`https://api.github.com/repos/acme/shop/issues?since=\${since}\`,
          {
            headers: {
              Accept: "application/vnd.github+json",
              Authorization: \`Bearer \${process.env.GITHUB_TOKEN}\`,
            },
          },
        );
        if (!response.ok) throw new Error(\`GitHub returned \${response.status}\`);
        const issues = await response.json();
        return { total: Array.isArray(issues) ? issues.length : 0, issues };
      },
    }),
  },
});`;

const EXECUTE_JS = `${GITHUB_BINDING_SOURCE}

const vm = await AgentOs.create({
  bindings: [github],
  permissions: { fs: "allow", childProcess: "allow", binding: "allow" },
});
try {
  await vm.writeFile("/tmp/report.mjs", \`
import { execFileSync } from "node:child_process";

const { result } = JSON.parse(execFileSync(
  "agentos-github",
  ["list-issues", "--days", "7"],
  { encoding: "utf8" },
));
console.log("# Issues, last 7 days: " + result.total);
\`);
  const result = await vm.exec("node /tmp/report.mjs");
  console.log(result.stdout);
} finally {
  await vm.dispose();
}`;

const EXECUTE_PYTHON = `${GITHUB_BINDING_SOURCE}

const vm = await AgentOs.create({
  bindings: [github],
  permissions: { fs: "allow", childProcess: "allow", binding: "allow" },
});
try {
  await vm.writeFile("/tmp/report.py", \`
import json
import subprocess

payload = subprocess.check_output(
    ["agentos-github", "list-issues", "--days", "7"],
    text=True,
)
result = json.loads(payload)["result"]
print(f"# Issues, last 7 days: {result['total']}")
\`);
  const result = await vm.exec("python /tmp/report.py");
  console.log(result.stdout);
} finally {
  await vm.dispose();
}`;

const EXECUTE_BASH = `${GITHUB_BINDING_SOURCE}

const vm = await AgentOs.create({
  bindings: [github],
  permissions: { fs: "allow", childProcess: "allow", binding: "allow" },
});
try {
  await vm.writeFile("/tmp/report.sh", \`#!/bin/bash
set -euo pipefail

payload="$(agentos-github list-issues --days 7)"
total="$(printf '%s' "$payload" | grep -o '"total":[0-9]*' | cut -d: -f2)"
printf '# Issues, last 7 days: %s\\n' "$total"
\`);
  const result = await vm.exec("bash /tmp/report.sh");
  console.log(result.stdout);
} finally {
  await vm.dispose();
}`;

const shellSession = (runCmd: string, reportLines: string[]): SessionLine[] => [
	{ kind: 'user', text: "generate a report of last week's issues" },
	{ kind: 'agent', text: 'Writing a script to fetch them and build the report.' },
	{ kind: 'script', text: '' },
	{ kind: 'cmd', text: runCmd },
	{ kind: 'out', text: '# Issues, last 7 days: 23' },
	...reportLines.map((text): SessionLine => ({ kind: 'out', text })),
	{ kind: 'agent', text: '23 issues last week; bug reports lead with 9.' },
];

// JavaScript and Python run on their native runtimes through the exec API:
// a run step replaces shell lines and the script prints the report it wrote.
// Only bash shows shell commands, because bash is the shell.
const runSession = (runLabel: string): SessionLine[] => [
	{ kind: 'user', text: "generate a report of last week's issues" },
	{ kind: 'agent', text: 'Writing a script to fetch them and build the report.' },
	{ kind: 'script', text: '' },
	{ kind: 'run', text: runLabel },
	{ kind: 'out', text: '# Issues, last 7 days: 23' },
	...REPORT_OUT.map((text): SessionLine => ({ kind: 'out', text })),
	{ kind: 'agent', text: '23 issues last week; bug reports lead with 9.' },
];

const TABS: SessionTab[] = [
	{
		key: 'nodejs',
		title: 'Node.js',
		description: 'Node v22 compatible, running on WASM. node, npm, and npx on the PATH.',
		docsHref: '/docs/javascript',
		docsLabel: 'Node.js runtime docs',
		iconSrc: '/images/registry/nodejs.svg',
		script: { fileName: 'report.mjs', lang: 'js', code: REPORT_JS },
		codeView: { fileName: 'execute-javascript.ts', code: EXECUTE_JS },
		session: runSession('Run report.mjs'),
	},
	{
		key: 'python',
		title: 'Python',
		description: 'CPython 3.13 with pip. Native wheels like numpy and pandas work.',
		docsHref: '/docs/python',
		docsLabel: 'Python runtime docs',
		iconSrc: 'https://upload.wikimedia.org/wikipedia/commons/thumb/3/31/Python-logo.png/120px-Python-logo.png',
		script: { fileName: 'report.py', lang: 'python', code: REPORT_PY },
		codeView: { fileName: 'execute-python.ts', code: EXECUTE_PYTHON },
		session: runSession('Run report.py'),
	},
	{
		key: 'bash',
		title: 'Bash',
		description: 'A POSIX userland with a process table, PTYs, TCP and UDP with DNS, and deny-by-default permissions.',
		docsHref: '/docs/processes',
		docsLabel: 'Processes & shell docs',
		iconSrc: '/images/registry/bash.svg',
		script: { fileName: 'report.sh', lang: 'bash', code: REPORT_SH },
		codeView: { fileName: 'execute-bash.ts', code: EXECUTE_BASH },
		session: shellSession('bash report.sh', ['- api: 6', '- bug: 9', '- docs: 3', '- ui: 5']),
	},
];

const WINDOW_TITLE = 'agentOS VM Execution';
const CAPTION = 'The agent writes one program instead of a chain of tool calls.';
const CODE_CAPTION = 'Run Bash, Node.js, and Python through the same agentOS API.';

// --- Tiny dark-palette tokenizer for the script block ---------------------
// The site's highlightCodeHtml is tuned for light panels and JS only; the
// script hero sits on ink and covers three languages, so it gets its own
// minimal pass: comments, strings, keywords, and shell variables.

type TokenType = 'kw' | 'str' | 'com' | 'var' | 'text';

interface Token {
	type: TokenType;
	value: string;
}

const TOKEN_CLASS: Record<Exclude<TokenType, 'text'>, string> = {
	kw: 'text-sage',
	str: 'text-[#CFA379]',
	com: 'italic text-cream/40',
	var: 'text-[#CFA379]',
};

const SCRIPT_RULES: Record<ScriptLang, RegExp> = {
	js: /(\/\/.*)|(`(?:\\.|[^`])*`|"(?:\\.|[^"])*"|'(?:\\.|[^'])*')|()\b(import|from|const|let|var|await|async|function|return|new|for|of|if|else|export)\b/g,
	python:
		/(#.*)|((?:[frbFRB]{1,2})?(?:"(?:\\.|[^"])*"|'(?:\\.|[^'])*'))|()\b(import|from|def|return|for|in|if|else|with|as|class|lambda)\b/g,
	bash: /(#.*)|("(?:\\.|[^"])*"|'[^']*')|(\$\{?\w+\}?)|\b(set|echo|if|then|else|fi|for|do|done)\b/g,
};

// Tokenizes the whole script (strings may span lines), then splits into
// per-line token runs for rendering.
const tokenizeScript = (code: string, lang: ScriptLang): Token[][] => {
	const rule = new RegExp(SCRIPT_RULES[lang].source, 'g');
	const tokens: Token[] = [];
	let last = 0;
	for (let match = rule.exec(code); match; match = rule.exec(code)) {
		if (match.index > last) tokens.push({ type: 'text', value: code.slice(last, match.index) });
		const [, com, str, shVar, kw] = match;
		const type: TokenType = com !== undefined ? 'com' : str !== undefined ? 'str' : shVar ? 'var' : kw ? 'kw' : 'text';
		tokens.push({ type, value: match[0] });
		last = match.index + match[0].length;
	}
	if (last < code.length) tokens.push({ type: 'text', value: code.slice(last) });

	const lines: Token[][] = [[]];
	for (const token of tokens) {
		token.value.split('\n').forEach((part, idx) => {
			if (idx > 0) lines.push([]);
			if (part) lines[lines.length - 1].push({ type: token.type, value: part });
		});
	}
	return lines;
};

// --- Playback --------------------------------------------------------------
// Pacing in clock ms. `user` and `cmd` lines type per character; the rest
// appear whole after their lead-in. The script keeps the longest pause so the
// eye can land on it; everything else is brisk.
const START_DELAY = 250;
const RUN_DELAY = 350;
const LEAD: Record<SessionLineKind, number> = { user: 200, agent: 550, cmd: 480, run: 600, out: 60, script: 700 };
const CHAR_MS: Partial<Record<SessionLineKind, number>> = { user: 17, cmd: 8 };

interface ScheduledLine extends SessionLine {
	start: number;
	dur: number;
}

const buildSchedule = (lines: SessionLine[]): { schedule: ScheduledLine[]; total: number } => {
	let t = START_DELAY;
	const schedule = lines.map((line, i) => {
		const prev = lines[i - 1]?.kind;
		const lead = line.kind === 'out' && (prev === 'cmd' || prev === 'run') ? RUN_DELAY : LEAD[line.kind];
		const start = t + lead;
		const dur = (CHAR_MS[line.kind] ?? 0) * line.text.length;
		t = start + dur;
		return { ...line, start, dur };
	});
	return { schedule, total: t };
};

// Drives the playback clock with an accumulated per-frame delta (capped so a
// backgrounded tab resumes where it paused instead of jumping to the end).
// The reset runs in a layout effect so a tab switch never flashes the new
// transcript fully-played before the clock restarts.
const usePlaybackClock = (total: number, running: boolean, playKey: number, skipToEnd: boolean) => {
	const [clock, setClock] = useState(skipToEnd ? total : 0);

	useLayoutEffect(() => {
		if (skipToEnd) {
			setClock(total);
			return;
		}
		if (!running) return;
		setClock(0);
		let raf = 0;
		let last = performance.now();
		let elapsed = 0;
		const step = (now: number) => {
			elapsed += Math.min(now - last, 100);
			last = now;
			setClock(elapsed);
			if (elapsed < total) raf = requestAnimationFrame(step);
		};
		raf = requestAnimationFrame(step);
		return () => cancelAnimationFrame(raf);
	}, [total, running, playKey, skipToEnd]);

	return clock;
};

const Cursor = ({ blink }: { blink?: boolean }) => (
	<span
		aria-hidden='true'
		className={`-mb-0.5 ml-px inline-block h-[1.05em] w-[7px] translate-y-[2px] bg-cream/80 ${
			blink ? 'animate-[session-cursor-blink_1.1s_steps(2,jump-none)_infinite]' : ''
		}`}
	/>
);

// The hero: the program the agent wrote, whole and syntax-lit, under a
// "Write <file>" header. Deliberately not typed out.
const ScriptBlock = ({ fileName, tokenLines }: { fileName: string; tokenLines: Token[][] }) => (
	<div className='my-1.5 ml-[1.35rem] overflow-hidden rounded-lg border border-cream/[0.14] bg-cream/[0.045]'>
		<div className='flex items-center gap-1.5 border-b border-cream/10 px-3 py-1.5 text-[11px] text-cream/60'>
			<SquarePen aria-hidden='true' className='h-3 w-3 text-cream/40' />
			Write {fileName}
		</div>
		<pre className='overflow-x-auto px-3 py-2.5 text-[12px] leading-[1.65] text-cream/85'>
			{tokenLines.map((tokens, i) => (
				<div key={i} className='whitespace-pre'>
					{tokens.length === 0
						? ' '
						: tokens.map((token, j) =>
								token.type === 'text' ? (
									token.value
								) : (
									<span key={j} className={TOKEN_CLASS[token.type]}>
										{token.value}
									</span>
								),
							)}
				</div>
			))}
		</pre>
	</div>
);

const FullCodeView = ({ tokenLines }: { tokenLines: Token[][] }) => (
	<div className='h-[440px] overflow-auto p-6 font-code text-[13px] leading-[1.75] [font-variant-ligatures:none] md:h-[560px]'>
		<div>
			{tokenLines.map((tokens, i) => (
				<div key={i} className='grid grid-cols-[2rem_1fr] gap-3'>
					<span aria-hidden='true' className='select-none text-right text-cream/20'>{i + 1}</span>
					<code className='whitespace-pre text-cream/85'>
						{tokens.length === 0
							? ' '
							: tokens.map((token, j) =>
									token.type === 'text' ? (
										token.value
									) : (
										<span key={j} className={TOKEN_CLASS[token.type]}>
											{token.value}
										</span>
									),
							)}
					</code>
				</div>
			))}
		</div>
	</div>
);

// A native tool step ("Run report.py"): the interpreter runs in the VM
// directly, so there is no shell line to type.
const RunBlock = ({ text }: { text: string }) => (
	<div className='my-1.5 ml-[1.35rem] flex w-fit items-center gap-1.5 rounded-lg border border-cream/[0.14] bg-cream/[0.045] px-3 py-1.5 text-[11px] text-cream/60'>
		<Play aria-hidden='true' className='h-3 w-3 text-sage' />
		{text}
	</div>
);

// One transcript row. Typed rows render a partial slice with the cursor at
// the write head; whole rows fade in on mount.
const SessionRow = ({
	line,
	chars,
	typing,
	script,
	tokenLines,
}: {
	line: ScheduledLine;
	chars: number;
	typing: boolean;
	script: SessionTab['script'];
	tokenLines: Token[][];
}) => {
	const typed = line.dur > 0;
	const text = typed ? line.text.slice(0, chars) : line.text;

	const body = (() => {
		switch (line.kind) {
			case 'user':
				return (
					<span className='text-cream'>
						<span aria-hidden='true' className='mr-2.5 select-none text-accent'>
							›
						</span>
						{text}
						{typing && <Cursor />}
					</span>
				);
			case 'agent':
				return (
					<span className='text-cream/80'>
						<span
							aria-hidden='true'
							className='mr-2.5 -mt-px inline-block h-[7px] w-[7px] rounded-full bg-sage align-middle'
						/>
						{text}
					</span>
				);
			case 'cmd':
				return (
					<span className='text-cream/90'>
						<span aria-hidden='true' className='mr-2.5 select-none text-sage'>
							$
						</span>
						{text}
						{typing && <Cursor />}
					</span>
				);
			case 'run':
				return <RunBlock text={line.text} />;
			case 'out':
				return <span className='block whitespace-pre-wrap pl-[1.35rem] text-cream/45'>{text}</span>;
			case 'script':
				return <ScriptBlock fileName={script.fileName} tokenLines={tokenLines} />;
		}
	})();

	if (typed) return <div>{body}</div>;
	return (
		<motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={{ duration: 0.18 }}>
			{body}
		</motion.div>
	);
};

// The runtime cards are tabs: real buttons with no interactive content nested
// inside them. The active runtime's docs link sits below the demo window.
const RuntimeTabs = ({ active, onChange }: { active: number; onChange: (idx: number) => void }) => {
	const indicatorId = useId();
	return (
		<div className='mb-5'>
			<div
				role='tablist'
				aria-label='Execution runtimes'
				className='scrollbar-hide flex w-full min-w-0 flex-nowrap gap-1 overflow-x-auto rounded-full border border-ink/15 bg-ink/[0.025] p-1.5'
			>
				{TABS.map((t, idx) => {
					const selected = active === idx;
					return (
						<button
							key={t.key}
							type='button'
							role='tab'
							aria-selected={selected}
							onClick={() => onChange(idx)}
							className={`relative flex min-w-max flex-1 shrink-0 items-center justify-center gap-2 rounded-full px-4 py-2.5 text-sm transition-colors ${
								selected ? 'font-medium text-ink' : 'text-ink-faint hover:text-ink'
							}`}
						>
							{selected && (
								<motion.span
									layoutId={`runtime-tab-${indicatorId}`}
									className='absolute inset-0 rounded-full bg-white shadow-sm ring-1 ring-inset ring-ink/15'
									transition={{ type: 'spring', stiffness: 480, damping: 38 }}
								/>
							)}
							<span className='relative z-[1] flex items-center gap-2'>
								<img
									src={t.iconSrc}
									alt=''
									aria-hidden='true'
									className='h-4 w-4 object-contain'
								/>
								{t.title}
							</span>
						</button>
					);
				})}
			</div>
		</div>
	);
};

export const AgentSessionDemo = () => {
	const reduced = useReducedMotion() ?? false;
	const [active, setActive] = useState(0);
	const [started, setStarted] = useState(false);
	const [playKey, setPlayKey] = useState(0);
	const [showCode, setShowCode] = useState(false);
	const scrollRef = useRef<HTMLDivElement>(null);

	const tab = TABS[active];
	const { schedule, total } = useMemo(() => buildSchedule(tab.session), [tab]);
	const tokenLines = useMemo(() => tokenizeScript(tab.script.code, tab.script.lang), [tab]);
	const codeViewTokenLines = useMemo(() => tokenizeScript(tab.codeView.code, 'js'), [tab]);
	const clock = usePlaybackClock(total, started, playKey, reduced);

	const visible = schedule.filter((line) => clock >= line.start);
	const done = clock >= total;

	const replay = () => {
		setStarted(true);
		setPlayKey((k) => k + 1);
	};

	const handleTabChange = (idx: number) => {
		setActive(idx);
		replay();
	};

	const toggleCodeView = () => {
		if (showCode) replay();
		setShowCode((visible) => !visible);
	};

	// Keep the newest line in view while the session plays. Instant, not
	// smooth: smooth scrolling lags behind per-frame typing updates.
	useEffect(() => {
		const el = scrollRef.current;
		if (!showCode && el) el.scrollTop = el.scrollHeight;
	}, [visible.length, clock, done, showCode]);

	return (
		<motion.div
			onViewportEnter={() => setStarted(true)}
			viewport={{ once: true, margin: '-20% 0px' }}
		>
			<RuntimeTabs active={active} onChange={handleTabChange} />

			<InkPanel caption={showCode ? CODE_CAPTION : CAPTION}>
				<div className='flex items-center gap-2 border-b border-cream/10 px-4 py-3'>
					<div className='h-3 w-3 rounded-full bg-cream/15' />
					<div className='h-3 w-3 rounded-full bg-cream/15' />
					<div className='h-3 w-3 rounded-full bg-cream/15' />
					<span className={`ml-2 hidden text-xs text-cream/65 sm:inline ${showCode ? 'font-code' : 'font-medium'}`}>{showCode ? tab.codeView.fileName : WINDOW_TITLE}</span>
					<div className='ml-auto flex items-center gap-1'>
						{!showCode && !reduced && (
							<button
								type='button'
								onClick={replay}
								aria-label='Replay session'
								className='flex h-7 w-7 items-center justify-center rounded-md text-cream/40 transition-colors hover:bg-cream/[0.06] hover:text-cream/80'
							>
								<RotateCcw className='h-3.5 w-3.5' />
							</button>
						)}
						<button
							type='button'
							onClick={toggleCodeView}
							aria-pressed={showCode}
							className='inline-flex h-7 items-center gap-1.5 rounded-md border border-cream/25 bg-cream/[0.1] px-2.5 text-[11px] font-medium text-cream/90 transition-colors hover:border-cream/35 hover:bg-cream/[0.16] hover:text-cream'
						>
							<Code2 className='h-3.5 w-3.5' />
							{showCode ? 'Show agent run' : 'Show me the code'}
						</button>
					</div>
				</div>

				{/* Tall enough on desktop to hold a whole session, script and all; if
				    wrapped lines overflow on small screens the auto-scroll keeps the
				    newest line in view. Ligatures off: a terminal shows `--` and
				    `->` as raw ASCII. */}
				{showCode ? (
					<FullCodeView tokenLines={codeViewTokenLines} />
				) : (
					<div
						ref={scrollRef}
						className='h-[440px] overflow-y-auto p-6 font-code text-[13px] leading-[1.75] [font-variant-ligatures:none] md:h-[560px]'
					>
						<div className='flex flex-col gap-0.5'>
							{visible.map((line, i) => {
								const typing = line.dur > 0 && clock < line.start + line.dur;
								const chars = line.dur > 0 ? Math.ceil(((clock - line.start) / line.dur) * line.text.length) : line.text.length;
								return (
									<SessionRow
										key={`${tab.key}-${i}`}
										line={line}
										chars={Math.min(chars, line.text.length)}
										typing={typing}
										script={tab.script}
										tokenLines={tokenLines}
									/>
								);
							})}
							{/* Idle prompt once the session finishes: the machine is still there. */}
							{done && !reduced && (
								<motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} transition={{ duration: 0.18, delay: 0.5 }}>
									<span aria-hidden='true' className='mr-2.5 select-none text-sage'>
										$
									</span>
									<Cursor blink />
								</motion.div>
							)}
						</div>
					</div>
				)}
			</InkPanel>
			<div className='mt-4 flex justify-end'>
				<a href={tab.docsHref} className='whitespace-nowrap text-sm text-accent-deep underline underline-offset-2 transition-colors hover:text-accent'>
					{tab.docsLabel} <span aria-hidden='true'>→</span>
				</a>
			</div>
		</motion.div>
	);
};
