<script lang="ts">
	import { suggestions } from '#lib/demo/script.js';
	import { envelope, span } from '#lib/ease.js';
	import { loop } from '#lib/motion.svelte.js';
	import Fit from './Fit.svelte';

	const QUERY = 'NVDA';
	const KEYS = [600, 760, 920, 1080];
	const MOVES = [2500, 3000];
	const ENTER = 3700;

	const RECENT = [
		{ code: 'WEI', title: 'World markets' },
		{ code: 'CONG', title: 'Congress trades' },
		{ code: 'GC', title: 'Treasury yield curve' },
		{ code: 'F7', title: 'Portfolio' }
	];

	const clock = loop(7200, 2000);
	let t = $derived(clock.t);

	let shown = $derived(envelope(t, 0, 250, 5800, 6400));
	let query = $derived(QUERY.slice(0, KEYS.filter((k) => t >= k).length));
	let rows = $derived(query ? suggestions(query) : RECENT);
	let selected = $derived(query ? MOVES.filter((m) => t >= m).length : -1);
	let pressed = $derived(t >= ENTER && t < ENTER + 160);
	let opened = $derived(span(t, ENTER, ENTER + 400));
	let caret = $derived(Math.floor(t / 530) % 2 === 0 || (t > KEYS[0] && t < KEYS[3] + 300));
</script>

<div class="card-visual" {@attach clock.observe}>
	<Fit width={506} height={304}>
		<div class="backdrop"></div>
		<div class="palette app" style:opacity={shown} style:transform="translateY({(1 - shown) * 8}px) scale({1 - opened * 0.02})">
			<div class="input">
				<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.6" /><path d="M10.4 10.4 14 14" /></svg>
				<span class="text">
					{#if query}
						{query}<span class="caret" style:opacity={caret ? 1 : 0}></span>
					{:else}
						<span class="caret" style:opacity={caret ? 1 : 0}></span><span class="placeholder">Type a ticker or command</span>
					{/if}
				</span>
			</div>
			{#each rows as row, i (row.code)}
				<div class={['row', { selected: i === selected, pressed: pressed && i === selected }]}>
					<span class="code">{row.code}</span>
					<span class="title">{row.title}</span>
					{#if 'kind' in row && row.kind}
						<span class="kind">{row.kind}</span>
					{/if}
					{#if i === selected}
						<span class="enter">↩</span>
					{/if}
				</div>
			{/each}
		</div>
	</Fit>
</div>

<style>
	.backdrop {
		position: absolute;
		inset: -12px;
		background: 20% 8% / cover url('/demo-dark.webp');
		filter: blur(3px) brightness(0.42) saturate(1.2);
	}

	:global(:root[data-theme='light']) .backdrop {
		background-image: url('/demo-light.webp');
		filter: blur(3px) brightness(0.96) saturate(1.1);
	}

	.palette {
		position: absolute;
		left: 52px;
		top: 38px;
		width: 402px;
		padding: 6px;
		border-radius: 16px;
		background: rgba(28, 28, 32, 0.6);
		backdrop-filter: blur(18px) saturate(180%);
		box-shadow:
			inset 0 0.5px 0 rgba(255, 255, 255, 0.3),
			inset 0 0 0 0.5px rgba(255, 255, 255, 0.14),
			0 24px 60px -12px rgba(0, 0, 0, 0.7);
		transform-origin: 50% 30%;
	}

	:global(:root[data-theme='light']) .palette {
		background: rgba(255, 255, 255, 0.72);
		box-shadow:
			inset 0 0.5px 0 #fff,
			inset 0 0 0 0.5px rgba(0, 0, 0, 0.08),
			0 24px 60px -12px rgba(0, 0, 0, 0.28);
	}

	.input {
		display: flex;
		align-items: center;
		gap: 10px;
		height: 44px;
		padding: 0 10px 4px;
		color: var(--text);
		font: 500 18px var(--sans);
	}

	.input svg {
		width: 16px;
		height: 16px;
		fill: none;
		stroke: var(--muted);
		stroke-width: 1.5;
		stroke-linecap: round;
	}

	.placeholder {
		color: color-mix(in srgb, var(--muted) 80%, transparent);
	}

	.text {
		display: flex;
		align-items: center;
	}

	.caret {
		width: 1.5px;
		height: 20px;
		margin: 0 1px;
		background: var(--text);
	}

	.row {
		display: flex;
		align-items: center;
		gap: 12px;
		height: 42px;
		padding: 0 16px;
		border-radius: 10px;
		color: var(--dim);
		font: 14px var(--sans);
		transition: background-color 0.18s;
	}

	.row.selected {
		background: var(--control);
		box-shadow: inset 0 0.5px 0 rgba(255, 255, 255, 0.18);
		color: var(--text);
	}

	.row.pressed {
		background: color-mix(in srgb, var(--text) 18%, transparent);
	}

	.code {
		width: 38px;
		color: var(--muted);
		font: 600 11px var(--mono);
	}

	.selected .code {
		color: var(--text);
	}

	.title {
		flex: 1;
	}

	.kind {
		color: var(--muted);
		font-size: 12px;
	}

	.enter {
		color: var(--muted);
		font-size: 13px;
	}
</style>
