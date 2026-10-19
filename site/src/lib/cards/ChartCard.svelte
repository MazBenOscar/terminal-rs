<script lang="ts">
	import Chart from '#lib/Chart.svelte';
	import { daily } from '#lib/chart.js';
	import { easeInOut, envelope, lerp, span } from '#lib/ease.js';
	import { loop } from '#lib/motion.svelte.js';
	import Fit from './Fit.svelte';

	const N = daily.candles.length;
	const clock = loop(9000, 5200);
	let t = $derived(clock.t);

	let shown = $derived(1 - span(t, 8500, 8950));
	let reveal = $derived(span(t, 300, 1200));
	let lines = $derived(span(t, 500, 1700));
	let cursor = $derived(
		t > 1900 && t < 7600 ? lerp(N * 0.08, N * 0.94, easeInOut(span(t, 2000, 7200))) : null
	);
	let cursorShown = $derived(envelope(t, 1900, 2200, 7300, 7600));
	let k = $derived(daily.candles[cursor === null ? N - 1 : Math.round(cursor)]);
</script>

<div class="card-visual" {@attach clock.observe}>
	<Fit width={506} height={395}>
		<div class="pane app" style:opacity={shown}>
			<div class="tabs">
				<span class="code">GP</span>
				<span class="title">AAPL</span>
				<span class="spacer"></span>
				{#each ['1m', '5m', '1H', '1D', '1W'] as label (label)}
					<span class={['interval', { on: label === '1D' }]}>{label}</span>
				{/each}
			</div>
			<div class="readout">
				<span>AAPL · 1D</span>
				<span>O{k.o.toFixed(2)}</span>
				<span>H{k.h.toFixed(2)}</span>
				<span>L{k.l.toFixed(2)}</span>
				<span class={k.c >= k.o ? 'up' : 'down'}>C{k.c.toFixed(2)}</span>
			</div>
			<div class="plot" style:--cross-opacity={cursorShown}>
				<Chart history={daily} width={458} height={262} {reveal} {lines} {cursor} />
			</div>
		</div>
	</Fit>
</div>

<style>
	.card-visual {
		background: radial-gradient(ellipse 70% 60% at 50% 0%, rgba(110, 150, 255, 0.09), transparent);
	}

	.pane {
		position: absolute;
		left: 16px;
		top: 44px;
		display: flex;
		flex-direction: column;
		width: 474px;
		height: 330px;
		overflow: hidden;
		border-radius: 10px;
		background: var(--panel);
		box-shadow:
			0 0 0 1px var(--line),
			0 20px 40px -12px var(--drop);
		font-family: var(--mono);
		color: var(--text);
	}

	.tabs {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 28px;
		padding: 0 10px;
		background: var(--header);
		font-size: 10px;
	}

	.code {
		color: var(--hl);
		font-weight: 700;
	}

	.title {
		font: 12px var(--sans);
	}

	.spacer {
		flex: 1;
	}

	.interval {
		padding: 1px 5px;
		border-radius: 4px;
		color: var(--muted);
	}

	.interval.on {
		background: var(--control);
		color: var(--text);
	}

	.readout {
		display: flex;
		gap: 9px;
		padding: 8px 10px 0;
		color: var(--muted);
		font-size: 10px;
		font-variant-numeric: tabular-nums;
	}

	.up {
		color: var(--up);
	}

	.down {
		color: var(--down);
	}

	.plot {
		padding: 6px 8px 0 10px;
	}

	.plot :global(.cross) {
		opacity: calc(var(--cross-opacity) * 0.55);
	}

	.plot :global(.cross-tag),
	.plot :global(.cross-text) {
		opacity: var(--cross-opacity);
	}
</style>
