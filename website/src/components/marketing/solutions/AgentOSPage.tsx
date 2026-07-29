'use client';

import { useId, useState, useEffect, useRef, useCallback, useMemo } from 'react';
import {
	ArrowRight,
	Layers,
	Wrench,
	ExternalLink,
	Activity,
	HardDrive,
	Users,
	Workflow,
	ChevronLeft,
	ChevronRight,
	Copy,
	Check,
	ShieldCheck,
	Package,
	Server,
	GitFork,
	RefreshCw,
	Moon,
	Blocks,
	ChartNoAxesCombined,
	Code2,
	Globe,
	CalendarClock,
	Gauge,
	AppWindow,
	Database,
	Rocket,
} from 'lucide-react';
import { AnimatePresence, motion, useReducedMotion } from 'framer-motion';
import { InkPanel } from '../editorial/InkPanel';
import { registry } from '../../../data/registry';
import { REGISTRY_ICONS } from '../../../data/registry-icons';
import { DEPLOY_TARGETS } from '../../../data/deploy-targets';
import { AGENT_PROMPT } from '../agentPrompt';
import { SectionHeading } from '../typography';
import { ColdStartTimeline } from '../diagrams/ColdStartTimeline';
import { AgentSessionDemo } from '../diagrams/AgentSessionDemo';
import { AgentOsTopologyCell, SandboxTopologyCell } from '../diagrams/TopologyCells';
import { Reveal } from '../motion';

// Premium porcelain card surface, shared by the architecture cards and the
// feature bento: a single top-down gradient, a hairline ring, an inset top
// edge-light, and a soft layered drop shadow. Hover deepens the shadow and
// lightens the ring with no transform, so it stays reduced-motion safe.
const CARD_SURFACE =
	'rounded-2xl bg-gradient-to-b from-white to-[#f9f9fa] ring-1 ring-ink/[0.08] ' +
	'shadow-[inset_0_1px_0_rgba(255,255,255,0.9),0_1px_2px_-1px_rgba(20,20,22,0.10),0_8px_24px_-14px_rgba(20,20,22,0.20)] ' +
	'transition-[box-shadow,--tw-ring-color] duration-300 ' +
	'hover:ring-ink/[0.14] hover:shadow-[inset_0_1px_0_rgba(255,255,255,0.95),0_2px_4px_-1px_rgba(20,20,22,0.12),0_12px_30px_-12px_rgba(20,20,22,0.26)] ' +
	'motion-reduce:transition-none';

// The page reads best at ~90% browser zoom, so it ships that density: zoom
// scales layout (unlike transform), and browsers without support render at
// 100%. The hero logo counter-zooms; see its wrapper.
const PAGE_ZOOM = 0.9;

interface HeroTabCode {
	key: string;
	fileName: string;
	code: string;
	highlightedCode: string;
}

interface AgentOSPageProps {
	heroTabs: HeroTabCode[];
	filesystemHighlightedCode: string;
	appsHighlightedCode: string;
}

// --- Animated agentOS Logo ---
interface AnimatedAgentOSLogoProps {
	className?: string;
	displayedAgent?: { src: string; name: string } | null;
	// Seconds for the main stroke-draw. Defaults to 3; pass a smaller value for
	// a faster intro (e.g. the v0.2 announcement card).
	drawDurationSec?: number;
}

export const AnimatedAgentOSLogo = ({ className, displayedAgent, drawDurationSec = 3 }: AnimatedAgentOSLogoProps) => {
	const containerRef = useRef<HTMLDivElement>(null);
	const [isReady, setIsReady] = useState(false);
	const osLayerRef = useRef<Element | null>(null);
	const agentImageRef = useRef<SVGImageElement | null>(null);

	useEffect(() => {
		const container = containerRef.current;
		if (!container) return;

		fetch('/images/agent-os/agentos-hero-logo-animated.svg')
			.then((res) => res.text())
			.then((svgText) => {
				container.innerHTML = svgText;

				const svg = container.querySelector('svg');
				if (!svg) return;

				svg.removeAttribute('width');
				svg.removeAttribute('height');
				svg.style.height = '100%';
				svg.style.width = 'auto';
				svg.style.display = 'block';

				const ns = 'http://www.w3.org/2000/svg';
				const textLayer = svg.querySelector('#text-layer');
				const strokeLayer = svg.querySelector('#stroke-layer');
				if (!textLayer || !strokeLayer) return;

				// Find and store reference to the OS layer (contains the "OS" text)
				const osLayer = svg.querySelector('#os-layer');
				if (osLayer) {
					osLayerRef.current = osLayer;
					// Set up transition for smooth opacity changes
					(osLayer as HTMLElement).style.transition = 'opacity 0.15s ease-out';
				}

				// Create agent image element inside the os-layer's parent, positioned like os-layer
				// The image will be positioned to appear inside the squircle where "OS" is
				const agentImg = document.createElementNS(ns, 'image');
				agentImg.setAttribute('id', 'agent-logo');
				// Position inside the squircle (viewBox is 0 0 305 102, squircle is on the right)
				agentImg.setAttribute('width', '32');
				agentImg.setAttribute('height', '32');
				agentImg.setAttribute('x', '249');
				agentImg.setAttribute('y', '25');
				agentImg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
				agentImg.style.opacity = '0';
				agentImg.style.transition = 'opacity 0.15s ease-out';
				svg.appendChild(agentImg);
				agentImageRef.current = agentImg;

				const strokePath = strokeLayer.querySelector('path');
				if (!strokePath) return;

				// The wordmark reveals the real glyphs through a mask that follows
				// the pen path, so the finished logo is the brand mark itself and
				// nothing settles or swaps after the pen stops. Three strokes per
				// subpath keep the reveal honest at self-intersections: a wide white
				// stroke reveals the written portion, a black stroke over the
				// unwritten remainder conceals ink the pen has not reached yet (the
				// hanging edges at crossings), and a narrower white stroke on top
				// restores the written line where the unwritten portion crosses it.
				// pathLength=100 normalizes the dash math so one keyframe pair
				// serves every stroke regardless of geometric length.
				const fullD = strokePath.getAttribute('d') || '';
				const lastM = fullD.lastIndexOf('M');
				const mainD = fullD.substring(0, lastM);
				const tailD = fullD.substring(lastM);

				const defs = document.createElementNS(ns, 'defs');
				svg.insertBefore(defs, svg.firstChild);
				const mask = document.createElementNS(ns, 'mask');
				mask.setAttribute('id', 'agentos-reveal-mask');
				mask.setAttribute('maskUnits', 'userSpaceOnUse');
				mask.setAttribute('x', '0');
				mask.setAttribute('y', '0');
				mask.setAttribute('width', '99999');
				mask.setAttribute('height', '99999');
				const groupTransform = strokeLayer.getAttribute('transform') || '';
				const makeMaskStroke = (d: string, color: string, width: number, cap: string) => {
					const group = document.createElementNS(ns, 'g');
					group.setAttribute('transform', groupTransform);
					const path = document.createElementNS(ns, 'path');
					path.setAttribute('d', d);
					path.setAttribute('pathLength', '100');
					path.setAttribute(
						'style',
						`fill:none; stroke:${color}; stroke-width:${width}px; stroke-linecap:${cap}; stroke-linejoin:round;`,
					);
					// The gap exceeds the dash by 1 so no offset ever lands a
					// zero-length dash on the path end — with round caps that
					// degenerate dash renders as a floating dot.
					path.style.strokeDasharray = '100 101';
					group.appendChild(path);
					mask.appendChild(group);
					return path;
				};
				// Paint order matters: reveal below, conceal above it, restore on top.
				// The conceal stroke uses butt caps so it ends exactly at the pen tip
				// instead of eating half a cap backwards into fresh ink. The restore
				// stroke is only a hair wider than the letterform (~6.8 in this
				// space): any wider and it re-reveals concealed ink where an
				// unwritten stroke crosses a written one.
				const reveals = [makeMaskStroke(mainD, 'white', 10.57, 'round'), makeMaskStroke(tailD, 'white', 10.57, 'round')];
				const conceals = [makeMaskStroke(mainD, 'black', 10.57, 'butt'), makeMaskStroke(tailD, 'black', 10.57, 'butt')];
				const restores = [makeMaskStroke(mainD, 'white', 7.6, 'round'), makeMaskStroke(tailD, 'white', 7.6, 'round')];
				defs.appendChild(mask);

				// Wrap text layer in a masked group
				const textParent = textLayer.parentNode;
				if (textParent) {
					const wrapper = document.createElementNS(ns, 'g');
					wrapper.setAttribute('mask', 'url(#agentos-reveal-mask)');
					textParent.insertBefore(wrapper, textLayer);
					wrapper.appendChild(textLayer);
				}
				strokeLayer.remove();

				// Animate like a real pen. The word must keep drawing at near-constant
				// speed to the end: an ease-out crawl leaves the "t" sitting uncrossed
				// and half-drawn for the last ~second of the animation. The crossbar
				// follows after a brief pen lift as a quick flick.
				const mainDuration = drawDurationSec;
				const penLift = 0.09;
				const tailDuration = 0.16;

				// Add keyframes if not already present
				if (!document.querySelector('#agentos-logo-animation-style')) {
					const style = document.createElement('style');
					style.id = 'agentos-logo-animation-style';
					style.textContent = `
						@keyframes agentos-draw {
							to { stroke-dashoffset: 0; }
						}
						@keyframes agentos-conceal {
							to { stroke-dashoffset: -100; }
						}
					`;
					document.head.appendChild(style);
				}

				if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
					// Static: mask fully open, filled glyphs shown immediately.
					for (const path of [...reveals, ...restores]) path.style.strokeDashoffset = '0';
					for (const path of conceals) path.style.strokeDashoffset = '-100';
				} else {
					const timings = [
						`${mainDuration}s cubic-bezier(0.3, 0, 0.75, 0.85) 0s`,
						`${tailDuration}s cubic-bezier(0.3, 0, 0.6, 1) ${mainDuration + penLift}s`,
					];
					[reveals, restores].forEach((pair) =>
						pair.forEach((path, i) => {
							path.style.strokeDashoffset = '100.5';
							path.style.animation = `agentos-draw ${timings[i]} forwards`;
						}),
					);
					conceals.forEach((path, i) => {
						path.style.strokeDashoffset = '0';
						path.style.animation = `agentos-conceal ${timings[i]} forwards`;
					});
				}

				setIsReady(true);
			});

		return () => {
			if (container) {
				container.innerHTML = '';
			}
		};
	}, []);

	// Update OS layer and agent image visibility when displayedAgent changes
	useEffect(() => {
		if (!isReady) return;

		const osLayer = osLayerRef.current;
		const agentImg = agentImageRef.current;

		if (osLayer && agentImg) {
			if (displayedAgent) {
				// Hide OS layer, show agent logo
				(osLayer as HTMLElement).style.opacity = '0';
				agentImg.setAttributeNS('http://www.w3.org/1999/xlink', 'xlink:href', displayedAgent.src);
				agentImg.setAttribute('href', displayedAgent.src);
				agentImg.style.opacity = '1';
			} else {
				// Show OS layer, hide agent logo
				(osLayer as HTMLElement).style.opacity = '1';
				agentImg.style.opacity = '0';
			}
		}
	}, [displayedAgent, isReady]);

	return (
		<div
			ref={containerRef}
			className={className}
			style={{
				opacity: isReady ? 1 : 0,
				transition: 'opacity 0.3s ease',
			}}
		/>
	);
};

// --- Set up with your agent (copies the agent prompt) ---
const SetupWithAgent = () => {
	const [copied, setCopied] = useState(false);

	const handleCopy = async () => {
		await navigator.clipboard.writeText(AGENT_PROMPT);
		setCopied(true);
		setTimeout(() => setCopied(false), 2000);
	};

	return (
		<button
			onClick={handleCopy}
			aria-label={copied ? 'Agent setup prompt copied' : 'Set up with your agent'}
			className='selection-dark inline-flex w-full items-center justify-center gap-2 whitespace-nowrap rounded-md bg-accent-deep px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-accent sm:w-auto'
		>
			{copied ? <Check className='h-4 w-4' /> : <Copy className='h-4 w-4' />}
			{/* Reserve the width of the longest label so the button doesn't shrink on copy */}
			<span className='grid place-items-center'>
				<span className='invisible col-start-1 row-start-1' aria-hidden='true'>Set up with your agent</span>
				<span className='col-start-1 row-start-1'>{copied ? 'Copied' : 'Set up with your agent'}</span>
			</span>
		</button>
	);
};

// The install command itself is the CTA: clicking anywhere on the chip copies
// it, with an inline confirmation that does not replace the command text.
const CopyInstallCommand = () => {
	const [copied, setCopied] = useState(false);
	const command = 'npm install @rivet-dev/agentos';

	const handleCopy = async () => {
		await navigator.clipboard.writeText(command);
		setCopied(true);
		setTimeout(() => setCopied(false), 2000);
	};

	return (
		<button
			type='button'
			onClick={handleCopy}
			aria-label={copied ? 'Install command copied' : `Copy ${command}`}
			className='group relative flex w-full items-center justify-center gap-2.5 rounded-md border border-ink/15 bg-white/55 px-3.5 py-2.5 font-mono text-[13px] text-ink-soft transition-colors hover:border-ink/30 hover:bg-white sm:w-auto'
		>
			<span aria-hidden='true' className='select-none text-pine'>$</span>
			<span className='whitespace-nowrap'>{command}</span>
			<span
				aria-hidden='true'
				className={`pointer-events-none absolute bottom-full left-1/2 mb-2 -translate-x-1/2 rounded bg-ink px-2 py-1 font-sans text-xs text-white shadow-sm transition-opacity ${copied ? 'opacity-100' : 'opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100'}`}
			>
				{copied ? 'Copied' : 'Copy'}
			</span>
		</button>
	);
};

// --- Hero Tabs (scrollable with fade + arrows) ---
interface HeroTabEntry {
	key: string;
	icon?: React.ComponentType<{ className?: string }>;
	iconSrc?: string;
	label: string;
	docsHref: string;
	docsLabel?: string;
	fileName?: string;
	code?: string;
	highlightedCode?: string;
}

const HeroTabs = ({ tabs, activeTab, onTabChange }: { tabs: HeroTabEntry[]; activeTab: number; onTabChange: (idx: number) => void }) => {
	const scrollRef = useRef<HTMLDivElement>(null);
	const [canScrollLeft, setCanScrollLeft] = useState(false);
	const [canScrollRight, setCanScrollRight] = useState(false);
	// The page renders more than one tab strip; a per-instance layoutId keeps
	// each strip's active-pill animation from jumping to the other strip.
	const indicatorLayoutId = useId();

	const checkOverflow = useCallback(() => {
		const el = scrollRef.current;
		if (!el) return;
		setCanScrollLeft(el.scrollLeft > 2);
		setCanScrollRight(el.scrollLeft + el.clientWidth < el.scrollWidth - 2);
	}, []);

	useEffect(() => {
		const el = scrollRef.current;
		if (!el) return;
		checkOverflow();
		el.addEventListener('scroll', checkOverflow, { passive: true });
		const ro = new ResizeObserver(checkOverflow);
		ro.observe(el);
		return () => {
			el.removeEventListener('scroll', checkOverflow);
			ro.disconnect();
		};
	}, [checkOverflow]);

	const scroll = (direction: 'left' | 'right') => {
		const el = scrollRef.current;
		if (!el) return;
		const amount = el.clientWidth * 0.5;
		el.scrollBy({ left: direction === 'left' ? -amount : amount, behavior: 'smooth' });
	};

	// Fade the tab strip into the porcelain with a content mask rather than a
	// solid-color overlay. A flat overlay would cover the page grain and read as
	// an odd lighter band; masking the content lets the grain show through.
	const maskImage =
		canScrollLeft || canScrollRight
			? `linear-gradient(to right, ${canScrollLeft ? 'transparent 0, #000 6rem' : '#000 0'}, ${canScrollRight ? '#000 calc(100% - 6rem), transparent 100%' : '#000 100%'})`
			: undefined;
	const maskStyle = maskImage ? { maskImage, WebkitMaskImage: maskImage } : undefined;

	return (
		<div className='relative mb-5 overflow-hidden rounded-full border border-ink/15 bg-ink/[0.025] p-1.5'>
			{/* Left fade + arrow */}
			{canScrollLeft && (
				<div
					className='pointer-events-none absolute inset-y-0 left-0 z-20 flex w-12 items-center justify-start'
				>
					<button
						type='button'
						onClick={() => scroll('left')}
						className='pointer-events-auto ml-1 flex h-8 w-8 items-center justify-center rounded-full bg-white/90 text-ink-faint shadow-sm hover:text-ink'
						aria-label='Scroll tabs left'
					>
						<ChevronLeft className='h-4 w-4' />
					</button>
				</div>
			)}

			{/* Scrollable tabs */}
			<div ref={scrollRef} className='scrollbar-hide overflow-x-auto' style={maskStyle}>
				<div className='flex min-w-max flex-nowrap items-center justify-start gap-1 lg:min-w-0'>
					{tabs.map((tab, idx) => {
						const LucideIcon = tab.icon;
						return (
							<button
								key={tab.label}
								type='button'
								onClick={() => onTabChange(idx)}
								className='relative inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-full px-4 py-2.5 font-sans text-sm transition-colors lg:flex-1'
							>
								{activeTab === idx && (
									<motion.div
										layoutId={indicatorLayoutId}
										className='absolute inset-0 rounded-full bg-white shadow-sm ring-1 ring-inset ring-ink/15'
										transition={{ type: 'spring', bounce: 0.2, duration: 0.4 }}
									/>
								)}
								<span className={`relative z-10 flex items-center gap-2 ${activeTab === idx ? 'font-medium text-ink' : 'text-ink-faint hover:text-ink'}`}>
									{tab.iconSrc ? (
										<img src={tab.iconSrc} alt='' aria-hidden='true' className='h-4 w-4 object-contain' />
									) : LucideIcon ? (
										<LucideIcon className='h-4 w-4' />
									) : null}
									{tab.label}
								</span>
							</button>
						);
					})}
				</div>
			</div>

			{/* Right fade + arrow */}
			{canScrollRight && (
				<div
					className='pointer-events-none absolute inset-y-0 right-0 z-20 flex w-12 items-center justify-end'
				>
					<button
						type='button'
						onClick={() => scroll('right')}
						className='pointer-events-auto mr-2 flex h-8 w-8 items-center justify-center rounded-full bg-white/90 text-ink-faint shadow-sm hover:text-ink'
						aria-label='Scroll tabs right'
					>
						<ChevronRight className='h-4 w-4' />
					</button>
				</div>
			)}
		</div>
	);
};

