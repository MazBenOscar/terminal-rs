<script lang="ts">
	import { ticks, type History } from '#lib/chart.js';
	import { clamp, easeOut, easeOutBack, lerp, span } from '#lib/ease.js';

	type Marker = { i: number; side: 'buy' | 'sell'; p: number };

	let {
		history,
		width,
		height,
		reveal = 1,
		lines = 1,
		cursor = null,
		markers = []
	}: {
		history: History;
		width: number;
		height: number;
		/** 0..1: candles sweep in from the left. */
		reveal?: number;
		/** 0..1: the moving averages trace in. */
		lines?: number;
		/** Fractional candle index for the crosshair. */
		cursor?: number | null;
		markers?: Marker[];
	} = $props();

	const AXIS = 46;
	const TOP = 8;

	let candles = $derived(history.candles);
	let n = $derived(candles.length);
	let plotW = $derived(width - AXIS);
	let bottom = $derived(height - 4);
	let step = $derived(plotW / n);
	let body = $derived(Math.max(1, step * 0.62));
	let range = $derived.by(() => {
		const values = history.averages.flat().filter((v) => v !== null);
		const lo = Math.min(...candles.map((k) => k.l), ...values);
		const hi = Math.max(...candles.map((k) => k.h), ...values);
		const pad = (hi - lo) * 0.08;
		return { lo: lo - pad, hi: hi + pad };
	});
	let priceH = $derived((bottom - TOP) * 0.84);
	let volH = $derived((bottom - TOP) * 0.18);

	const x = (i: number) => step * (i + 0.5);
	const y = (p: number) => TOP + ((range.hi - p) / (range.hi - range.lo)) * priceH;

	let levels = $derived(ticks(range.lo, range.hi, 4));
	let averages = $derived(
		history.averages.map((avg) =>
			avg.flatMap((v, i) => (v === null ? [] : [`${x(i).toFixed(1)},${y(v).toFixed(1)}`])).join(' ')
		)
	);
	let last = $derived(candles[n - 1]);

	const grow = (i: number) => easeOut(clamp(reveal * (n + 10) - i, 0, 10) / 10);

	let cross = $derived.by(() => {
		if (cursor === null) return null;
		const k = candles[clamp(Math.round(cursor), 0, n - 1)];
		return { x: x(cursor), y: y(k.c), price: k.c };
	});
</script>

<svg class="chart" viewBox="0 0 {width} {height}" {width} {height} aria-hidden="true">
	{#each levels as level (level)}
		<line class="grid" x1="0" x2={plotW} y1={y(level)} y2={y(level)} />
		<text class="axis" x={plotW + 6} y={y(level) + 3}>{level.toFixed(0)}</text>
	{/each}

	{#each candles as k, i (k)}
		{@const f = grow(i)}
		{#if f > 0}
			{@const close = lerp(k.o, k.c, f)}
			<rect
				class="vol"
				x={x(i) - body / 2}
				y={bottom - k.v * volH * f}
				width={body}
				height={k.v * volH * f}
			/>
			<g class={k.c >= k.o ? 'up' : 'down'} opacity={clamp(f * 3)}>
				<line x1={x(i)} x2={x(i)} y1={y(lerp(k.o, k.h, f))} y2={y(lerp(k.o, k.l, f))} />
				<rect
					x={x(i) - body / 2}
					y={Math.min(y(k.o), y(close))}
					width={body}
					height={Math.max(0.8, Math.abs(y(close) - y(k.o)))}
				/>
			</g>
		{/if}
	{/each}

	{#each averages as points, i (i)}
		<polyline
			class="sma sma{i + 1}"
			{points}
			pathLength="1"
			stroke-dasharray="1"
			stroke-dashoffset={1 - lines}
			opacity={Math.min(1, reveal * 2)}
		/>
	{/each}

	{#each markers as m (m.i)}
		{@const k = candles[m.i]}
		{@const s = easeOutBack(m.p)}
		{#if m.p > 0}
			<circle
				class={m.side}
				cx={x(m.i)}
				cy={m.side === 'buy' ? y(k.l) + 9 : y(k.h) - 9}
				r={Math.max(0, 4.5 * s)}
			/>
		{/if}
	{/each}

	<g opacity={span(reveal, 0.85, 1)}>
		<line class="last" x1="0" x2={plotW} y1={y(last.c)} y2={y(last.c)} />
		<rect class="tag" x={plotW + 2} y={y(last.c) - 6.5} width={AXIS - 4} height="13" rx="2" />
		<text class="tag-text" x={plotW + 6} y={y(last.c) + 3}>{last.c.toFixed(2)}</text>
	</g>

	{#if cross}
		<line class="cross" x1={cross.x} x2={cross.x} y1={TOP} y2={bottom} />
		<line class="cross" x1="0" x2={plotW} y1={cross.y} y2={cross.y} />
		<rect class="cross-tag" x={plotW + 2} y={cross.y - 6.5} width={AXIS - 4} height="13" rx="2" />
		<text class="cross-text" x={plotW + 6} y={cross.y + 3}>{cross.price.toFixed(2)}</text>
	{/if}
</svg>

<style>
	.chart {
		display: block;
		overflow: visible;
		font-family: var(--mono);
		font-size: 8.5px;
	}

	.grid {
		stroke: var(--grid);
		stroke-width: 1;
	}

	.axis {
		fill: var(--muted);
	}

	.vol {
		fill: var(--muted);
		opacity: 0.18;
	}

	.up {
		stroke: var(--up);
		fill: var(--up);
	}

	.down {
		stroke: var(--down);
		fill: var(--down);
	}

	.up line,
	.down line {
		stroke-width: 1;
	}

	.sma {
		fill: none;
		stroke-width: 1.3;
		stroke-linejoin: round;
	}

	.sma1 {
		stroke: var(--sma1);
	}

	.sma2 {
		stroke: var(--sma2);
	}

	.buy {
		fill: var(--buy);
		stroke: var(--panel);
		stroke-width: 1.5;
	}

	.sell {
		fill: var(--sell);
		stroke: var(--panel);
		stroke-width: 1.5;
	}

	.last {
		stroke: var(--down);
		stroke-dasharray: 2 3;
		stroke-width: 0.8;
		opacity: 0.6;
	}

	.tag {
		fill: var(--down);
	}

	.tag-text {
		fill: #fff;
	}

	.cross {
		stroke: var(--text);
		stroke-dasharray: 3 3;
		stroke-width: 0.8;
		opacity: 0.55;
	}

	.cross-tag {
		fill: var(--text);
	}

	.cross-text {
		fill: var(--panel);
	}
</style>
