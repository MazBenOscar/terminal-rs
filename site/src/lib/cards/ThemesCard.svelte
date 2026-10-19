<script lang="ts">
	import { series } from '#lib/chart.js';
	import { easeOut, span } from '#lib/ease.js';
	import { loop } from '#lib/motion.svelte.js';
	import Fit from './Fit.svelte';

	const THEMES = [
		{ name: 'Terminal Dark', bg: '#0C0C0D', bar: '#1C1C1E', accent: '#FFB000', muted: '#8E8E93', text: '#ECECEE', up: '#30D158' },
		{ name: 'Terminal Light', bg: '#FFFFFF', bar: '#ECECEE', accent: '#B26800', muted: '#6E6E73', text: '#1D1D1F', up: '#1C9640' },
		{ name: 'Amber', bg: '#000000', bar: '#141414', accent: '#FFD666', muted: '#B07A00', text: '#FFB000', up: '#00E676' },
		{ name: 'High Contrast', bg: '#000000', bar: '#1A1A1A', accent: '#FFEB00', muted: '#BDBDBD', text: '#FFFFFF', up: '#00FF66' },
		{ name: 'Solarized Dark', bg: '#002B36', bar: '#073642', accent: '#B58900', muted: '#839496', text: '#D0D8D8', up: '#859900' },
		{ name: 'Solarized Light', bg: '#FDF6E3', bar: '#EEE8D5', accent: '#9A6F00', muted: '#657B83', text: '#073642', up: '#859900' },
		{ name: 'Nord', bg: '#242933', bar: '#2E3440', accent: '#EBCB8B', muted: '#81A1C1', text: '#ECEFF4', up: '#A3BE8C' },
		{ name: 'Monokai', bg: '#1E1F1C', bar: '#272822', accent: '#E6DB74', muted: '#75715E', text: '#F8F8F2', up: '#A6E22E' }
	];
	const STEP = 1100;

	const closes = series(17, 14, 330, 343.46, 0.012).map((k) => k.c);
	const lo = Math.min(...closes);
	const hi = Math.max(...closes);
	const points = closes.map((c, i) => `${((i / 13) * 92).toFixed(1)},${(27 - ((c - lo) / (hi - lo)) * 23).toFixed(1)}`).join(' ');

	const clock = loop(STEP * THEMES.length, 900);
	let t = $derived(clock.t);

	let active = $derived(Math.floor(t / STEP));
	let draw = $derived(easeOut(span(t - active * STEP, 0, 700)));
</script>

<div class="card-visual" {@attach clock.observe}>
	<Fit width={506} height={304}>
		<div class="grid">
			{#each THEMES as theme, i (theme.name)}
				<div
					class={['tile', { active: i === active }]}
					title={theme.name}
					style:--bg={theme.bg}
					style:--bar={theme.bar}
					style:--accent={theme.accent}
					style:--muted={theme.muted}
					style:--text={theme.text}
					style:--up={theme.up}
				>
					<div class="bar"><span class="code">GP</span> AAPL</div>
					<div class="quote">
						<span class="price">343.46</span>
						<span class="chg">+0.43%</span>
					</div>
					<svg viewBox="0 0 92 31" aria-hidden="true">
						<polyline
							{points}
							pathLength="1"
							stroke-dasharray="1"
							stroke-dashoffset={i === active ? 1 - draw : 0}
						/>
						<line class="base" x1="0" x2="92" y1="30.5" y2="30.5" />
					</svg>
					<div class="swatches">
						<span class="accent"></span>
						<span></span>
						<span></span>
					</div>
				</div>
			{/each}
		</div>
	</Fit>
</div>

<style>
	.card-visual {
		background: radial-gradient(ellipse 70% 60% at 50% 0%, rgba(110, 150, 255, 0.08), transparent);
	}

	.grid {
		position: absolute;
		left: 25px;
		top: 32px;
		display: grid;
		grid-template-columns: repeat(4, 108px);
		gap: 10px 8px;
	}

	.tile {
		height: 116px;
		overflow: hidden;
		border-radius: 8px;
		background: var(--bg);
		box-shadow: 0 0 0 1px var(--line);
		color: var(--text);
		font-family: var(--mono);
		transition:
			transform 0.35s cubic-bezier(0.3, 1.4, 0.5, 1),
			box-shadow 0.35s;
	}

	.tile.active {
		box-shadow:
			0 0 0 1.5px color-mix(in srgb, var(--fg) 90%, transparent),
			0 0 0 5px color-mix(in srgb, var(--fg) 10%, transparent),
			0 14px 30px -6px rgba(0, 0, 0, 0.6);
		transform: translateY(-3px) scale(1.04);
	}

	.bar {
		height: 20px;
		padding: 0 8px;
		background: var(--bar);
		color: var(--muted);
		font-size: 8px;
		line-height: 20px;
	}

	.code {
		color: var(--accent);
		font-weight: 700;
	}

	.quote {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		padding: 10px 8px 4px;
	}

	.price {
		font-size: 12px;
	}

	.chg {
		color: var(--up);
		font-size: 8px;
	}

	svg {
		display: block;
		width: 92px;
		height: 31px;
		margin: 0 8px;
		overflow: visible;
	}

	polyline {
		fill: none;
		stroke: var(--up);
		stroke-width: 1.2;
		stroke-linejoin: round;
	}

	.base {
		stroke: color-mix(in srgb, var(--muted) 35%, transparent);
		stroke-width: 1;
	}

	.swatches {
		display: flex;
		gap: 4px;
		padding: 9px 8px 0;
	}

	.swatches span {
		width: 14px;
		height: 4px;
		border-radius: 2px;
		background: var(--muted);
	}

	.swatches .accent {
		width: 22px;
		background: var(--accent);
	}
</style>