// --- Hero ---
interface SupportedAgent {
	src: string;
	name: string;
	href: string;
	wordmark?: boolean;
	comingSoon?: boolean;
}

const agents: SupportedAgent[] = [
	{ src: '/images/agent-logos/pi.svg', name: 'Pi', href: '/docs/agents/pi' },
	{ src: '/images/agent-logos/claude-code.svg', name: 'Claude Code', href: '/docs/agents/claude' },
	{ src: '/images/agent-logos/codex.svg', name: 'Codex', href: '/docs/agents/codex' },
	{ src: '/images/agent-logos/opencode.svg', name: 'OpenCode', href: '/docs/agents/opencode' },
];

// Frameworks agentOS works with. Eve's mark is its wordmark, so its chip
// renders the logo alone; the others use their square mark.
const frameworks: SupportedAgent[] = [
	{ src: '/images/frameworks/eve.svg', name: 'Eve', wordmark: true, href: '/docs/frameworks/vercel-eve' },
	{ src: '/images/frameworks/flue.svg', name: 'Flue', href: '/docs/frameworks/flue' },
	{ src: '/images/frameworks/rivet.svg', name: 'Rivet', href: '/docs/frameworks/rivet' },
];

// Tab metadata for the orchestration code panel, leading with agents
// coordinating other agents. Filters the highlighted-snippet array passed
// from index.astro by key. (The execution section renders recorded agent
// sessions instead; see AgentSessionDemo.)
interface HeroTabMeta {
	key: string;
	icon?: React.ComponentType<{ className?: string }>;
	iconSrc?: string;
	label: string;
	docsHref: string;
	docsLabel: string;
}

const orchestrationTabMeta: HeroTabMeta[] = [
	{ key: 'workflows', icon: Workflow, label: 'Workflows & Graphs', docsHref: '/docs/workflows', docsLabel: 'Workflow docs' },
	{ key: 'multiplayer', icon: Users, label: 'Multiplayer', docsHref: '/docs/multiplayer', docsLabel: 'Multiplayer docs' },
	{ key: 'agent-agent', icon: Layers, label: 'Agent-to-Agent', docsHref: '/docs/agent-to-agent', docsLabel: 'Agent-to-agent docs' },
	{ key: 'cron', icon: CalendarClock, label: 'Loops & Crons', docsHref: '/docs/cron', docsLabel: 'Cron jobs docs' },
	{ key: 'human-in-the-loop', icon: ShieldCheck, label: 'Human-in-the-loop', docsHref: '/docs/approvals', docsLabel: 'Approval docs' },
];

// Joins tab metadata with the highlighted snippets rendered at Astro build
// time, dropping any tab whose snippet is missing.
const joinTabs = (meta: HeroTabMeta[], heroTabs: HeroTabCode[]) =>
	meta.flatMap((tab) => {
		const snippet = heroTabs.find((heroTab) => heroTab.key === tab.key);
		return snippet ? [{ ...tab, ...snippet }] : [];
	});

const HERO_COPY = {
	heading: 'Give agents an operating system as a library.',
	primaryDescription: 'Each agent gets a lightweight OS for execution, filesystems, orchestration, and apps.\nRuns in your existing backend – no sandboxes, VMs, or SaaS.',
};

const Hero = () => {
	const [autoPlayAgent, setAutoPlayAgent] = useState<{ src: string; name: string } | null>(null);

	// Highlight stats — best-case "up to" figures, sourced from bench.ts.
	// Figures match the benchmark section's default view (the shell/execution
	// workload, p50, AWS ARM): the chart a chip links to must show the same
	// number the chip claims.
	const heroStats = [
		{ value: `${Math.round(benchColdStart[0].sandbox / benchColdStart[0].agentOS)}×`, label: 'faster cold starts', sub: `p50 · vs. ${SANDBOX_COLDSTART_PROVIDER}`, href: '#bench-cold-start' },
		{ value: `${benchWorkloads.shell.memory.multiplier.split('x')[0]}×`, label: 'less memory', sub: 'vs. 1 GiB sandbox minimum', href: '#bench-memory' },
		{ value: `${benchWorkloads.shell.cost.find((tier) => tier.label === 'AWS ARM')?.ratio ?? Math.min(...benchWorkloads.shell.cost.map((tier) => tier.ratio))}×`, label: 'cheaper to run', sub: `self-hosted vs. ${SANDBOX_COST_PROVIDER}`, href: '#bench-cost' },
	];

	// Auto-cycle through agents starting 2.5s before stroke animation ends
	useEffect(() => {
		const logoAnimationDuration = 800; // Start cycling 2.5s before the 3.3s animation ends
		const agentDisplayDuration = 400; // Time to show each agent

		const startAutoPlay = setTimeout(() => {
			let currentIndex = 0;

			const cycleAgents = () => {
				if (currentIndex < agents.length) {
					setAutoPlayAgent(agents[currentIndex]);
					currentIndex++;
					setTimeout(cycleAgents, agentDisplayDuration);
				} else {
					// End on OS (null)
					setAutoPlayAgent(null);
				}
			};

			cycleAgents();
		}, logoAnimationDuration);

		return () => clearTimeout(startAutoPlay);
	}, []);

	return (
		// Keep the hero height tied to the viewport, as on the main landing page,
		// while preserving enough room below the foundation tiles.
		<section className='relative flex min-h-[92svh] flex-col justify-center bg-paper px-6 pt-44 pb-28 md:pt-52 md:pb-32'>
			<div className='mx-auto flex w-full max-w-5xl flex-col items-center text-center'>
				{/* Centered single-column hero */}
				<div className='flex w-full flex-col items-center text-center'>
					{/* Title */}
					<motion.div
						id='hero-logo'
						initial={{ opacity: 0, y: 20 }}
						animate={{ opacity: 1, y: 0 }}
						transition={{ duration: 0.5, delay: 0.05 }}
						className='mb-7 flex'
						// Counter-zoom back to effective 100%: the logo's stroke-draw mask
						// (userSpaceOnUse) renders unreliably under an ancestor CSS zoom in
						// some browsers, and this is the page's only masked SVG animation.
						style={{ zoom: 1 / PAGE_ZOOM }}
					>
						<AnimatedAgentOSLogo className='h-11 w-auto md:h-12' displayedAgent={autoPlayAgent} />
					</motion.div>

					{/* Headline */}
					<motion.h1
						initial={{ opacity: 0, y: 20 }}
						animate={{ opacity: 1, y: 0 }}
						transition={{ duration: 0.5, delay: 0.1 }}
						className='mb-4 max-w-5xl text-balance text-4xl font-medium leading-[1.06] tracking-[-0.02em] text-ink md:text-5xl'
					>
						{HERO_COPY.heading.split('\n').map((line) => (
							<span key={line} className='block'>
								{line}
							</span>
						))}
					</motion.h1>

					{/* Description */}
					<motion.p
						initial={{ opacity: 0, y: 20 }}
						animate={{ opacity: 1, y: 0 }}
						transition={{ duration: 0.5, delay: 0.13 }}
						className='mb-7 max-w-3xl whitespace-pre-line text-base leading-relaxed text-ink-soft md:text-lg'
					>
						{HERO_COPY.primaryDescription}
					</motion.p>

					{/* Benchmark highlights — proof for "faster, lighter, cheaper", linked to the benchmarks below */}
					<motion.div
						initial={{ opacity: 0, y: 10 }}
						animate={{ opacity: 1, y: 0 }}
						transition={{ duration: 0.5, delay: 0.16 }}
						className='mb-8 flex flex-wrap items-start justify-center gap-x-8 gap-y-3'
					>
						{heroStats.map((stat) => (
							<a
								key={stat.label}
								href={stat.href}
								aria-label={`${stat.value} ${stat.label} (${stat.sub}) — jump to the benchmark`}
								className='group inline-flex flex-col items-center gap-0.5'
							>
								<span className='inline-flex items-baseline gap-1.5'>
									<span className='text-xl font-medium text-pine md:text-2xl'>{stat.value}</span>
									<span className='text-sm text-ink-soft transition-colors group-hover:text-ink md:text-base'>{stat.label}</span>
								</span>
							</a>
						))}
					</motion.div>

					{/* Buttons */}
					<motion.div
						initial={{ opacity: 0, y: 20 }}
						animate={{ opacity: 1, y: 0 }}
						transition={{ duration: 0.5, delay: 0.19 }}
						className='flex w-full flex-col flex-wrap items-center gap-x-4 gap-y-3 sm:flex-row sm:justify-center'
					>
						<CopyInstallCommand />
						<SetupWithAgent />
					</motion.div>

				</div>
			</div>
			<div className='mx-auto mt-12 w-full max-w-7xl md:mt-16'>
				<FloatingFoundation />
			</div>
		</section>
	);
};


const DiagramNode = ({ children, className = '' }: { children: React.ReactNode; className?: string }) => (
	<div className={`z-10 flex items-center justify-center rounded-xl border border-ink/10 bg-white text-center shadow-[0_8px_24px_-20px_rgba(20,20,22,0.45)] ${className}`}>
		{children}
	</div>
);

