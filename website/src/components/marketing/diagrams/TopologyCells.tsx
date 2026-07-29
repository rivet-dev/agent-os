'use client';

import { useId } from 'react';
import { motion, useReducedMotion } from 'framer-motion';
import { EASE, VIEWPORT } from '../motion';

// ---------------------------------------------------------------------------
// Equal-footprint density panels following the isolate-vs-VM model: every
// traditional sandbox reserves a large Linux VM, while agentOS packs many
// compact WebAssembly instances into the same footprint.
// ---------------------------------------------------------------------------

const SANDBOXES = Array.from({ length: 6 }, (_, i) => i);
const AGENT_OS_ACTORS = Array.from({ length: 40 }, (_, i) => i);
const LINUX_MARK = '/images/registry/linux.svg';
const WEBASSEMBLY_MARK = '/images/agent-os/webassembly-logo.svg';

const PerlinBackground = ({ index, reduced }: { index: number; reduced: boolean | null }) => {
	const filterId = `agentos-noise-${useId().replace(/:/g, '')}`;

	return (
		<motion.svg
			aria-hidden='true'
			focusable='false'
			viewBox='0 0 100 100'
			preserveAspectRatio='none'
			initial={{ x: '-5%', y: '-4%', opacity: 0.4 }}
			animate={reduced ? { x: '-5%', y: '-4%', opacity: 0.4 } : { x: ['-5%', '3%', '-5%'], y: ['-4%', '4%', '-4%'], opacity: [0.4, 0.68, 0.4] }}
			transition={reduced ? { duration: 0 } : { duration: 5 + (index % 5) * 0.3, repeat: Infinity, delay: (index % 8) * 0.06, ease: 'easeInOut' }}
			className='pointer-events-none absolute -inset-[18%] z-0 h-[136%] w-[136%]'
		>
			<defs>
				<filter id={filterId} x='-20%' y='-20%' width='140%' height='140%'>
					<feTurbulence type='fractalNoise' baseFrequency='0.045' numOctaves='3' seed={index + 7} result='noise' />
					<feColorMatrix
						in='noise'
						type='matrix'
						values='0.16 0 0 0 0.45  0 0.12 0 0 0.40  0 0 0.08 0 0.88  0.07 0.07 0.07 0 0.03'
					/>
				</filter>
			</defs>
			<rect width='100' height='100' fill='white' filter={`url(#${filterId})`} />
		</motion.svg>
	);
};

export const AgentOsTopologyCell = () => {
	const reduced = useReducedMotion();
	return (
		<div className='aspect-[3/2] rounded-xl bg-[#faf8f3] p-3 ring-1 ring-ink/10 shadow-[inset_0_1px_0_rgba(255,255,255,0.8)]'>
			<div className='relative h-full overflow-hidden rounded-md border border-pine/15 bg-pine/[0.065]' aria-label='Forty compact WebAssembly agentOS instances'>
				<div className='absolute inset-2 grid grid-cols-8 grid-rows-5 gap-1'>
					{AGENT_OS_ACTORS.map((actor, i) => (
					<motion.span
						key={actor}
						initial={reduced ? undefined : { opacity: 0, scale: 0.6 }}
						whileInView={reduced ? undefined : { opacity: 1, scale: 1 }}
						viewport={VIEWPORT}
						transition={{ duration: 0.28, delay: 0.1 + i * 0.035, ease: [...EASE] }}
						className='relative flex aspect-square min-w-0 items-center justify-center overflow-hidden rounded-md border border-pine/20 bg-white/80 shadow-[0_1px_2px_rgba(46,64,52,0.06)]'
						aria-label='Isolated WebAssembly agentOS instance'
					>
						<PerlinBackground index={i} reduced={reduced} />
						<img src={WEBASSEMBLY_MARK} alt='' aria-hidden='true' className='relative z-10 h-[30%] w-[30%] object-contain opacity-70' />
					</motion.span>
					))}
				</div>
			</div>
		</div>
	);
};

export const SandboxTopologyCell = () => {
	const reduced = useReducedMotion();
	return (
		<div className='aspect-[3/2] rounded-xl bg-ink/[0.03] p-3 ring-1 ring-ink/[0.08]'>
			<div className='grid h-full grid-cols-3 grid-rows-2 place-items-center gap-2'>
				{SANDBOXES.map((sandbox, i) => (
					<motion.div
						key={sandbox}
						initial={reduced ? undefined : { opacity: 0, scale: 0.92 }}
						whileInView={reduced ? undefined : { opacity: 1, scale: 1 }}
						viewport={VIEWPORT}
						transition={{ duration: 0.35, delay: 0.14 + i * 0.05, ease: [...EASE] }}
						className='flex h-full aspect-square min-h-0 min-w-0 items-center justify-center overflow-hidden rounded-md border border-pine/15 bg-pine/[0.065]'
						aria-label='Full Linux virtual machine'
					>
						<img src={LINUX_MARK} alt='' aria-hidden='true' className='h-[24%] w-[24%] object-contain opacity-55' />
					</motion.div>
				))}
			</div>
		</div>
	);
};