const OrchestrationVisualization = ({ pattern }: { pattern: string }) => {
	if (pattern === 'workflows') {
		return (
			<div className='flex h-full flex-col items-center justify-center p-5 sm:p-8'>
				<div className='relative h-64 w-full max-w-2xl'>
					<svg aria-hidden='true' viewBox='0 0 640 280' preserveAspectRatio='none' className='absolute inset-0 h-full w-full overflow-visible'>
						<defs>
							<marker id='workflow-arrow' viewBox='0 0 10 10' refX='8' refY='5' markerWidth='6' markerHeight='6' orient='auto'>
								<path d='M 0 0 L 10 5 L 0 10 z' fill='rgba(85,83,78,0.55)' />
							</marker>
						</defs>
						<path d='M 134 130 L 166 130' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#workflow-arrow)' />
						<path d='M 300 130 L 333 130' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#workflow-arrow)' />
						<path d='M 467 130 C 494 130 488 55 512 55' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#workflow-arrow)' />
						<path d='M 467 130 C 494 130 488 225 512 225' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#workflow-arrow)' />
						<path d='M 512 245 C 410 278 70 278 70 174' fill='none' stroke='rgba(85,83,78,0.3)' strokeWidth='1.5' strokeDasharray='5 5' vectorEffect='non-scaling-stroke' markerEnd='url(#workflow-arrow)' />
					</svg>

					<DiagramNode className='absolute top-[90px] left-0 h-20 w-[21%] flex-col px-2'>
						<Code2 className='h-4 w-4 text-pine' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Write code</span>
					</DiagramNode>
					<DiagramNode className='absolute top-[90px] left-[26%] h-20 w-[21%] flex-col px-2'>
						<ShieldCheck className='h-4 w-4 text-pine' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Review</span>
					</DiagramNode>
					<DiagramNode className='absolute top-[90px] left-[52%] h-20 w-[21%] flex-col px-2'>
						<GitFork className='h-4 w-4 text-pine' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Decision</span>
					</DiagramNode>
					<DiagramNode className='absolute top-[15px] right-0 h-20 w-[20%] flex-col px-2'>
						<Check className='h-4 w-4 text-pine' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Ship</span>
						<span className='mt-0.5 text-[9px] text-ink-faint sm:text-[10px]'>Approved</span>
					</DiagramNode>
					<DiagramNode className='absolute right-0 bottom-[15px] h-20 w-[20%] flex-col px-2'>
						<RefreshCw className='h-4 w-4 text-olive' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Retry</span>
						<span className='mt-0.5 text-[9px] text-ink-faint sm:text-[10px]'>Rejected</span>
					</DiagramNode>
				</div>
				<div className='mt-4 flex items-center gap-2 rounded-full border border-ink/10 bg-white/70 px-3 py-1.5 text-xs text-ink-soft'>
					<RefreshCw className='h-3.5 w-3.5 text-pine' />
					RivetKit checkpoints each step, decision, and retry.
				</div>
			</div>
		);
	}

	if (pattern === 'human-in-the-loop') {
		return (
			<div className='flex h-full flex-col items-center justify-center p-5 sm:p-8'>
				<div className='relative h-64 w-full max-w-2xl'>
					<svg aria-hidden='true' viewBox='0 0 640 280' preserveAspectRatio='none' className='absolute inset-0 h-full w-full overflow-visible'>
						<defs>
							<marker id='approval-arrow' viewBox='0 0 10 10' refX='8' refY='5' markerWidth='6' markerHeight='6' orient='auto'>
								<path d='M 0 0 L 10 5 L 0 10 z' fill='rgba(85,83,78,0.55)' />
							</marker>
						</defs>
						<path d='M 150 140 L 236 140' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#approval-arrow)' />
						<path d='M 404 140 C 446 140 441 69 482 69' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#approval-arrow)' />
						<path d='M 404 140 C 446 140 441 211 482 211' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#approval-arrow)' />
					</svg>

					<DiagramNode className='absolute top-[100px] left-0 h-20 w-[23%] flex-col px-2'>
						<Activity className='h-4 w-4 text-pine' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Agent requests</span>
						<span className='mt-0.5 text-[9px] text-ink-faint sm:text-[10px]'>tool call</span>
					</DiagramNode>
					<DiagramNode className='absolute top-[90px] left-[37%] h-24 w-[26%] flex-col px-2'>
						<ShieldCheck className='h-5 w-5 text-pine' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Human reviews</span>
						<span className='mt-1 rounded-full bg-olive/10 px-2 py-0.5 text-[9px] font-medium text-olive sm:text-[10px]'>Agent paused</span>
					</DiagramNode>
					<DiagramNode className='absolute top-[29px] right-0 h-20 w-[22%] flex-col px-2'>
						<Check className='h-4 w-4 text-pine' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Approve</span>
						<span className='mt-0.5 text-[9px] text-ink-faint sm:text-[10px]'>Resume agent</span>
					</DiagramNode>
					<DiagramNode className='absolute right-0 bottom-[29px] h-20 w-[22%] flex-col px-2'>
						<RefreshCw className='h-4 w-4 text-olive' />
						<span className='mt-2 text-[11px] font-medium text-ink sm:text-sm'>Reject</span>
						<span className='mt-0.5 text-[9px] text-ink-faint sm:text-[10px]'>Return feedback</span>
					</DiagramNode>
				</div>
				<div className='mt-4 flex items-center gap-2 rounded-full border border-ink/10 bg-white/70 px-3 py-1.5 text-xs text-ink-soft'>
					<Moon className='h-3.5 w-3.5 text-olive' />
					The session waits durably for a human decision.
				</div>
			</div>
		);
	}

	if (pattern === 'multiplayer') {
		return (
			<div className='grid h-full grid-cols-[0.9fr_4rem_1.1fr] items-center gap-2 p-6 sm:grid-cols-[0.8fr_7rem_1.2fr] sm:p-10'>
				<div className='space-y-4'>
					{['You', 'Teammate', 'App'].map((client) => (
						<DiagramNode key={client} className='h-12 px-3 text-xs font-medium text-ink sm:h-14 sm:text-sm'>{client}</DiagramNode>
					))}
				</div>
				<svg aria-hidden='true' viewBox='0 0 100 220' preserveAspectRatio='none' className='h-56 w-full overflow-visible'>
					<defs>
						<marker id='multiplayer-arrow' viewBox='0 0 10 10' refX='8' refY='5' markerWidth='6' markerHeight='6' orient='auto'>
							<path d='M 0 0 L 10 5 L 0 10 z' fill='rgba(85,83,78,0.55)' />
						</marker>
					</defs>
					<path d='M 0 35 C 42 35 48 110 96 110' fill='none' stroke='rgba(85,83,78,0.35)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#multiplayer-arrow)' />
					<path d='M 0 110 L 96 110' fill='none' stroke='rgba(85,83,78,0.35)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#multiplayer-arrow)' />
					<path d='M 0 185 C 42 185 48 110 96 110' fill='none' stroke='rgba(85,83,78,0.35)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#multiplayer-arrow)' />
				</svg>
				<DiagramNode className='min-h-40 flex-col px-4 py-6 sm:min-h-48 sm:px-8'>
					<Users className='h-6 w-6 text-pine' />
					<span className='mt-4 text-sm font-medium text-ink sm:text-base'>Shared agent session</span>
					<span className='mt-2 text-xs leading-relaxed text-ink-faint'>One event stream.<br />Many connected clients.</span>
				</DiagramNode>
			</div>
		);
	}

	if (pattern === 'agent-agent') {
		return (
			<div className='flex h-full items-center justify-center p-5 sm:p-8'>
				<div className='relative h-64 w-full max-w-xl'>
					<svg aria-hidden='true' viewBox='0 0 560 240' preserveAspectRatio='none' className='absolute inset-0 h-full w-full'>
						<defs>
							<marker id='agent-arrow' viewBox='0 0 10 10' refX='8' refY='5' markerWidth='6' markerHeight='6' orient='auto'>
								<path d='M 0 0 L 10 5 L 0 10 z' fill='rgba(85,83,78,0.55)' />
							</marker>
						</defs>
						<path d='M 138 82 C 230 32 330 32 422 82' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#agent-arrow)' />
						<path d='M 422 158 C 330 208 230 208 138 158' fill='none' stroke='rgba(85,83,78,0.38)' strokeWidth='1.5' vectorEffect='non-scaling-stroke' markerEnd='url(#agent-arrow)' />
					</svg>
					<DiagramNode className='absolute top-1/2 left-0 h-28 w-24 -translate-y-1/2 flex-col px-3 sm:w-32'>
						<Activity className='h-5 w-5 text-pine' />
						<span className='mt-3 text-xs font-medium text-ink sm:text-sm'>Writer agent</span>
					</DiagramNode>
					<div className='absolute top-1/2 left-1/2 z-20 -translate-x-1/2 -translate-y-1/2 rounded-full border border-ink/10 bg-paper px-3 py-1.5 font-mono text-[10px] text-ink-soft'>binding</div>
					<DiagramNode className='absolute top-1/2 right-0 h-28 w-24 -translate-y-1/2 flex-col px-3 sm:w-32'>
						<ShieldCheck className='h-5 w-5 text-pine' />
						<span className='mt-3 text-xs font-medium text-ink sm:text-sm'>Reviewer agent</span>
					</DiagramNode>
					<span className='absolute top-7 left-1/2 -translate-x-1/2 text-[10px] text-ink-faint sm:text-xs'>file + request</span>
					<span className='absolute bottom-7 left-1/2 -translate-x-1/2 text-[10px] text-ink-faint sm:text-xs'>review result</span>
				</div>
			</div>
		);
	}

	return (
		<div className='flex h-full flex-col items-center justify-center p-5 sm:p-8'>
			<div className='inline-flex items-center gap-2 rounded-full border border-pine/20 bg-pine/[0.05] px-4 py-2 font-mono text-xs font-medium text-pine'>
				<CalendarClock className='h-4 w-4' />
				*/5 * * * * <span className='font-sans font-normal text-ink-soft'>Every five minutes</span>
			</div>

			<div className='relative mt-9 w-full max-w-xl'>
				<div aria-hidden='true' className='absolute top-3 right-[10%] left-[10%] h-px bg-ink/15' />
				<div className='relative flex justify-between'>
					{['09:00', '09:05', '09:10'].map((time) => (
						<div key={time} className='flex w-[30%] max-w-32 flex-col items-center'>
							<span className='relative z-10 flex h-6 w-6 items-center justify-center rounded-full border border-pine/25 bg-white shadow-sm'>
								<span className='h-2 w-2 rounded-full bg-pine' />
							</span>
							<DiagramNode className='mt-4 h-24 w-full flex-col px-2'>
								<span className='font-mono text-[10px] text-ink-faint sm:text-xs'>{time}</span>
								<Activity className='mt-2 h-4 w-4 text-pine' />
								<span className='mt-1.5 text-[11px] font-medium text-ink sm:text-sm'>Run agent</span>
							</DiagramNode>
						</div>
					))}
			</div>
			</div>

			<div className='mt-7 flex items-center gap-2 rounded-full border border-ink/10 bg-white/70 px-3 py-1.5 text-xs text-ink-soft'>
				<Moon className='h-3.5 w-3.5 text-olive' />
				The agent sleeps between scheduled runs.
		</div>
		</div>
	);
};

// Each orchestration pattern opens as a visual explanation. Code remains one
// click away and is generated at build time from the checked examples.
const CodePanel = ({ tabs }: { tabs: HeroTabEntry[] }) => {
	const [activeTab, setActiveTab] = useState(0);
	const [showCode, setShowCode] = useState(false);
	const active = tabs[activeTab];

	const selectTab = (index: number) => {
		setActiveTab(index);
		setShowCode(false);
	};

	return (
		<div>
			<HeroTabs tabs={tabs} activeTab={activeTab} onTabChange={selectTab} />

			<div className='overflow-hidden rounded-xl border border-zinc-200 bg-zinc-50'>
				<div className='flex items-center gap-2 border-b border-zinc-200 px-4 py-3'>
					<div className='h-3 w-3 rounded-full bg-zinc-200' />
					<div className='h-3 w-3 rounded-full bg-zinc-200' />
					<div className='h-3 w-3 rounded-full bg-zinc-200' />
					<span className={`ml-2 hidden text-xs text-zinc-700 sm:inline ${showCode ? 'font-code' : 'font-medium'}`}>{showCode ? active?.fileName ?? 'index.ts' : active?.label ?? 'Loops & Crons'}</span>
					<button
						type='button'
						onClick={() => setShowCode((visible) => !visible)}
						aria-pressed={showCode}
						className='ml-auto inline-flex h-7 items-center gap-1.5 rounded-md border border-ink/20 bg-ink/[0.06] px-2.5 text-[11px] font-medium text-ink transition-colors hover:border-ink/30 hover:bg-ink/[0.1]'
					>
						<Code2 className='h-3.5 w-3.5' />
						{showCode ? 'Show diagram' : 'Show me the code'}
					</button>
				</div>
				<div className='relative h-[420px] overflow-hidden'>
					<AnimatePresence mode='wait' initial={false}>
						{showCode ? (
						<motion.div
							key={`code-${activeTab}`}
							initial={{ opacity: 0 }}
							animate={{ opacity: 1 }}
							exit={{ opacity: 0 }}
							transition={{ duration: 0.2 }}
							className='absolute inset-0 overflow-auto p-6 font-code text-sm leading-relaxed text-zinc-600 [&_.line]:break-all [&_.shiki]:!m-0 [&_.shiki]:!bg-transparent [&_.shiki]:!p-0 [&_.shiki]:font-code [&_.shiki]:text-sm [&_.shiki]:leading-relaxed [&_pre]:whitespace-pre-wrap'
						>
							<span
								className='not-prose code'
								// biome-ignore lint/security/noDangerouslySetInnerHtml: generated at Astro render time
								dangerouslySetInnerHTML={{ __html: active?.highlightedCode ?? '' }}
							/>
						</motion.div>
						) : (
							<motion.div key={`visual-${activeTab}`} initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={{ duration: 0.2 }} className='absolute inset-0'>
								<OrchestrationVisualization pattern={active?.key ?? 'cron'} />
							</motion.div>
						)}
					</AnimatePresence>
				</div>
			</div>
			{active && (
				<div className='mt-4 flex justify-end'>
					<a href={active.docsHref} className='whitespace-nowrap text-sm text-accent-deep underline underline-offset-2 transition-colors hover:text-accent'>
						{active.docsLabel ?? `${active.label} docs`} <span aria-hidden='true'>→</span>
					</a>
				</div>
			)}
		</div>
	);
};

const OrchestrationSection = ({ heroTabs }: { heroTabs: HeroTabCode[] }) => (
	<section id='orchestration' className='scroll-mt-24 border-t border-ink/10 bg-paper-mid px-6 py-16 md:py-32'>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<div className='grid gap-10 lg:grid-cols-[0.65fr_1.35fr] lg:items-center lg:gap-16'>
					<div>
						<h2 className='text-balance text-4xl font-medium leading-[1.04] tracking-[-0.035em] text-ink md:text-6xl'>Orchestration</h2>
						<p className='mt-5 max-w-xl text-base leading-relaxed text-ink-soft md:text-lg'>Schedule recurring agent jobs, build durable RivetKit workflows, connect agents, and share live sessions with ordinary application code.</p>
						<ActorsAttribution />
					</div>
					<div className='min-w-0'>
						<CodePanel tabs={joinTabs(orchestrationTabMeta, heroTabs)} />
					</div>
				</div>
			</Reveal>
		</div>
	</section>
);

export const APPS_CODE = `import { deployApp } from "@rivet-dev/agentos-apps";

// Deploy the files generated by your agent as an application.
await deployApp({
  appId: "team-board",
  source: new URL("./generated-app/", import.meta.url),
});`;

const AppsSourceStack = () => (
	<div className='flex flex-col items-center justify-center'>
		<p className='mb-2 text-[10px] font-medium text-ink'>Generated files</p>
		<div className='relative h-36 w-full max-w-[9.5rem]'>
			<div className='absolute top-1 right-6 left-0 z-10 h-24 -rotate-6 rounded-lg border border-ink/10 bg-white p-2 shadow-[0_8px_20px_-14px_rgba(20,20,22,0.4)]'>
				<div className='flex items-center gap-1.5'>
					<span className='flex h-5 w-5 items-end justify-end rounded bg-[#f7df1e] p-0.5 text-[7px] font-bold leading-none text-black'>JS</span>
					<span className='font-mono text-[8px] text-ink-soft'>server.js</span>
				</div>
				<div className='mt-3 space-y-1.5'>
					<div className='h-1 w-4/5 rounded-full bg-ink/10' />
					<div className='h-1 w-3/5 rounded-full bg-ink/[0.07]' />
				</div>
			</div>

			<div className='absolute top-4 right-0 left-6 z-20 h-24 rotate-6 rounded-lg border border-ink/10 bg-white p-2 shadow-[0_8px_20px_-14px_rgba(20,20,22,0.4)]'>
				<div className='flex items-center gap-1.5'>
					<span className='flex h-5 w-5 items-center justify-center rounded bg-[#eef7ed]'>
						<img src='/images/registry/nodejs.svg' alt='' aria-hidden='true' className='h-3.5 w-3.5 object-contain' />
					</span>
					<span className='font-mono text-[8px] text-ink-soft'>api.ts</span>
				</div>
				<div className='mt-3 space-y-1.5'>
					<div className='h-1 w-3/4 rounded-full bg-ink/10' />
					<div className='h-1 w-1/2 rounded-full bg-ink/[0.07]' />
				</div>
			</div>

			<div className='absolute top-7 right-3 left-3 z-30 h-24 rounded-lg border border-ink/10 bg-white p-2 shadow-[0_10px_24px_-15px_rgba(20,20,22,0.48)]'>
				<div className='flex items-center gap-1.5'>
					<span className='flex h-5 w-5 items-center justify-center rounded bg-[#eaf8fc] text-sm leading-none text-[#149eca]'>⚛</span>
					<span className='font-mono text-[8px] text-ink-soft'>App.tsx</span>
				</div>
				<div className='mt-3 space-y-1.5'>
					<div className='h-1 w-5/6 rounded-full bg-ink/10' />
					<div className='h-1 w-2/3 rounded-full bg-ink/[0.07]' />
					<div className='h-1 w-3/4 rounded-full bg-ink/[0.07]' />
				</div>
			</div>
		</div>
	</div>
);

const AppsVisualization = ({ highlightedCode }: { highlightedCode: string }) => {
	const [showCode, setShowCode] = useState(false);

	return (
		<div className='overflow-hidden rounded-xl border border-zinc-200 bg-zinc-50'>
			<div className='flex items-center gap-2 border-b border-zinc-200 px-4 py-3'>
				<div className='h-3 w-3 rounded-full bg-zinc-200' />
				<div className='h-3 w-3 rounded-full bg-zinc-200' />
				<div className='h-3 w-3 rounded-full bg-zinc-200' />
				<span className={`ml-2 hidden text-xs text-zinc-700 sm:inline ${showCode ? 'font-code' : 'font-medium'}`}>{showCode ? 'deploy.ts' : 'Deploy an AI-generated app'}</span>
				<button
					type='button'
					onClick={() => setShowCode((visible) => !visible)}
					aria-pressed={showCode}
					className='ml-auto inline-flex h-7 items-center gap-1.5 rounded-md border border-ink/20 bg-ink/[0.06] px-2.5 text-[11px] font-medium text-ink transition-colors hover:border-ink/30 hover:bg-ink/[0.1]'
				>
					<Code2 className='h-3.5 w-3.5' />
					{showCode ? 'Show diagram' : 'Show me the code'}
				</button>
			</div>

			<AnimatePresence mode='wait' initial={false}>
				{showCode ? (
					<motion.div key='apps-code' initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={{ duration: 0.2 }} className='h-[302px] overflow-auto p-6 font-code text-sm leading-relaxed text-zinc-600 sm:h-[318px] [&_.line]:break-all [&_.shiki]:!m-0 [&_.shiki]:!bg-transparent [&_.shiki]:!p-0 [&_.shiki]:font-code [&_.shiki]:text-sm [&_.shiki]:leading-relaxed [&_pre]:whitespace-pre-wrap'>
						<span
							className='not-prose code'
							// biome-ignore lint/security/noDangerouslySetInnerHtml: generated at Astro render time
							dangerouslySetInnerHTML={{ __html: highlightedCode }}
						/>
					</motion.div>
				) : (
					<motion.div key='apps-visual' initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={{ duration: 0.2 }} className='p-4 sm:p-6'>
			<div className='grid min-h-[270px] gap-3 md:grid-cols-[9.5rem_2rem_7rem_2rem_12rem] md:items-center md:justify-center'>
				<AppsSourceStack />

				<div className='flex items-center justify-center text-pine/55'>
					<ArrowRight className='h-6 w-8 rotate-90 md:rotate-0' />
				</div>

				<div className='min-w-28 rounded-xl border border-pine/20 bg-white px-4 py-4 text-center ring-1 ring-pine/[0.04] shadow-[0_12px_28px_-18px_rgba(20,20,22,0.5)]'>
					<Rocket className='mx-auto h-5 w-5 text-pine' />
					<p className='mt-2 text-[9px] font-medium uppercase tracking-[0.1em] text-ink-soft'>Deploy app</p>
					<p className='mt-1 font-mono text-[11px] font-medium text-ink'>deployApp()</p>
				</div>

				<div className='flex items-center justify-center text-pine/55'>
					<ArrowRight className='h-6 w-8 rotate-90 md:rotate-0' />
				</div>

				<div className='mx-auto w-full max-w-[12rem] overflow-hidden rounded-lg border border-ink/10 bg-white shadow-[0_10px_26px_-18px_rgba(20,20,22,0.45)]'>
					<div className='flex items-center gap-1.5 border-b border-ink/10 bg-ink/[0.025] px-2 py-1.5'>
						<div className='flex shrink-0 items-center gap-1' aria-hidden='true'>
							<span className='h-1.5 w-1.5 rounded-full bg-ink/15' />
							<span className='h-1.5 w-1.5 rounded-full bg-ink/15' />
							<span className='h-1.5 w-1.5 rounded-full bg-ink/15' />
						</div>
						<div className='min-w-0 flex-1 truncate rounded border border-ink/10 bg-white px-1.5 py-0.5 font-mono text-[7px] text-ink-faint'>apps.acme.dev/board</div>
					</div>
					<div className='h-36 p-2.5'>
						<div className='flex items-center justify-between'>
							<span className='text-[9px] font-semibold tracking-[-0.01em] text-ink'>Kanban</span>
							<span className='rounded-full bg-pine/[0.08] px-1.5 py-0.5 text-[6px] font-medium text-pine'>3 tasks</span>
						</div>
						<div className='mt-2.5 grid grid-cols-3 gap-1.5'>
							{['To do', 'Doing', 'Done'].map((label, columnIndex) => (
								<div key={label} className='rounded border border-ink/[0.07] bg-paper p-1.5'>
									<p className='text-[6px] font-medium text-ink-faint'>{label}</p>
									<div className='mt-1.5 space-y-1'>
										{Array.from({ length: columnIndex === 0 ? 2 : 1 }).map((_, taskIndex) => (
											<div key={taskIndex} className='rounded border border-ink/[0.07] bg-white p-1'>
												<div className={`h-1 rounded-full ${columnIndex === 1 ? 'w-4/5 bg-pine/20' : 'w-full bg-ink/10'}`} />
												<div className='mt-1 h-1 w-3/5 rounded-full bg-ink/[0.06]' />
														</div>
													))}
												</div>
											</div>
										))}
									</div>
									</div>
									</div>
						</div>
					</motion.div>
				)}
			</AnimatePresence>
		</div>
	);
};

const appsFeatures = [
	{ icon: AppWindow, title: 'REST APIs & frontends', description: 'Serve APIs and generated interfaces from the same application.' },
	{ icon: Database, title: 'SQLite', description: 'Store durable application data in an actor-owned SQLite database.' },
	{ icon: Workflow, title: 'Workflows & queues', description: 'Run durable jobs that can sleep, retry, resume, and process queued work.' },
	{ icon: CalendarClock, title: 'Crons & schedules', description: 'Schedule recurring application work while idle instances sleep.' },
	{ icon: Users, title: 'Multiplayer', description: 'Share realtime state between every user connected to an application.' },
];

const AppsSection = ({ highlightedCode }: { highlightedCode: string }) => (
	<section id='apps' className='scroll-mt-24 border-t border-ink/10 bg-paper px-6 py-16 text-ink md:py-32'>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<div className='grid gap-10 lg:grid-cols-[1.35fr_0.65fr] lg:items-center lg:gap-16'>
					<div className='lg:order-2'>
						<div className='mb-4'>
							<span className='font-mono text-[10px] font-medium uppercase tracking-[0.18em] text-pine'>Preview</span>
						</div>
						<h2 className='text-balance text-4xl font-medium leading-[1.04] tracking-[-0.035em] text-ink md:text-6xl'>Apps</h2>
						<p className='mt-5 max-w-xl text-base leading-relaxed text-ink-soft md:text-lg'>Deploy AI-generated applications for each of your users. Every server runs in agentOS, and the entire deployment stack is a library in your existing backend—not extra infrastructure.</p>
						<div className='mt-7 max-w-md border-t border-ink/10 pt-6'>
							<ul className='space-y-4'>
								{appsFeatures.map((feature) => {
									const Icon = feature.icon;
									return (
										<li key={feature.title} className='flex items-start gap-3'>
											<span className='flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-ink/10 bg-white/55 text-pine'>
												<Icon className='h-4 w-4' />
											</span>
											<div>
												<h3 className='text-sm font-medium text-ink'>{feature.title}</h3>
												<p className='mt-0.5 text-xs leading-relaxed text-ink-soft'>{feature.description}</p>
											</div>
										</li>
									);
								})}
							</ul>
							<a href='/docs/apps' className='mt-6 inline-flex items-center gap-2 text-sm font-medium text-accent-deep underline underline-offset-4 transition-colors hover:text-accent'>
								Explore agentOS Apps
								<ArrowRight className='h-3.5 w-3.5' />
							</a>
						</div>
					</div>
					<div className='min-w-0 lg:order-1'>
						<AppsVisualization highlightedCode={highlightedCode} />
					</div>
				</div>
			</Reveal>
		</div>
	</section>
);

const ActorsAttribution = () => (
	<aside className='mt-7 border-t border-ink/10 pt-5 text-ink'>
		<div className='flex items-start gap-3'>
			<img src='/rivet-icon.svg' alt='' aria-hidden='true' className='mt-0.5 h-4 w-4 shrink-0 opacity-70' />
			<div>
				<p className='text-xs font-medium text-ink'>Powered by Rivet Actors</p>
				<p className='mt-1.5 max-w-sm text-xs leading-relaxed text-ink-soft'>Every agent runs as a durable actor with realtime, workflows, and fault tolerance built in.</p>
				<a href='https://rivet.dev/docs/actors/' target='_blank' rel='noopener noreferrer' className='mt-2 inline-flex items-center gap-1 text-xs font-medium text-ink-soft underline decoration-ink/20 underline-offset-4 transition-colors hover:text-ink hover:decoration-ink/50'>
					Learn more
					<ExternalLink className='h-3 w-3' />
				</a>
			</div>
		</div>
	</aside>
);

const filesystemConnections = [
	{ path: '/workspace', label: 'Host directory', icon: <HardDrive aria-hidden='true' className='h-6 w-6 shrink-0 text-olive sm:h-7 sm:w-7' /> },
	{ path: '/data', label: 'S3', icon: <img src='/images/registry/s3.svg' alt='' aria-hidden='true' className='h-6 w-6 shrink-0 object-contain sm:h-7 sm:w-7' /> },
	{ path: '/documents', label: 'Google Drive', icon: <img src='/images/registry/google-drive.svg' alt='' aria-hidden='true' className='h-6 w-6 shrink-0 object-contain sm:h-7 sm:w-7' /> },
];

export const FILESYSTEM_CODE = `import { agentOS, setup } from "@rivet-dev/agentos";

const vm = agentOS({
  mounts: [
    {
      path: "/workspace",
      plugin: {
        id: "host_dir",
        config: { hostPath: process.cwd() },
      },
      readOnly: true,
    },
    {
      path: "/data",
      plugin: {
        id: "chunked_s3",
        config: {
          bucket: process.env.S3_BUCKET!,
          metadataPath: "/var/lib/s3.sqlite",
        },
      },
    },
    {
      path: "/documents",
      plugin: {
        id: "google_drive",
        config: {
          credentials: {
            clientEmail: process.env.GOOGLE_DRIVE_CLIENT_EMAIL!,
            privateKey: process.env.GOOGLE_DRIVE_PRIVATE_KEY!,
          },
          folderId: process.env.GOOGLE_DRIVE_FOLDER_ID!,
        },
      },
    },
  ],
});

export const registry = setup({ use: { vm } });
registry.start();`;

const FilesystemMap = () => (
	<div className='flex h-[360px] items-center justify-center p-5 sm:h-[430px] sm:p-10'>
		<div className='w-full max-w-2xl'>
			<div className='grid grid-cols-[minmax(0,1fr)_2.5rem_minmax(0,1.1fr)] items-end px-1 pb-3 text-[10px] font-medium uppercase tracking-[0.09em] text-ink-faint sm:grid-cols-[minmax(0,1fr)_4.5rem_minmax(0,1.15fr)]'>
				<span>Storage</span>
				<span aria-hidden='true' />
				<span>Filesystem path</span>
			</div>
			<div className='space-y-3'>
				{filesystemConnections.map((connection) => (
					<div key={connection.path} className='grid grid-cols-[minmax(0,1fr)_2.5rem_minmax(0,1.1fr)] items-center sm:grid-cols-[minmax(0,1fr)_4.5rem_minmax(0,1.15fr)]'>
						<div className='flex h-14 min-w-0 items-center gap-2.5 rounded-xl border border-ink/10 bg-white px-3 shadow-[0_5px_18px_-16px_rgba(20,20,22,0.4)] sm:gap-3 sm:px-4'>
							{connection.icon}
							<span className='min-w-0 text-xs font-medium leading-tight text-ink sm:text-sm'>{connection.label}</span>
						</div>
						<div className='flex items-center px-1.5 text-ink-faint sm:px-2.5' aria-hidden='true'>
							<span className='h-px flex-1 bg-ink/15' />
							<ArrowRight className='-ml-px h-3.5 w-3.5 shrink-0' />
						</div>
						<div className='flex h-14 min-w-0 items-center rounded-xl border border-ink/10 bg-white px-3 font-mono text-[11px] text-ink-soft shadow-[0_5px_18px_-16px_rgba(20,20,22,0.4)] sm:px-4 sm:text-sm'>
							{connection.path}
						</div>
					</div>
				))}
			</div>
			<p className='mt-5 text-center text-xs text-ink-faint'>Mounted into one persistent POSIX filesystem.</p>
		</div>
	</div>
);

const FilesystemCodeView = ({ highlightedCode }: { highlightedCode: string }) => (
	<div className='h-[360px] overflow-auto p-6 font-code text-sm leading-relaxed text-zinc-600 sm:h-[430px] [&_.line]:break-all [&_.shiki]:!m-0 [&_.shiki]:!bg-transparent [&_.shiki]:!p-0 [&_.shiki]:font-code [&_.shiki]:text-sm [&_.shiki]:leading-relaxed [&_pre]:whitespace-pre-wrap'>
		<span
			className='not-prose code'
			// biome-ignore lint/security/noDangerouslySetInnerHtml: generated at Astro render time
			dangerouslySetInnerHTML={{ __html: highlightedCode }}
		/>
	</div>
);

const FilesystemSection = ({ highlightedCode }: { highlightedCode: string }) => {
	const [showCode, setShowCode] = useState(false);

	return (
		<section id='filesystem' className='scroll-mt-24 border-t border-ink/10 bg-paper px-6 py-16 text-ink md:py-24'>
			<div className='mx-auto max-w-7xl'>
				<Reveal>
					<div className='grid gap-10 lg:grid-cols-[1.35fr_0.65fr] lg:items-center lg:gap-16'>
						<div className='lg:order-2'>
							<h2 className='text-balance text-4xl font-medium leading-[1.04] tracking-[-0.035em] text-ink md:text-6xl'>Filesystem</h2>
							<p className='mt-5 max-w-xl text-base leading-relaxed text-ink-soft md:text-lg'>Every agent gets its own persistent POSIX filesystem. Mount S3, Google Drive, host directories, or a full sandbox at a normal path, then use familiar files and shell tools everywhere.</p>
							<a href='/docs/filesystem' className='mt-6 inline-flex items-center gap-2 text-sm font-medium text-accent-deep underline underline-offset-4 transition-colors hover:text-accent'>
								Explore the filesystem
								<ArrowRight className='h-3.5 w-3.5' />
							</a>
						</div>

						<div className='min-w-0 overflow-hidden rounded-xl border border-zinc-200 bg-zinc-50 lg:order-1'>
							<div className='flex items-center gap-2 border-b border-zinc-200 px-4 py-3'>
								<div className='h-3 w-3 rounded-full bg-zinc-200' />
								<div className='h-3 w-3 rounded-full bg-zinc-200' />
								<div className='h-3 w-3 rounded-full bg-zinc-200' />
								<span className={`ml-2 hidden text-xs text-zinc-700 sm:inline ${showCode ? 'font-code' : 'font-medium'}`}>{showCode ? 'filesystem.ts' : 'agentOS VM Mounts'}</span>
								<button
									type='button'
									onClick={() => setShowCode((visible) => !visible)}
									aria-pressed={showCode}
									className='ml-auto inline-flex h-7 items-center gap-1.5 rounded-md border border-ink/20 bg-ink/[0.06] px-2.5 text-[11px] font-medium text-ink transition-colors hover:border-ink/30 hover:bg-ink/[0.1]'
								>
									<Code2 className='h-3.5 w-3.5' />
									{showCode ? 'Show diagram' : 'Show me the code'}
								</button>
							</div>
							{showCode ? <FilesystemCodeView highlightedCode={highlightedCode} /> : <FilesystemMap />}
						</div>
					</div>
				</Reveal>
			</div>
		</section>
	);
};

// --- Execution layer ---
const executionFeatures = [
	{
		icon: Gauge,
		title: 'Native JavaScript performance',
		description: 'Run Node.js on native V8 isolates with the full JIT, not JavaScript compiled to WASM.',
	},
	{
		icon: Check,
		title: 'Built-in type checks',
		description: 'Type-check agent-generated code and return diagnostics so the agent can fix errors.',
	},
	{
		icon: Blocks,
		title: 'Bindings',
		description: 'Expose typed backend functions without giving credentials to the VM.',
	},
	{
		icon: GitFork,
		title: 'Subprocesses',
		description: 'Spawn, stream, signal, and manage child processes with Linux-like semantics.',
	},
	{
		icon: Globe,
		title: 'Network requests & servers',
		description: 'Make outbound requests or serve HTTP from inside the VM under explicit permissions.',
	},
];

const ExecutionSection = () => (
	<section id='execution' className='scroll-mt-24 border-t border-ink/10 bg-paper-mid px-6 py-24 md:py-32'>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<div className='grid gap-10 lg:grid-cols-[0.65fr_1.35fr] lg:items-center lg:gap-16'>
					<div>
						<h2 className='text-balance text-4xl font-medium leading-[1.04] tracking-[-0.035em] text-ink md:text-6xl'>Execution</h2>
						<p className='mt-5 max-w-xl text-base leading-relaxed text-ink-soft md:text-lg'>Run Bash, Node.js, Python, and registry software inside the same isolated VM.</p>
						<div className='mt-7 max-w-md border-t border-ink/10 pt-6'>
							<ul className='space-y-4'>
								{executionFeatures.map((feature) => {
									const Icon = feature.icon;
									return (
										<li key={feature.title} className='flex items-start gap-3'>
											<span className='flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-ink/10 bg-white/55 text-pine'>
												<Icon className='h-4 w-4' />
											</span>
											<div>
												<h3 className='text-sm font-medium text-ink'>{feature.title}</h3>
												<p className='mt-0.5 text-xs leading-relaxed text-ink-soft'>{feature.description}</p>
											</div>
										</li>
									);
								})}
							</ul>
							<a href='/docs/processes' className='mt-6 inline-flex items-center gap-2 text-sm font-medium text-accent-deep underline underline-offset-4 transition-colors hover:text-accent'>
								Explore execution docs
								<ArrowRight className='h-3.5 w-3.5' />
							</a>
						</div>
					</div>
					<div className='min-w-0'>
						<AgentSessionDemo />
					</div>
				</div>
			</Reveal>
		</div>
	</section>
);

// --- Registry ecosystem ---
const REGISTRY_TYPE_LABELS: Record<string, string> = {
  agent: 'Agent',
  'file-system': 'File System',
  browser: 'Browser',
  software: 'Software',
};

// The landing-page marquee is an ecosystem showcase, not an inventory of
// Linux utilities. Keep this focused on agents, filesystems, browsers, and
// recognizable software that demonstrates useful agent workloads.
const REGISTRY_ROW_A = ['pi', 'claude-code', 'codex', 'opencode', 's3', 'google-drive', 'host-dir', 'memory'];
const REGISTRY_ROW_B = ['browserbase', 'git', 'vim', 'duckdb', 'sqlite3', 'ripgrep', 'jq', 'codex-cli'];
const pickRegistry = (slugs: string[]) =>
  slugs
    .map((slug) => registry.find((entry) => entry.slug === slug))
    .filter((entry): entry is (typeof registry)[number] => entry !== undefined);
const registryRowA = pickRegistry(REGISTRY_ROW_A);
const registryRowB = pickRegistry(REGISTRY_ROW_B);

const RegistryAppTile = ({ entry, hidden }: { entry: (typeof registry)[number]; hidden?: boolean }) => {
  const available = entry.status !== 'coming-soon';
  const external = entry.status === 'external';
  const category = REGISTRY_TYPE_LABELS[entry.types[0]] ?? 'Integration';
  const IconComponent = entry.icon ? REGISTRY_ICONS[entry.icon] : undefined;
  const action = entry.status === 'external' ? 'Deploy' : entry.status === 'docs' ? 'Docs' : entry.status === 'config' ? 'Use' : entry.status === 'available' ? 'Get' : 'Soon';
  return (
    <a
      href={external ? entry.href : `/registry/${entry.slug}`}
      target={external ? '_blank' : undefined}
      rel={external ? 'noopener noreferrer' : undefined}
      aria-hidden={hidden}
      tabIndex={hidden ? -1 : undefined}
      className='group/tile flex w-64 shrink-0 items-center gap-3.5 rounded-xl border border-ink/10 bg-white/55 p-3 transition-colors hover:border-ink/25 hover:bg-white/80'
    >
      <div className='flex h-12 w-12 shrink-0 items-center justify-center rounded-xl border border-ink/10 bg-ink/5'>
        {entry.image ? (
          <img src={entry.image} alt={entry.title} width={26} height={26} className='object-contain' />
        ) : IconComponent ? (
          <IconComponent style={{ width: 24, height: 24 }} className='text-ink' />
        ) : (
          <span className='font-mono text-base font-medium text-ink-soft'>{entry.title.charAt(0)}</span>
        )}
      </div>
      <div className='min-w-0 flex-1'>
        <h4 className='truncate text-sm font-medium text-ink'>{entry.title}</h4>
        <p className='truncate text-xs text-ink-faint'>{category}</p>
      </div>
      <span
        className={`shrink-0 rounded-full border px-3 py-1 text-[11px] font-semibold uppercase tracking-wide transition-colors ${
          available
            ? 'border-ink/15 text-ink-soft group-hover/tile:border-ink group-hover/tile:bg-ink group-hover/tile:text-cream'
            : 'border-ink/10 text-ink-faint'
        }`}
      >
        {action}
      </span>
    </a>
  );
};

const RegistryMarqueeRow = ({
  apps,
  direction,
}: {
  apps: (typeof registry)[number][];
  direction: 'left' | 'right';
}) => (
  <div className='group relative overflow-hidden [-webkit-mask-image:linear-gradient(to_right,transparent,#000_6%,#000_94%,transparent)] [mask-image:linear-gradient(to_right,transparent,#000_6%,#000_94%,transparent)]'>
    <div
      className={`flex w-max gap-3 ${
        direction === 'left'
          ? 'animate-[registry-marquee-left_46s_linear_infinite]'
          : 'animate-[registry-marquee-right_46s_linear_infinite]'
      } group-hover:[animation-play-state:paused] motion-reduce:animate-none`}
    >
      {[...apps, ...apps].map((entry, i) => (
        <RegistryAppTile key={`${entry.slug}-${i}`} entry={entry} hidden={i >= apps.length} />
      ))}
    </div>
  </div>
);

const RegistrySection = () => (
	<section id='registry-ecosystem' className='scroll-mt-24 border-t border-ink/10 bg-paper-mid px-6 py-20 text-ink md:py-32'>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<div className='mx-auto max-w-5xl text-center'>
					<h2 className='text-balance text-4xl font-medium leading-[1.05] tracking-[-0.035em] text-ink md:text-6xl'>Whatever the workload, there&apos;s a package for it.</h2>
					<p className='mx-auto mt-5 max-w-3xl text-balance text-base leading-relaxed text-ink-soft md:text-lg'>Extend agentOS with agents, filesystems, browsers, and software from one registry.</p>
				</div>
			</Reveal>

			<Reveal>
				<div className='mt-12 overflow-hidden md:mt-16'>
					<div className='flex flex-col gap-3'>
						<RegistryMarqueeRow apps={registryRowA} direction='left' />
						<RegistryMarqueeRow apps={registryRowB} direction='right' />
					</div>
					<div className='mt-8 flex items-center justify-center'>
						<a href='/registry' className='selection-dark inline-flex flex-shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-md bg-ink px-4 py-2 text-sm font-medium text-cream transition-colors hover:bg-ink/85'>
							Explore the Registry
							<ArrowRight className='h-4 w-4' />
						</a>
					</div>
				</div>
			</Reveal>
		</div>
	</section>
);

// --- Benchmarks ---
// Benchmark data (computed from raw inputs in bench.ts)
import { benchColdStart, benchWorkloads, BENCHMARK_DATE, SANDBOX_COLDSTART_PROVIDER, SANDBOX_COST_PROVIDER, sandboxCostPerSec, type WorkloadKey } from '../../../data/bench';

function BenchInfoTooltip({ children }: { children: React.ReactNode }) {
	// Anchored to the icon itself: the wrapper is positioned and the tooltip
	// opens directly above it. The pb-2 gap lives inside the hover target so
	// the pointer can travel from icon to tooltip (for the links) without a
	// dead zone. The tooltip stays an ink plate — the site's surface for data
	// asides — floating over the light card.
	return (
		<span className='group/tip relative ml-1.5 inline-flex align-middle'>
			<svg
				className='h-3.5 w-3.5 cursor-help text-ink/30 transition-colors group-hover/tip:text-ink/60'
				viewBox='0 0 16 16'
				fill='currentColor'
			>
				<path d='M8 0a8 8 0 100 16A8 8 0 008 0zm1 12H7V7h2v5zm-1-6a1 1 0 110-2 1 1 0 010 2z' />
			</svg>
			<span className='pointer-events-none absolute bottom-full left-0 z-50 pb-2 opacity-0 transition-opacity duration-200 group-hover/tip:pointer-events-auto group-hover/tip:opacity-100'>
				<span className='block w-max max-w-[min(20rem,80vw)] rounded-lg border border-cream/15 bg-ink p-3 text-left text-[11px] leading-relaxed text-cream/80 shadow-xl [&_a]:text-accent [&_a]:underline [&_a]:underline-offset-2 [&_strong]:font-medium [&_strong]:text-cream'>
					{children}
				</span>
			</span>
		</span>
	);
}

// Pill options in the card's caption bar, reusing the site's rounded-full
// ring pill (carousel chevrons, the cards' ? buttons); the active option is
// filled ink like the primary buttons.
function BenchToggle({ options, active, onChange }: { options: string[]; active: number; onChange: (idx: number) => void }) {
  return (
    <div className='flex flex-wrap items-center gap-1.5'>
      {options.map((label, i) => {
        const isActive = i === active;
        return (
          <button
            key={label}
            type='button'
            onClick={() => onChange(i)}
            aria-pressed={isActive}
            className={`whitespace-nowrap rounded-full px-2.5 py-1 font-sans text-[11px] font-medium transition-colors ${
              isActive
                ? 'bg-ink text-cream'
                : 'text-ink-soft ring-1 ring-inset ring-ink/15 hover:text-ink hover:ring-ink/30'
            }`}
          >
            {label}
          </button>
        );
      })}
    </div>
  );
}

interface BenchRowEntry {
	label: React.ReactNode;
	value: string;
	highlight?: boolean;
}

// Splits a stat string into a leading symbol prefix, the numeric portion, and a
// trailing unit suffix so the number can be counted up while the units stay put.
// Returns null when there is no number to animate (e.g. "Infinite").
function parseStatNumber(text: string) {
	const match = text.match(/^([^\d-]*)(-?[\d,]*\.?\d+)(.*)$/);
	if (!match) return null;
	const [, prefix, rawNumber, suffix] = match;
	const normalized = rawNumber.replace(/,/g, '');
	const decimals = normalized.includes('.') ? normalized.split('.')[1].length : 0;
	return {
		prefix,
		suffix,
		value: Number.parseFloat(normalized),
		decimals,
		grouped: rawNumber.includes(','),
	};
}

// Counts the numeric part of a stat from 0 up to its value. The first run is
// gated on `active` (the card scrolling into view) and only fires once; later
// value changes (toggling workload or tier) re-trigger the count from the
// previous value. Honors reduced-motion by rendering the final value outright.
function CountUpStat({ text, active }: { text: string; active: boolean }) {
	const parsed = useMemo(() => parseStatNumber(text), [text]);
	const reducedMotion = useReducedMotion();
	const target = parsed?.value ?? 0;

	const [display, setDisplay] = useState(0);
	const startedRef = useRef(false);
	const fromRef = useRef(0);
	const rafRef = useRef(0);

	useEffect(() => {
		if (!parsed) return;
		if (reducedMotion) {
			setDisplay(target);
			fromRef.current = target;
			startedRef.current = true;
			return;
		}
		// Not yet scrolled into view: stay primed at zero for the first count-up.
		if (!active) {
			if (!startedRef.current) setDisplay(0);
			return;
		}
		const from = startedRef.current ? fromRef.current : 0;
		startedRef.current = true;
		const duration = 850;
		let start = 0;
		const step = (now: number) => {
			if (!start) start = now;
			const t = Math.min(1, (now - start) / duration);
			const eased = 1 - (1 - t) ** 3;
			setDisplay(from + (target - from) * eased);
			if (t < 1) {
				rafRef.current = requestAnimationFrame(step);
			} else {
				fromRef.current = target;
			}
		};
		rafRef.current = requestAnimationFrame(step);
		return () => cancelAnimationFrame(rafRef.current);
	}, [parsed, target, active, reducedMotion]);

	if (!parsed) return <>{text}</>;

	const formatted = parsed.grouped
		? display.toLocaleString(undefined, {
				minimumFractionDigits: parsed.decimals,
				maximumFractionDigits: parsed.decimals,
			})
		: display.toFixed(parsed.decimals);

	return (
		<span className='tabular-nums'>
			{parsed.prefix}
			{formatted}
			{parsed.suffix}
		</span>
	);
}

// Light data card on the site's card surface: title, headline multiplier,
// comparison rows, and an optional tier toggle in the caption bar. The
// agentOS row carries pine, matching the ledger's "agentOS" column above.
function BenchCard({
  title,
  statNote,
  verb,
  toggle,
  rows,
  note,
  onHelp,
  helpLabel,
  helpTip,
}: {
  title: string;
  statNote: string;
  verb: string;
  toggle?: React.ReactNode;
  rows: BenchRowEntry[];
  note?: string;
  onHelp?: () => void;
  helpLabel?: string;
  helpTip?: React.ReactNode;
}) {
  // Trigger the count-up the first time the card scrolls into view, once.
  const [inView, setInView] = useState(false);

  return (
    <motion.div
      className={`flex h-full flex-col p-6 md:p-7 ${CARD_SURFACE}`}
      onViewportEnter={() => setInView(true)}
      viewport={{ once: true, margin: '-10% 0px' }}
    >
      <div className='flex min-h-[2.5rem] items-start justify-between gap-3'>
        <span className='text-sm font-medium text-ink'>{title}</span>
        {onHelp ? (
          <button
            type='button'
            onClick={onHelp}
            aria-label={helpLabel}
            title={helpLabel}
            className='flex h-5 w-5 flex-none items-center justify-center rounded-full text-[11px] font-medium text-ink-soft ring-1 ring-inset ring-ink/15 transition-colors hover:text-ink hover:ring-ink/30'
          >
            ?
          </button>
        ) : helpTip ? (
          // Anchored under the ? itself, opening down-left from the card corner.
          <span className='group/tip relative inline-flex'>
            <span className='flex h-5 w-5 flex-none cursor-help items-center justify-center rounded-full text-[11px] font-medium text-ink-soft ring-1 ring-inset ring-ink/15 transition-colors group-hover/tip:text-ink group-hover/tip:ring-ink/30'>
              ?
            </span>
            <span className='pointer-events-none absolute right-0 top-full z-50 pt-2 opacity-0 transition-opacity duration-200 group-hover/tip:opacity-100'>
              <span className='block w-max max-w-[min(18rem,80vw)] rounded-lg border border-cream/15 bg-ink p-3 text-left text-[11px] leading-relaxed text-cream/80 shadow-xl'>
                {helpTip}
              </span>
            </span>
          </span>
        ) : null}
      </div>

      {/* Verdict: the headline multiplier */}
      <div className='mt-5 flex items-baseline gap-2'>
        <span className='font-sans text-[2.75rem] font-medium leading-[1.0] tracking-[-0.02em] tabular-nums text-ink md:text-5xl'>
          <CountUpStat text={statNote} active={inView} />
        </span>
        <span className='font-sans text-lg font-medium text-ink-faint md:text-xl'>{verb}</span>
      </div>

      {/* Comparison ledger: ours vs theirs, same unit, right-aligned */}
      <div className='mb-6 mt-6 divide-y divide-ink/10 border-y border-ink/10'>
        {rows.map((row, i) => (
          <div key={i} className='flex items-baseline justify-between gap-4 py-2.5'>
            <span className={`inline-flex min-w-0 items-baseline text-sm ${row.highlight ? 'font-medium text-pine' : 'text-ink-faint'}`}>
              {row.label}
            </span>
            <span className={`whitespace-nowrap font-mono text-[15px] tabular-nums ${row.highlight ? 'font-medium text-ink' : 'font-normal text-ink-faint'}`}>
              {row.value}
            </span>
          </div>
        ))}
      </div>

      {/* Caption bar flush with the card foot: holds the tier options or a
          one-line measurement note. */}
      {toggle || note ? (
        <div className='-mx-6 -mb-6 mt-auto border-t border-ink/10 px-6 py-2.5 font-mono text-[11px] leading-relaxed text-ink-faint md:-mx-7 md:-mb-7 md:px-7'>
          {toggle ?? note}
        </div>
      ) : null}
    </motion.div>
  );
}

function BenchColdStartChart({ onHelp }: { onHelp?: () => void }) {
	const groups = benchColdStart;
	// Default to p50 so the hero chip's claim and this card's first-shown number agree.
	const [active, setActive] = useState(0);
	const g = groups[active];

	return (
		<BenchCard
			title='Cold Start'
			statNote={`${Math.round(g.sandbox / g.agentOS)}x`}
				verb='faster'
			onHelp={onHelp}
			helpLabel='Watch how a cold start breaks down'
			toggle={<BenchToggle options={groups.map((t) => t.label)} active={active} onChange={setActive} />}
			rows={[
				{
					label: (
						<>
							agentOS
							<BenchInfoTooltip>
								<strong>What&apos;s measured:</strong> Time from requesting an execution to first code running.
								<br /><br />
								<strong>Why the gap:</strong> agentOS runs agents in-process — WASM inside your host. No VM to boot, no network hop, no disk image. Sandboxes must boot an entire environment, allocate memory, and establish a network connection before code can run.
								<br /><br />
								<strong>Sandbox baseline:</strong> {SANDBOX_COLDSTART_PROVIDER}, the fastest mainstream sandbox provider as of {BENCHMARK_DATE}.
								<br /><br />
								<strong>agentOS:</strong> Median of 10,000 runs (100 iterations x 100 samples) on Intel i7-12700KF.
							</BenchInfoTooltip>
						</>
					),
					value: `${g.agentOS} ms`,
					highlight: true,
				},
				{ label: 'Fastest sandbox', value: `${g.sandbox.toLocaleString()} ms` },
			]}
		/>
	);
}

function BenchMemoryBar({ workload }: { workload: WorkloadKey }) {
	const mem = benchWorkloads[workload].memory;
	const [memMult, memVerb] = mem.multiplier.split(' ');

	return (
		<BenchCard
			title='Memory Per Instance'
			statNote={memMult}
				verb={memVerb}
			rows={[
				{
					label: (
						<>
							agentOS
							<BenchInfoTooltip>
								<strong>What&apos;s measured:</strong> Memory footprint added per concurrent execution.
								<br /><br />
								<strong>Why the gap:</strong> In-process isolates share the host's memory. Each additional execution only adds its own heap and stack. Sandboxes allocate a dedicated environment with a minimum memory reservation, even if the code inside uses far less.
								<br /><br />
								<strong>Sandbox baseline:</strong> {SANDBOX_COST_PROVIDER}, the cheapest mainstream sandbox provider as of {BENCHMARK_DATE}. Default sandbox: 1 vCPU + 1 GiB RAM.
								<br /><br />
								<strong>agentOS:</strong> {workload === 'agent' ? `${benchWorkloads.agent.memory.agentOS} for a full Pi coding agent session with MCP servers and file system mounts.` : `${benchWorkloads.shell.memory.agentOS} for the minimal shell workload under sustained load.`}
							</BenchInfoTooltip>
						</>
					),
					value: mem.agentOS,
					highlight: true,
				},
				{ label: 'Cheapest sandbox', value: mem.sandbox },
			]}
			helpTip='Sandboxes reserve idle RAM per agent.'
		/>
	);
}

function BenchCostChart({ workload }: { workload: WorkloadKey }) {
	const tiers = benchWorkloads[workload].cost;
	const sandboxCost = benchWorkloads[workload].sandboxCost;
	// Default to AWS ARM so the hero chip's claim and this card's first-shown number agree.
	const [active, setActive] = useState(() => Math.max(0, tiers.findIndex((tier) => tier.label === 'AWS ARM')));
	const t = tiers[active];
	const [costMult, costVerb] = t.multiplier.split(' ');

	return (
		<BenchCard
			title='Cost Per Execution-Second'
			statNote={costMult}
				verb={costVerb}
			toggle={<BenchToggle options={tiers.map((tier) => tier.label)} active={active} onChange={setActive} />}
			rows={[
				{
					label: (
						<>
							agentOS
							<BenchInfoTooltip>
								<strong>What&apos;s measured:</strong> <code className='rounded bg-cream/10 px-1 py-0.5 text-[10px]'>server price per second / concurrent executions per server</code>
								<br /><br />
								<strong>Why it&apos;s cheaper:</strong> Each execution uses {benchWorkloads[workload].memory.agentOS} instead of a {benchWorkloads[workload].memory.sandbox} sandbox minimum. And you run on your own hardware, which is significantly cheaper than per-second sandbox billing.
								<br /><br />
								<strong>Sandbox baseline:</strong> {SANDBOX_COST_PROVIDER}, the cheapest mainstream sandbox provider as of {BENCHMARK_DATE}. Default sandbox: 1 vCPU + 1 GiB RAM at $0.0504/vCPU-h + $0.0162/GiB-h.
								<br /><br />
								<strong>agentOS:</strong> {benchWorkloads[workload].memory.agentOS} baseline per execution, assuming 70% utilization (industry-standard HPA scaling threshold). Select a hardware tier above to compare.
							</BenchInfoTooltip>
						</>
					),
					value: t.value,
					highlight: true,
				},
				{ label: 'Cheapest sandbox', value: sandboxCost },
			]}
			helpTip='Assumes one agent per sandbox, needed for isolation.'
		/>
	);
}

function BenchmarkSection({ onShowColdStart }: { onShowColdStart?: () => void }) {
	// Single measured workload; the coding-agent variant lives on in bench.ts
	// data but is not surfaced here.
	const workload: WorkloadKey = 'shell';

	return (
		<motion.div
			initial={{ opacity: 0, y: 20 }}
			whileInView={{ opacity: 1, y: 0 }}
			viewport={{ once: true }}
			transition={{ duration: 0.5 }}
		>

			<div className='grid grid-cols-1 gap-6 md:grid-cols-2 lg:grid-cols-3'>
				<div id='bench-cold-start' className='scroll-mt-24'>
					<BenchColdStartChart onHelp={onShowColdStart} />
				</div>
				<div id='bench-memory' className='scroll-mt-24'>
					<BenchMemoryBar workload={workload} />
				</div>
				<div id='bench-cost' className='scroll-mt-24'>
					<BenchCostChart workload={workload} />
				</div>
			</div>

		</motion.div>
	);
}

// --- Floating foundation map ---
// Execution, filesystem, and orchestration split evenly across the section.
// The bubbles name recognizable integrations and patterns without turning the
// section into an inventory.
interface FoundationBubble {
	label: string;
	src?: string;
	icon?: React.ComponentType<{ className?: string }>;
	glyph?: string;
	left: string;
	top: string;
	size: number;
	delay: number;
	duration: number;
	rotation: number;
}

interface FoundationCardData {
	title: string;
	description: string;
	href: string;
	bubbles: FoundationBubble[];
}

const foundationCards: FoundationCardData[] = [
	{
		title: 'Execution',
		description: 'Run the language and tools each task needs.',
		href: '#execution',
		bubbles: [
			{ label: 'Node.js', src: '/images/registry/nodejs.svg', left: '10%', top: '11%', size: 88, delay: 0.1, duration: 7.7, rotation: -7 },
			{ label: 'Python', src: 'https://upload.wikimedia.org/wikipedia/commons/thumb/3/31/Python-logo.png/120px-Python-logo.png', left: '62%', top: '9%', size: 84, delay: 0.8, duration: 7.1, rotation: 7 },
			{ label: 'Bash', src: '/images/registry/bash.svg', left: '37%', top: '40%', size: 82, delay: 1.4, duration: 8.2, rotation: -3 },
		],
	},
	{
		title: 'Filesystem',
		description: 'Mount durable and remote storage as normal files.',
		href: '#filesystem',
		bubbles: [
			{ label: 'S3', src: '/images/registry/s3.svg', left: '9%', top: '10%', size: 82, delay: 0.2, duration: 7.4, rotation: -7 },
			{ label: 'Drive', src: '/images/registry/google-drive.svg', left: '62%', top: '8%', size: 80, delay: 0.8, duration: 8.1, rotation: 7 },
			{ label: 'SQLite', src: '/images/registry/sqlite3.svg', left: '34%', top: '35%', size: 88, delay: 1.2, duration: 7.6, rotation: -4 },
			{ label: 'Host', icon: HardDrive, left: '74%', top: '46%', size: 66, delay: 1.7, duration: 6.9, rotation: 8 },
		],
	},
	{
		title: 'Orchestration',
		description: 'Compose modern agent patterns in code.',
		href: '#orchestration',
		bubbles: [
			{ label: 'Loops', icon: RefreshCw, left: '9%', top: '10%', size: 76, delay: 0.1, duration: 7.4, rotation: -7 },
			{ label: 'Multiplayer', icon: Users, left: '62%', top: '8%', size: 84, delay: 0.7, duration: 8.1, rotation: 7 },
			{ label: 'Workflows', icon: Workflow, left: '32%', top: '36%', size: 92, delay: 1.2, duration: 7.8, rotation: -4 },
			{ label: 'Agent-to-Agent', icon: Layers, left: '72%', top: '45%', size: 76, delay: 1.7, duration: 6.9, rotation: 8 },
		],
	},
];

const scrollToFoundationSection = (event: React.MouseEvent<HTMLAnchorElement>, href: string, reduceMotion: boolean) => {
	if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
	const target = document.querySelector<HTMLElement>(href);
	if (!target) return;
	event.preventDefault();
	window.history.pushState(null, '', href);
	target.scrollIntoView({ behavior: reduceMotion ? 'auto' : 'smooth', block: 'start' });
};

const FloatingFoundationCard = ({ card }: { card: FoundationCardData }) => {
	const reduceMotion = useReducedMotion() ?? false;

	return (
		<motion.a
			href={card.href}
			onClick={(event) => scrollToFoundationSection(event, card.href, reduceMotion)}
			animate='rest'
			whileHover='active'
			initial={{ opacity: 0, y: 20 }}
			whileInView={{ opacity: 1, y: 0 }}
			viewport={{ once: true }}
			transition={{ duration: 0.5 }}
			className='group relative block min-h-[24rem] cursor-pointer overflow-hidden rounded-2xl bg-white/75 ring-1 ring-ink/[0.10] shadow-[inset_0_1px_0_rgba(255,255,255,0.95),0_2px_5px_-2px_rgba(20,20,22,0.12),0_18px_40px_-28px_rgba(20,20,22,0.30)] transition-[background-color,--tw-ring-color,box-shadow] duration-300 hover:bg-white hover:ring-ink/[0.18] hover:shadow-[inset_0_1px_0_rgba(255,255,255,1),0_4px_9px_-3px_rgba(20,20,22,0.14),0_22px_48px_-24px_rgba(20,20,22,0.34)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pine/60 motion-reduce:transition-none lg:aspect-square lg:min-h-0'
		>
			<div aria-hidden='true' className='absolute inset-0'>
				{card.bubbles.map((bubble) => {
					const Icon = bubble.icon;
					return (
						<div key={bubble.label} className='absolute' style={{ left: bubble.left, top: bubble.top }}>
							<motion.div
								variants={{
									rest: { y: 0, scale: 1 },
									active: reduceMotion
										? { y: 0, scale: 1 }
										: {
											y: [0, -17, 2, 0],
											scale: [1, 1.035, 1, 1],
											transition: { duration: bubble.duration * 0.62, delay: bubble.delay * 0.3, repeat: Infinity, ease: 'easeInOut' },
										},
								}}
								className='flex flex-col items-center justify-center rounded-2xl bg-ink/[0.035] opacity-65 grayscale ring-1 ring-ink/[0.08] shadow-none transition-[filter,opacity,box-shadow,background-color,--tw-ring-color] duration-300 group-hover:bg-gradient-to-b group-hover:from-white group-hover:to-[#ededf0] group-hover:opacity-100 group-hover:grayscale-0 group-hover:ring-ink/10 group-hover:shadow-[0_2px_6px_-1px_rgba(20,20,22,0.10),0_16px_34px_-12px_rgba(20,20,22,0.26)] motion-reduce:transition-none'
								style={{ width: bubble.size, height: bubble.size, rotate: bubble.rotation }}
							>
								{bubble.src ? <img src={bubble.src} alt='' className='h-7 w-7 object-contain' /> : Icon ? <Icon className='h-7 w-7 text-ink-faint transition-colors duration-300 group-hover:text-pine' /> : <span className='text-2xl font-medium leading-none text-ink-faint transition-colors duration-300 group-hover:text-pine'>{bubble.glyph}</span>}
								<span className='mt-1.5 px-1 text-center text-[9px] font-medium leading-tight text-ink-soft transition-colors duration-300 group-hover:text-ink'>{bubble.label}</span>
							</motion.div>
						</div>
					);
				})}
			</div>
			<div className='pointer-events-none absolute inset-x-0 bottom-0 h-1/2 bg-gradient-to-t from-white via-white/95 to-transparent' />
			<div className='absolute inset-x-0 bottom-0 p-6 md:p-8'>
				<h3 className='text-2xl font-medium tracking-[-0.015em] text-ink md:text-3xl'>{card.title}</h3>
				<p className='mt-2 text-sm leading-relaxed text-ink-soft md:text-base'>{card.description}</p>
			</div>
		</motion.a>
	);
};

const FloatingFoundation = () => {
	const reduceMotion = useReducedMotion() ?? false;

	return (
		<div>
			<div className='grid gap-4 lg:grid-cols-3'>
				{foundationCards.map((card) => (
					<FloatingFoundationCard key={card.title} card={card} />
				))}
			</div>
			<a href='#apps' onClick={(event) => scrollToFoundationSection(event, '#apps', reduceMotion)} className='group mx-auto mt-9 flex w-fit max-w-full flex-wrap items-center justify-center gap-x-2 gap-y-1 text-center text-sm leading-relaxed text-ink-soft transition-colors hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pine/50 focus-visible:ring-offset-2'>
				<span className='font-medium text-ink'>Apps</span>
				<span>Deploy AI-generated applications for each of your users.</span>
				<ArrowRight aria-hidden='true' className='h-3.5 w-3.5 text-ink-faint transition-transform duration-200 group-hover:translate-x-1' />
			</a>
		</div>
	);
};

const AgentCompatibilitySection = () => (
	<section className='border-t border-ink/10 bg-paper-mid px-6 py-16 text-ink md:py-24'>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<div className='mx-auto max-w-4xl text-center'>
					<h2 className='text-balance text-3xl font-medium leading-[1.08] tracking-[-0.025em] text-ink md:text-5xl'>Bring any agent or framework.</h2>
					<p className='mx-auto mt-5 max-w-3xl text-balance text-base leading-relaxed text-ink-soft md:text-lg'>Pi, Claude Code, Codex, and OpenCode on the same virtual operating system—with native Rivet orchestration and integrations for Eve and Flue.</p>
				</div>
			</Reveal>

			<Reveal>
				<div className='mx-auto mt-10 flex flex-wrap items-center justify-center gap-4 md:mt-12'>
					{/* Casually rotated agent cards spread out and straighten on hover. */}
					<motion.div className='flex items-center pl-2' initial='rest' whileHover='spread' animate='rest'>
						{agents.map((agent, i) => {
							const tilt = [-16, -8, 0, 9, 17][i] ?? 0;
							return (
								<motion.a
									key={agent.name}
									href={agent.href}
									aria-label={agent.name}
									variants={{
										rest: { rotate: tilt, marginLeft: i === 0 ? 0 : -18 },
										spread: { rotate: 0, marginLeft: i === 0 ? 0 : 7 },
									}}
									transition={{ duration: 0.3, ease: [0.22, 1, 0.36, 1] }}
									style={{ zIndex: i }}
									className='group relative flex h-14 w-14 cursor-pointer items-center justify-center rounded-2xl border border-ink/10 bg-white shadow-[0_3px_10px_-2px_rgba(20,20,22,0.16)] md:h-16 md:w-16'
								>
									<img src={agent.src} alt='' aria-hidden='true' className='h-8 w-8 object-contain md:h-9 md:w-9' />
									<span className='pointer-events-none absolute top-full left-1/2 mt-2 -translate-x-1/2 whitespace-nowrap text-xs font-medium text-ink-soft opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-visible:opacity-100'>
										{agent.name}
									</span>
								</motion.a>
							);
						})}
					</motion.div>

					<span aria-hidden='true' className='inline-flex h-14 shrink-0 items-center font-mono text-base text-ink-faint md:h-16 md:text-lg'>
						&amp;
					</span>

					<motion.div className='flex items-center pl-2' initial='rest' whileHover='spread' animate='rest'>
						{frameworks.map((framework, i) => {
							const tilt = [-12, 0, 12][i] ?? 0;
							return (
								<motion.a
									key={framework.name}
									href={framework.href}
									aria-label={framework.comingSoon ? `${framework.name} (coming soon)` : framework.name}
									variants={{
										rest: { rotate: tilt, marginLeft: i === 0 ? 0 : -10 },
										spread: { rotate: 0, marginLeft: i === 0 ? 0 : 7 },
									}}
									transition={{ duration: 0.3, ease: [0.22, 1, 0.36, 1] }}
									style={{ zIndex: i }}
									className='group relative flex h-14 w-14 cursor-pointer items-center justify-center rounded-2xl border border-ink/10 bg-white shadow-[0_3px_10px_-2px_rgba(20,20,22,0.16)] md:h-16 md:w-16'
								>
									<img
										src={framework.src}
										alt=''
										aria-hidden='true'
										className={framework.wordmark ? 'w-9 object-contain opacity-90 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100 md:w-10' : 'h-8 w-8 object-contain opacity-90 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100 md:h-9 md:w-9'}
									/>
									{framework.comingSoon && <span className='pointer-events-none absolute -bottom-2 left-1/2 z-10 -translate-x-1/2 whitespace-nowrap rounded-full border border-ink/10 bg-paper px-1.5 py-0.5 text-[6px] font-medium uppercase tracking-[0.04em] text-ink/55 shadow-[0_1px_4px_rgba(20,20,22,0.08)] md:text-[7px]'>Coming soon</span>}
									<span className='pointer-events-none absolute top-full left-1/2 mt-2 -translate-x-1/2 whitespace-nowrap text-xs font-medium text-ink-soft opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-visible:opacity-100'>
										{framework.name}
									</span>
								</motion.a>
							);
						})}
					</motion.div>
				</div>
				<a href='/docs/agents/custom' className='mx-auto mt-9 flex w-fit items-center gap-1.5 text-sm text-ink-faint transition-colors hover:text-ink'>
					Or build your own agent
					<ArrowRight className='h-3.5 w-3.5' />
				</a>
			</Reveal>
		</div>
	</section>
);

// --- Runtime architecture ---
const runtimeFeatures = [
	{
		title: 'Library',
		contrast: 'not extra infrastructure',
		description: 'Install agentOS from npm and run it inside the backend process you already operate.',
	},
	{
		title: 'WebAssembly + V8',
		contrast: 'not a microVM',
		description: 'V8 isolates run JavaScript while WebAssembly contains compiled tools inside the same compact runtime.',
	},
	{
		title: 'Direct function calls',
		contrast: 'not extra APIs',
		description: 'Connect agents to your application with ordinary JavaScript calls instead of another network service.',
	},
	{
		title: 'Scoped access',
		contrast: 'not exposed credentials',
		description: 'Bind trusted host functions without ever giving the agent your raw secrets.',
	},
	{
		title: 'One process',
		contrast: 'not a VM per agent',
		description: 'Many isolated agent operating systems share one compact runtime instead of duplicating a full microVM stack.',
	},
	{
		title: 'Runs anywhere',
		contrast: 'no nested virtualization',
		description: 'Deploy on standard Linux infrastructure without a hypervisor or nested virtualization.',
	},
];

const runtimeColdStart = benchColdStart[0];
const runtimeMemory = benchWorkloads.shell.memory;
const runtimeCost = benchWorkloads.shell.cost.find((tier) => tier.label === 'AWS ARM') ?? benchWorkloads.shell.cost[0];
const runtimeAgentOSCostPerExecutionSecond = runtimeCost.costPerHour / 3600 / runtimeCost.execs;
const runtimeMeterVmCount = 100_000;
const runtimeAgentOSMeterRate = runtimeAgentOSCostPerExecutionSecond * runtimeMeterVmCount;
const runtimeSandboxMeterRate = sandboxCostPerSec * runtimeMeterVmCount;
const runtimeMeterRunSeconds = 12;
const runtimeColdStartDots = Array.from({ length: 81 }, (_, index) => index);

const runtimeBenchmarkDetails = {
	coldStart: [
		{ label: "What's measured:", text: 'Time from requesting an execution to first code running.' },
		{ label: 'Why the gap:', text: 'agentOS runs in-process with no VM boot, network hop, or disk image. Sandboxes must allocate and boot an entire environment before code can run.' },
		{ label: 'Sandbox baseline:', text: `${SANDBOX_COLDSTART_PROVIDER}, the fastest mainstream sandbox provider as of ${BENCHMARK_DATE}.` },
		{ label: 'agentOS:', text: 'Median of 10,000 runs (100 iterations × 100 samples) on Intel i7-12700KF.' },
	],
	memory: [
		{ label: "What's measured:", text: 'Memory footprint added per concurrent execution.' },
		{ label: 'Why the gap:', text: 'In-process isolates share host memory, so each execution adds only its own heap and stack. Sandboxes reserve a dedicated environment even when its code uses far less.' },
		{ label: 'Sandbox baseline:', text: `${SANDBOX_COST_PROVIDER}, the cheapest mainstream sandbox provider as of ${BENCHMARK_DATE}. Default sandbox: 1 vCPU + 1 GiB RAM.` },
		{ label: 'agentOS:', text: `${runtimeMemory.agentOS} for the minimal shell workload under sustained load.` },
	],
	cost: [
		{ label: "What's measured:", text: 'Server price per second divided by concurrent executions per server.' },
		{ label: 'Why the gap:', text: `Each execution uses ${runtimeMemory.agentOS} instead of a ${runtimeMemory.sandbox} sandbox minimum, while agentOS runs on your own hardware rather than per-second sandbox billing.` },
		{ label: 'Sandbox baseline:', text: `${SANDBOX_COST_PROVIDER}, the cheapest mainstream sandbox provider as of ${BENCHMARK_DATE}. Default sandbox: 1 vCPU + 1 GiB RAM at $0.0504/vCPU-h + $0.0162/GiB-h.` },
		{ label: 'agentOS:', text: `${runtimeMemory.agentOS} per execution on AWS ARM, assuming 70% utilization.` },
	],
};

function RuntimeBenchInfo({
	label,
	details,
	align = 'left',
}: {
	label: string;
	details: Array<{ label: string; text: string }>;
	align?: 'left' | 'right';
}) {
	return (
		<span className='group/runtime-info relative inline-flex'>
			<button
				type='button'
				aria-label={label}
				aria-haspopup='dialog'
				className='flex h-5 w-5 items-center justify-center rounded-full text-[11px] font-medium text-ink-faint ring-1 ring-inset ring-ink/15 transition-colors hover:text-ink hover:ring-ink/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pine/50'
			>
				?
			</button>
			<span
				className={`invisible absolute top-full z-50 w-72 max-w-[80vw] pt-2 opacity-0 transition-[opacity,visibility] group-hover/runtime-info:visible group-hover/runtime-info:opacity-100 group-focus-within/runtime-info:visible group-focus-within/runtime-info:opacity-100 ${align === 'right' ? 'right-0' : 'left-0'}`}
			>
				<span role='dialog' aria-label={label} className='block rounded-lg border border-cream/15 bg-ink p-3 text-left text-[11px] leading-relaxed text-cream/75 shadow-xl'>
					{details.map((detail) => (
						<span key={detail.label} className='mt-2 block first:mt-0'>
							<strong className='font-medium text-cream'>{detail.label}</strong> {detail.text}
						</span>
					))}
					<a href='/docs/performance#reproducing' className='mt-3 inline-flex font-medium text-cream underline decoration-cream/30 underline-offset-4 transition-colors hover:text-white hover:decoration-cream/70'>
						Methodology &amp; reproduction →
					</a>
				</span>
			</span>
		</span>
	);
}

function RuntimeCostMeter() {
	const meterRef = useRef<HTMLDivElement>(null);
	const reducedMotion = useReducedMotion();
	const [elapsedSeconds, setElapsedSeconds] = useState(runtimeMeterRunSeconds);

	useEffect(() => {
		if (reducedMotion) {
			setElapsedSeconds(runtimeMeterRunSeconds);
			return;
		}

		const meter = meterRef.current;
		if (!meter) return;

		let animationFrame = 0;
		let animationStart = 0;
		let running = false;
		const runDurationMs = runtimeMeterRunSeconds * 1000;
		const resetPauseMs = 750;

		const tick = (now: number) => {
			if (!running) return;
			const elapsed = (now - animationStart) % (runDurationMs + resetPauseMs);
			setElapsedSeconds(elapsed < runDurationMs ? elapsed / 1000 : 0);
			animationFrame = requestAnimationFrame(tick);
		};

		const observer = new IntersectionObserver(([entry]) => {
			if (entry?.isIntersecting && !running) {
				running = true;
				animationStart = performance.now();
				animationFrame = requestAnimationFrame(tick);
			} else if (!entry?.isIntersecting && running) {
				running = false;
				cancelAnimationFrame(animationFrame);
				setElapsedSeconds(runtimeMeterRunSeconds);
			}
		});

		observer.observe(meter);
		return () => {
			running = false;
			cancelAnimationFrame(animationFrame);
			observer.disconnect();
		};
	}, [reducedMotion]);

	return (
		<div
			ref={meterRef}
			className='mt-auto pt-7'
			aria-label={`Modeled live cost for ${runtimeMeterVmCount.toLocaleString('en-US')} concurrent VMs: agentOS accrues $${runtimeAgentOSMeterRate.toFixed(4)} per second and a sandbox accrues $${runtimeSandboxMeterRate.toFixed(2)} per second`}
		>
			<p className='mb-4 font-mono text-[10px] uppercase tracking-[0.1em] text-ink-faint'>
				Metering {runtimeMeterVmCount.toLocaleString('en-US')} concurrent VMs
			</p>
			<div className='border-y border-ink/10'>
				<div className='flex items-baseline justify-between gap-3 py-3.5'>
					<span className='text-xs font-medium text-pine'>agentOS</span>
					<span className='font-mono text-xl font-medium tracking-[-0.03em] text-pine'>${(runtimeAgentOSMeterRate * elapsedSeconds).toFixed(2)}</span>
				</div>
				<div className='flex items-baseline justify-between gap-3 border-t border-ink/10 py-3.5'>
					<span className='text-xs font-medium text-ink-soft'>Sandbox</span>
					<span className='font-mono text-xl font-medium tracking-[-0.03em] text-ink-soft'>${(runtimeSandboxMeterRate * elapsedSeconds).toFixed(2)}</span>
				</div>
			</div>
		</div>
	);
}

const RuntimeArgumentSection = () => (
	<section className='border-t border-ink/10 bg-paper px-6 py-32 text-ink md:py-40'>
		<style>{`
			.runtime-benchmark-dot {
				width: 6px;
				height: 6px;
				border-radius: 9999px;
				background: rgba(27, 25, 22, 0.12);
				animation-timing-function: steps(1, end);
				animation-iteration-count: infinite;
			}
			.runtime-benchmark-dot--fast {
				animation-name: runtime-benchmark-dot-fast;
				animation-duration: 2200ms;
				animation-timing-function: ease-out;
			}
			.runtime-benchmark-dot--slow {
				animation-name: runtime-benchmark-dot-slow;
				animation-duration: 35640ms;
			}
			@keyframes runtime-benchmark-dot-fast {
				0%, 3% { background: rgba(27, 25, 22, 0.12); box-shadow: none; transform: scale(0.92); }
				8% { background: #305b46; box-shadow: 0 0 7px rgba(48, 91, 70, 0.3); transform: scale(1.22); }
				20% { background: rgba(48, 91, 70, 0.4); box-shadow: none; transform: scale(1); }
				34%, 100% { background: rgba(27, 25, 22, 0.12); box-shadow: none; transform: scale(0.92); }
			}
			@keyframes runtime-benchmark-dot-slow {
				0%, 1.235% { background: rgba(27, 25, 22, 0.7); transform: scale(1.18); }
				1.236%, 100% { background: rgba(27, 25, 22, 0.12); transform: scale(1); }
			}
			@media (prefers-reduced-motion: reduce) {
				.runtime-benchmark-dot { animation: none; }
				.runtime-benchmark-dot--fast { background: rgba(48, 91, 70, 0.65); }
			}
		`}</style>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<div className='mx-auto mb-12 max-w-5xl text-center md:mb-16'>
					<h2 className='text-balance text-4xl font-medium leading-[1.05] tracking-[-0.035em] text-ink md:text-6xl'>
						Tiny runtime.
						<br />
						Battle-tested security.
					</h2>
					<div className='mx-auto mt-7 flex max-w-4xl flex-col items-center gap-2 text-balance text-base leading-relaxed text-ink-soft md:text-lg'>
						<p className='flex w-full flex-wrap items-center justify-center gap-x-1.5'>
							<span>Powered by</span>
							<span className='inline-flex items-center gap-1 whitespace-nowrap'><img src='/images/agent-os/webassembly-logo.svg' alt='' aria-hidden='true' className='h-4 w-4 translate-y-px object-contain' /><strong className='font-semibold text-ink'>WebAssembly</strong></span>
							<span>and</span>
							<span className='inline-flex items-center gap-1 whitespace-nowrap'><img src='/images/agent-os/v8-logo.svg' alt='' aria-hidden='true' className='h-4 w-4 translate-y-px object-contain' /><strong className='font-semibold text-ink'>V8 isolates.</strong></span>
						</p>
						<p className='flex w-full flex-wrap items-center justify-center gap-x-1.5'>
							<span>The same technology powering</span>
							<span className='inline-flex items-center gap-1 whitespace-nowrap'><img src='/images/registry/chrome.svg' alt='' aria-hidden='true' className='h-4 w-4 translate-y-px object-contain' /><strong className='font-semibold text-ink'>Chrome</strong></span>
							<span>and</span>
							<span className='inline-flex items-center gap-1 whitespace-nowrap'><img src='/images/registry/cloudflare.svg' alt='' aria-hidden='true' className='h-4 w-4 translate-y-px object-contain' /><strong className='font-semibold text-ink'>Cloudflare Workers.</strong></span>
						</p>
						<p className='flex w-full flex-wrap items-center justify-center gap-x-1.5'>
							<span className='inline-flex items-center gap-1 whitespace-nowrap'><img src='/images/registry/linux.svg' alt='' aria-hidden='true' className='h-4 w-4 translate-y-px object-contain' /><strong className='font-semibold text-ink'>Linux-compatible</strong></span>
							<span>with POSIX files, processes, and networking.</span>
						</p>
					</div>
				</div>
			</Reveal>

			<Reveal>
				<div className='grid overflow-hidden rounded-2xl border-l border-t border-ink/10 sm:grid-cols-2 lg:grid-cols-3'>
					<article id='bench-cold-start' className='flex min-h-[25rem] scroll-mt-24 flex-col border-b border-r border-ink/10 bg-white/55 p-6 md:p-8'>
						<div className='flex items-center gap-2'>
							<h3 className='text-sm font-medium text-ink-soft'>Cold starts</h3>
							<RuntimeBenchInfo label='Cold-start benchmark details' details={runtimeBenchmarkDetails.coldStart} />
						</div>
						<p className='mt-5 text-4xl font-medium tracking-[-0.04em] text-ink md:text-5xl'>{Math.round(runtimeColdStart.sandbox / runtimeColdStart.agentOS)}× faster</p>
						<p className='mt-2 text-base font-medium text-pine'>{runtimeColdStart.agentOS} ms p50</p>
						<div className='mt-auto grid grid-cols-2 gap-6 pt-8'>
							<div>
								<div className='mx-auto grid w-[5.75rem] grid-cols-9 gap-1' aria-hidden='true'>
									{runtimeColdStartDots.map((index) => <span key={index} className='runtime-benchmark-dot runtime-benchmark-dot--fast' style={{ animationDelay: `${index * runtimeColdStart.agentOS}ms` }} />)}
								</div>
								<p className='mt-3 text-center text-xs font-medium text-pine'>agentOS<br />{runtimeColdStart.agentOS} ms p50</p>
							</div>
							<div>
								<div className='mx-auto grid w-[5.75rem] grid-cols-9 gap-1' aria-hidden='true'>
									{runtimeColdStartDots.map((index) => <span key={index} className='runtime-benchmark-dot runtime-benchmark-dot--slow' style={{ animationDelay: `${index * runtimeColdStart.sandbox}ms` }} />)}
								</div>
								<p className='mt-3 text-center text-xs text-ink-faint'>Sandbox<br />{runtimeColdStart.sandbox} ms p50</p>
							</div>
						</div>
					</article>

					<article id='bench-memory' className='flex min-h-[25rem] scroll-mt-24 flex-col border-b border-r border-ink/10 bg-white/55 p-6 md:p-8'>
						<div className='flex items-center gap-2'>
							<h3 className='text-sm font-medium text-ink-soft'>Memory per instance</h3>
							<RuntimeBenchInfo label='Memory benchmark details' details={runtimeBenchmarkDetails.memory} />
						</div>
						<p className='mt-5 text-4xl font-medium tracking-[-0.04em] text-ink md:text-5xl'>{runtimeMemory.multiplier.replace('x', '×')}</p>
						<p className='mt-2 text-base font-medium text-pine'>{runtimeMemory.agentOS} per instance</p>
						<div className='mt-auto flex h-40 items-end justify-center gap-12 pt-8' aria-label={`agentOS uses ${runtimeMemory.agentOS}; a sandbox uses ${runtimeMemory.sandbox}`}>
							<div className='flex flex-col items-center gap-3'>
								<div className='flex h-24 w-24 items-end justify-center'><span className='h-4 w-4 bg-pine/75 shadow-[0_5px_16px_-6px_rgba(48,91,70,0.75)]' /></div>
								<p className='text-center text-xs font-medium text-pine'>agentOS<br />{runtimeMemory.agentOS}</p>
							</div>
							<div className='flex flex-col items-center gap-3'>
								<div className='h-24 w-24 border border-ink/15 bg-ink/10' />
								<p className='text-center text-xs text-ink-faint'>Sandbox<br />{runtimeMemory.sandbox}</p>
							</div>
						</div>
					</article>

					<article id='bench-cost' className='flex min-h-[25rem] scroll-mt-24 flex-col border-b border-r border-ink/10 bg-white/55 p-6 md:p-8'>
						<div className='flex items-center gap-2'>
							<h3 className='text-sm font-medium text-ink-soft'>Cost per execution-second</h3>
							<RuntimeBenchInfo label='Cost benchmark details' details={runtimeBenchmarkDetails.cost} align='right' />
						</div>
						<p className='mt-5 text-4xl font-medium tracking-[-0.04em] text-ink md:text-5xl'>{runtimeCost.multiplier.replace('x', '×')}</p>
						<p className='mt-2 text-base font-medium text-pine'>{runtimeCost.value}</p>
						<RuntimeCostMeter />
					</article>

					{runtimeFeatures.map((feature) => (
						<article key={feature.title} className='flex min-h-48 flex-col border-b border-r border-ink/10 bg-white/35 p-6 md:p-8'>
							<h3 className='text-xl font-medium leading-snug tracking-[-0.02em] text-ink'>
								{feature.title}<span className='text-ink-faint'>, {feature.contrast}</span>
							</h3>
							<p className='mt-4 max-w-sm text-sm leading-relaxed text-ink-soft'>{feature.description}</p>
						</article>
					))}
				</div>
			</Reveal>
			<Reveal>
				<div className='mt-10 flex justify-center'>
					<a href='/docs/architecture' className='inline-flex items-center gap-1.5 text-sm text-ink-faint transition-colors hover:text-ink'>
						Read about the architecture
						<ArrowRight className='h-3.5 w-3.5' />
					</a>
				</div>
			</Reveal>
		</div>
	</section>
);

const secondaryFeatures = [
	{
		icon: RefreshCw,
		title: 'Durable agents',
		description: 'Persist sessions and transcripts so fault-tolerant agents can pause and resume across restarts.',
		docsHref: '/docs/persistence',
	},
	{
		icon: Moon,
		title: 'Sleeps when idle',
		description: 'Idle VMs sleep to free resources, then wake automatically with durable state when work arrives.',
		docsHref: '/docs/persistence',
	},
	{
		icon: Wrench,
		title: 'Bindings',
		description: 'Expose typed JavaScript functions as CLI tools while credentials remain on the host.',
		docsHref: '/docs/bindings',
	},
	{
		icon: ShieldCheck,
		title: 'Human in the loop',
		description: 'Pause permission requests for review, then approve or reject them in your own UI.',
		docsHref: '/docs/approvals',
	},
	{
		icon: Package,
		title: 'Sandbox mounting',
		description: 'Mount a full Linux sandbox only when native binaries or heavy compilation need one.',
		docsHref: '/docs/sandboxes',
	},
	{
		icon: AppWindow,
		title: 'Browsers',
		description: 'Connect agents to serverless browser providers like Browserbase without running a browser inside the VM.',
		docsHref: '/docs/browser',
	},
	{
		icon: ChartNoAxesCombined,
		title: 'Observability',
		description: 'Surface structured logs, resource snapshots, and warnings before bounded limits are reached.',
		docsHref: '/docs/architecture/limits-and-observability',
	},
	{
		icon: Globe,
		title: 'Preview URLs',
		description: 'Expose services running inside a VM through signed, time-limited URLs.',
		docsHref: '/docs/networking#preview-urls',
	},
	{
		icon: CalendarClock,
		title: 'Cron jobs',
		description: 'Schedule commands or agent sessions while agents sleep between runs.',
		docsHref: '/docs/cron',
	},
	{
		icon: Gauge,
		title: 'Resource Limits',
		description: 'Set per-VM caps for processes, memory, files, sockets, and execution time.',
		docsHref: '/docs/resource-limits',
	},
	{
		icon: Blocks,
		title: 'Client-server & React SDKs',
		description: 'Connect backends and live agent interfaces with typed clients and first-party React hooks.',
		docsHref: '/docs/quickstart',
	},
	{
		icon: Layers,
		title: 'Built on standards',
		description: 'ACP and A2A at the agent boundary. WebAssembly and POSIX underneath.',
		docsHref: '/docs/architecture',
	},
];

const SecondaryFeaturesSection = () => (
	<section className='border-t border-ink/10 bg-paper px-6 py-16 md:py-24'>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<div className='mx-auto max-w-4xl text-center'>
					<h2 className='text-balance text-3xl font-medium leading-[1.08] tracking-[-0.025em] text-ink md:text-5xl'>Built for flexible, production-grade agents.</h2>
					<p className='mx-auto mt-5 max-w-3xl text-balance text-base leading-relaxed text-ink-soft md:text-lg'>Persist agent state, expose backend functions, review tool calls, route models, and attach a full sandbox only when a workload needs one.</p>
				</div>
			</Reveal>

			<Reveal>
				<div className='mt-14 grid gap-x-10 gap-y-12 sm:grid-cols-2 md:mt-20 lg:grid-cols-3 lg:gap-x-14 lg:gap-y-16'>
					{secondaryFeatures.map((feature) => {
						const Icon = feature.icon;
						return (
							<a key={feature.title} href={feature.docsHref} className='group -m-4 block cursor-pointer rounded-xl p-4 transition-[background-color,box-shadow] duration-200 hover:bg-white/60 hover:shadow-[inset_0_0_0_1px_rgba(20,20,22,0.08),0_8px_24px_-20px_rgba(20,20,22,0.35)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pine/50'>
								<span className='flex items-center gap-3'>
									<Icon className='h-5 w-5 text-ink-soft transition-colors group-hover:text-ink' />
									<span className='text-lg font-medium tracking-[-0.015em] text-ink'>{feature.title}</span>
									<ArrowRight className='ml-auto h-4 w-4 -translate-x-1 text-ink-faint opacity-0 transition-[opacity,transform,color] duration-200 group-hover:translate-x-0 group-hover:text-ink-soft group-hover:opacity-100 group-focus-visible:translate-x-0 group-focus-visible:opacity-100' />
								</span>
								<span className='mt-4 block text-base leading-relaxed text-ink-soft transition-colors group-hover:text-ink'>{feature.description}</span>
							</a>
						);
					})}
				</div>
			</Reveal>
		</div>
	</section>
);

// --- Argument (why a library, not a sandbox service) ---
// The topology makes the architectural contrast first; the benchmark block
// below provides the measured proof for startup, memory, and cost.

// Modal wrapper for the cold-start timeline, opened from the benchmarks
// header. Mounting the timeline on open replays its sequence each time.
const ColdStartModal = ({ open, onClose }: { open: boolean; onClose: () => void }) => {
	useEffect(() => {
		if (!open) return;
		const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
		document.addEventListener('keydown', onKey);
		const prevOverflow = document.body.style.overflow;
		document.body.style.overflow = 'hidden';
		return () => {
			document.removeEventListener('keydown', onKey);
			document.body.style.overflow = prevOverflow;
		};
	}, [open, onClose]);

	return (
		<AnimatePresence>
			{open && (
				<motion.div
					initial={{ opacity: 0 }}
					animate={{ opacity: 1 }}
					exit={{ opacity: 0 }}
					transition={{ duration: 0.2 }}
					className='fixed inset-0 z-[60] flex items-center justify-center bg-ink/50 p-4 md:p-8'
					onClick={onClose}
				>
					<motion.div
						role='dialog'
						aria-modal='true'
						aria-label='Cold-start timeline'
						initial={{ opacity: 0, y: 14, scale: 0.98 }}
						animate={{ opacity: 1, y: 0, scale: 1 }}
						exit={{ opacity: 0, y: 10, scale: 0.98 }}
						transition={{ duration: 0.25, ease: [0.22, 1, 0.36, 1] }}
						onClick={(e) => e.stopPropagation()}
						className='relative max-h-full w-full max-w-3xl overflow-y-auto rounded-2xl bg-paper p-6 shadow-[0_24px_70px_-20px_rgba(20,20,22,0.45)] ring-1 ring-ink/10 md:p-9'
					>
						<ColdStartTimeline onClose={onClose} />
					</motion.div>
				</motion.div>
			)}
		</AnimatePresence>
	);
};

const ComparisonSection = () => (
	<section className='border-t border-ink/10 bg-paper-mid px-6 py-16 md:py-32'>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<SectionHeading
					title={
						<>
							An isolated OS for every agent.
							<br />
							Much lighter than a full sandbox VM.
						</>
					}
					subtitle='Per-agent isolation without duplicating a complete VM stack for every agent.'
					className='mb-10 max-w-3xl md:mb-12'
				/>
			</Reveal>

			{/* Complete VM stacks versus compact isolated OS instances. */}
			<Reveal>
				<div id='versus' className={`scroll-mt-24 overflow-hidden p-6 md:p-8 ${CARD_SURFACE}`}>
					<div className='grid gap-6 md:grid-cols-2'>
						<div>
							<p className='mb-2 text-sm text-ink-soft'>Full sandbox per agent</p>
							<SandboxTopologyCell />
						</div>
						<div>
							<p className='mb-2 text-sm text-ink-soft'>Isolated OS per agent</p>
							<AgentOsTopologyCell />
						</div>
					</div>
				</div>
			</Reveal>
		</div>
	</section>
);

const BenchmarksSection = () => {
	const [showColdStart, setShowColdStart] = useState(false);

	return (
		<>
			<section className='border-t border-ink/10 bg-paper-mid px-6 py-16 md:py-32'>
				<div className='mx-auto max-w-7xl'>
					<Reveal>
						<div className='mb-8 flex items-baseline justify-between gap-4'>
							<h2 className='text-2xl font-medium tracking-[-0.015em] text-ink md:text-3xl'>
								What staying in-process saves.
							</h2>
							<a
								href='/docs/performance'
								className='inline-flex shrink-0 items-center gap-1 text-sm text-accent-deep underline underline-offset-2 transition-colors hover:text-accent'
							>
								Benchmark document
								<ExternalLink className='h-3 w-3' />
							</a>
						</div>
					</Reveal>
					<BenchmarkSection onShowColdStart={() => setShowColdStart(true)} />
				</div>
			</section>

			<ColdStartModal open={showColdStart} onClose={() => setShowColdStart(false)} />
		</>
	);
};

// --- Deployment ---
// Mirrors the "Start local. Scale to millions." hosting section from rivet.dev:
// a three-card local -> managed -> self-host story, with deploy targets in the
// self-host card.
// Content is grounded in /docs/deployment (agentOS runs as Rivet Actors).

const DEPLOY_CARD_CLASS =
	'relative flex h-full flex-col border border-ink/10 bg-white/55 p-6 md:p-8';
const DEPLOY_CARD_TITLE_CLASS = 'text-base font-medium tracking-tight text-ink';
const DEPLOY_BUTTON_BASE =
	'inline-flex h-10 w-full items-center justify-center gap-2 whitespace-nowrap rounded-md px-4 text-sm font-medium transition-colors';
const DEPLOY_GHOST_BUTTON_CLASS = `${DEPLOY_BUTTON_BASE} border border-ink/15 text-ink-soft hover:border-ink/40 hover:text-ink`;
const DEPLOY_PRIMARY_BUTTON_CLASS = `${DEPLOY_BUTTON_BASE} selection-dark bg-ink text-cream hover:bg-ink/85`;

const DeploymentSection = () => (
	<section className='border-t border-ink/10 bg-paper-mid py-16 md:py-32'>
		<div className='mx-auto max-w-7xl px-6'>
			<div className='mb-12'>
				<motion.h2
					initial={{ opacity: 0, y: 20 }}
					whileInView={{ opacity: 1, y: 0 }}
					viewport={{ once: true }}
					transition={{ duration: 0.5 }}
					className='mb-2 text-3xl font-medium tracking-[-0.015em] text-ink md:text-5xl'
				>
					Ships wherever your backend ships.
				</motion.h2>
				<motion.p
					initial={{ opacity: 0, y: 20 }}
					whileInView={{ opacity: 1, y: 0 }}
					viewport={{ once: true }}
					transition={{ duration: 0.5, delay: 0.1 }}
					className='max-w-xl text-base leading-relaxed text-ink-soft'
				>
					Each VM is a Rivet Actor with durable state. Your existing deployment works.
				</motion.p>
			</div>

			<motion.div
				initial={{ opacity: 0, y: 20 }}
				whileInView={{ opacity: 1, y: 0 }}
				viewport={{ once: true }}
				transition={{ duration: 0.5 }}
				className='grid grid-cols-1 gap-6 md:grid-cols-3 md:items-stretch'
			>
				{/* Card 1: Just a Library */}
				<div className={DEPLOY_CARD_CLASS}>
					<div className='mb-3 flex h-6 items-center gap-2.5'>
						<Package className='h-4 w-4 text-olive' />
						<h3 className={DEPLOY_CARD_TITLE_CLASS}>Library</h3>
					</div>
					<p className='text-sm leading-relaxed text-ink-soft'>
						npm install and run in your process. No servers.
					</p>
					<div className='flex-1' />
					<a href='/docs/quickstart' className={`mt-6 ${DEPLOY_GHOST_BUTTON_CLASS}`}>
						Get Started
					</a>
				</div>

				{/* Card 2: Rivet Cloud (primary) */}
				<div className={`${DEPLOY_CARD_CLASS} border-ink/20`}>
					<div className='mb-3 flex h-6 items-center gap-2.5'>
						<img className='h-4 w-4' src='/rivet-icon.svg' alt='Rivet' />
						<h3 className={DEPLOY_CARD_TITLE_CLASS}>Rivet Cloud</h3>
					</div>
					<p className='text-sm leading-relaxed text-ink-soft'>
						Managed agentOS on Rivet&apos;s edge network with BYOC support. Scales to millions of agents.
					</p>
					<div className='flex-1' />
					<a
						href='https://dashboard.rivet.dev'
						target='_blank'
						rel='noopener noreferrer'
						className={`mt-6 ${DEPLOY_PRIMARY_BUTTON_CLASS}`}
					>
						Sign Up
					</a>
				</div>

				{/* Card 3: Self-Host */}
				<div className={DEPLOY_CARD_CLASS}>
					<div className='mb-3 flex h-6 items-center gap-2.5'>
						<Server className='h-4 w-4 text-olive' />
						<h3 className={DEPLOY_CARD_TITLE_CLASS}>Self-Host</h3>
					</div>
					<p className='text-sm leading-relaxed text-ink-soft'>
						Run the open-source platform on Kubernetes, VMs, or bare metal.
					</p>
					<div className='mt-5 flex flex-wrap items-center gap-2'>
						{DEPLOY_TARGETS.filter(({ slug }) => slug !== 'rivet-compute').map(({ title, href, image }) => (
							<a
								key={title}
								href={href}
								target='_blank'
								rel='noopener noreferrer'
								aria-label={`Deploy to ${title}`}
								className='group relative flex h-8 w-8 items-center justify-center opacity-55 grayscale transition-[transform,filter,opacity] duration-200 hover:-translate-y-0.5 hover:opacity-100 hover:grayscale-0 focus-visible:opacity-100 focus-visible:grayscale-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pine/50 motion-reduce:transform-none motion-reduce:transition-none'
							>
								<img src={image} alt='' aria-hidden='true' className='h-5 w-5 object-contain' />
								<span className='pointer-events-none absolute bottom-full left-1/2 z-10 mb-2 -translate-x-1/2 whitespace-nowrap rounded-md bg-ink px-2 py-1 text-[11px] font-medium text-cream opacity-0 shadow-lg transition-opacity duration-150 group-hover:opacity-100 group-focus-visible:opacity-100'>
									{title}
								</span>
							</a>
						))}
					</div>
					<div className='flex-1' />
					<a href='/docs/deployment' className={`mt-6 ${DEPLOY_GHOST_BUTTON_CLASS}`}>
						Self-Hosting Docs
					</a>
				</div>
			</motion.div>
		</div>
	</section>
);

// --- Closing band ---
// The page opens with the argument (an OS, not a sandbox) and closes with the
// action. Repeats the hero CTAs so the reader never scrolls back up to act.
const ClosingCta = () => (
	<section className='border-t border-ink/10 bg-paper px-6 py-24 md:py-32'>
		<div className='mx-auto max-w-7xl'>
			<Reveal>
				<InkPanel>
					<div className='flex flex-col items-center px-6 py-16 text-center md:py-24'>
						<h2 className='mb-3 max-w-2xl text-balance text-3xl font-medium tracking-[-0.015em] text-cream md:text-5xl'>
							Turn your backend into the agent platform.
						</h2>
						<p className='mb-8 text-base leading-relaxed text-cream/70'>
							Open source under Apache 2.0. One npm install away.
						</p>
						<div className='flex flex-col flex-wrap items-center gap-x-4 gap-y-3 sm:flex-row sm:justify-center'>
							<SetupWithAgent />
							<a
								href='/docs'
								className='inline-flex w-full items-center justify-center gap-2 whitespace-nowrap rounded-md border border-cream/25 px-4 py-2 text-sm font-medium text-cream transition-colors hover:border-cream/50 hover:bg-cream/[0.04] sm:w-auto'
							>
								Read the Docs
								<ArrowRight className='h-4 w-4' />
							</a>
							<a
								href='https://github.com/rivet-dev/agentos'
								target='_blank'
								rel='noopener noreferrer'
								className='inline-flex w-full items-center justify-center gap-2 whitespace-nowrap rounded-md border border-cream/25 px-4 py-2 text-sm font-medium text-cream transition-colors hover:border-cream/50 hover:bg-cream/[0.04] sm:w-auto'
							>
								View on GitHub
								<ExternalLink className='h-4 w-4' />
							</a>
						</div>
					</div>
				</InkPanel>
			</Reveal>
		</div>
	</section>
);

// --- Main Page ---
// Section surfaces intentionally alternate paper / paper-mid in the render
// order below. If sections move, update their background class to preserve the
// light-tinted-light rhythm rather than following each component in isolation.
export default function AgentOSPage({ heroTabs, filesystemHighlightedCode, appsHighlightedCode }: AgentOSPageProps) {
	return (
		<div className='paper-grain min-h-screen font-sans text-ink-soft' style={{ overflowX: 'clip', zoom: PAGE_ZOOM }}>
			<main>
				<Hero />
				<AgentCompatibilitySection />
				<RuntimeArgumentSection />
				<ExecutionSection />
				<FilesystemSection highlightedCode={filesystemHighlightedCode} />
				<OrchestrationSection heroTabs={heroTabs} />
				<AppsSection highlightedCode={appsHighlightedCode} />
				<RegistrySection />
				<SecondaryFeaturesSection />
				<DeploymentSection />
				<ClosingCta />
			</main>
		</div>
	);
}
