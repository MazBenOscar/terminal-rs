<script lang="ts">
	import Chart from '#lib/Chart.svelte';
	import Logo from '#lib/Logo.svelte';
	import { daily, intraday } from '#lib/chart.js';
	import { clamp, easeInOut, easeOut, easeOutBack, envelope, lerp, span } from '#lib/ease.js';
	import { CAPTIONS, CHART_PANE, SIDE_PANE, T, WINDOW, suggestions } from './script.js';

	let { t }: { t: number } = $props();

	// Camera: zooms the window onto the chart pane and back out.
	const FOCUS = { x: WINDOW.x + CHART_PANE.x + CHART_PANE.w / 2, y: WINDOW.y + CHART_PANE.y + CHART_PANE.h / 2 };
	const AIM = { x: 600, y: 356 };
	const ZOOM = 1.62;
	let zoom = $derived(
		easeInOut(span(t, T.zoomIn[0], T.zoomIn[1])) - easeInOut(span(t, T.zoomOut[0], T.zoomOut[1]))
	);
	let camera = $derived.by(() => {
		const s = lerp(1, ZOOM, zoom);
		const x = lerp(FOCUS.x, AIM.x, zoom) - s * FOCUS.x;
		const y = lerp(FOCUS.y, AIM.y, zoom) - s * FOCUS.y;
		return `translate(${x}px, ${y}px) scale(${s})`;
	});

	// Command bar and palette.
	let bar = $derived(T.bar.find((b) => t >= b.in - 50 && t < b.out + 400) ?? null);
	let barShown = $derived(bar ? envelope(t, bar.in, bar.in + 320, bar.out, bar.out + 280) : 0);
	let query = $derived(bar ? bar.typing.text.slice(0, bar.typing.keys.filter((k) => t >= k).length) : '');
	let pressed = $derived(bar !== null && t >= bar.go && t < bar.go + 170);
	let rows = $derived(suggestions(query));
	let paletteShown = $derived(
		bar && query ? envelope(t, bar.typing.start, bar.typing.start + 180, bar.go + 60, bar.go + 240) : 0
	);
	let typing = $derived(bar !== null && bar.typing.keys.some((k) => t >= k && t < k + 450));
	let caret = $derived(typing || Math.floor(t / 530) % 2 === 0);

	// Chart pane: empty until GO, then daily candles, then five-minute candles.
	let loaded = $derived(span(t, T.load - 100, T.load + 100));
	let fine = $derived(t >= T.reflowIn[0]);
	let history = $derived(fine ? intraday : daily);
	let candles = $derived(history.candles);
	let reveal = $derived(
		t < T.reflowOut[0]
			? span(t, T.load, T.load + 550)
			: fine
				? span(t, T.reflowIn[0], T.reflowIn[1])
				: 1 - span(t, T.reflowOut[0], T.reflowOut[1])
	);
	let lines = $derived(
		fine ? span(t, T.reflowIn[0] + 200, T.reflowIn[1] + 500) : span(t, T.load + 150, T.load + 1100)
	);
	let cursor = $derived.by(() => {
		const range = T.cursor.find(([a, b]) => t >= a - 150 && t <= b + 250);
		if (!range) return null;
		const n = candles.length;
		return lerp(n * 0.12, n * 0.9, easeInOut(span(t, range[0], range[1])));
	});
	// Congress trades are dated by day, so their markers only show on the daily chart.
	const MARKERS = [
		{ i: 14, side: 'buy' },
		{ i: 33, side: 'sell' },
		{ i: 52, side: 'buy' }
	] as const;
	let markers = $derived(
		fine ? [] : MARKERS.map((m, i) => ({ ...m, p: span(t, T.markers[i], T.markers[i] + 380) }))
	);
	let readout = $derived(candles[cursor === null ? candles.length - 1 : clamp(Math.round(cursor), 0, candles.length - 1)]);

	const INTERVALS = ['1m', '5m', '15m', '1H', '1D', '1W'];
	let interval = $derived(lerp(4, 1, easeInOut(span(t, T.interval, T.interval + 260))));

	// Options chain, slid in over the Congress pane.
	const SPOT = 214.13;
	const STRIKES = Array.from({ length: 10 }, (_, i) => 195 + i * 5);
	const quote = (intrinsic: number, k: number) => {
		const mid = intrinsic + 6.2 * Math.exp(-(((k - SPOT) / 22) ** 2)) + 0.35;
		return { bid: (mid * 0.97).toFixed(2), ask: (mid * 1.03).toFixed(2) };
	};
	const CHAIN = STRIKES.map((k) => ({
		k,
		call: quote(Math.max(SPOT - k, 0), k),
		put: quote(Math.max(k - SPOT, 0), k),
		iv: (38 + Math.abs(k - SPOT) * 0.18).toFixed(1)
	}));
	let options = $derived(easeOut(span(t, T.options[0], T.options[1])));
	const row = (i: number) => easeOut(span(t, T.rows + i * T.rowStep, T.rows + i * T.rowStep + 220));

	let notice = $derived(span(t, T.notify, T.notify + 520));

	let caption = $derived(CAPTIONS.find((c) => t >= c.start - 200 && t < c.end + 900) ?? null);
	let captionShown = $derived(caption ? envelope(t, caption.start - 200, caption.start, caption.end + 600, caption.end + 900) : 0);
</script>

<div class="scene">
	<div class="wallpaper"></div>

	<div class="camera" style:transform={camera}>
		<div class="window">
			<div
				class={['pane chart-pane', { focused: t < T.options[0] }]}
				style:left="{CHART_PANE.x}px"
				style:top="{CHART_PANE.y}px"
				style:width="{CHART_PANE.w}px"
				style:height="{CHART_PANE.h}px"
			>
				<div class="tabs">
					<span class="code">GP</span>
					<span class="title">{loaded > 0.5 ? 'AAPL' : 'Chart'}</span>
					<span class="spacer"></span>
					<span class="intervals" style:opacity={loaded}>
						<span class="thumb" style:transform="translateX({interval * 22}px)"></span>
						{#each INTERVALS as label, i (label)}
							<span class={['interval', { on: Math.round(interval) === i }]}>{label}</span>
						{/each}
					</span>
				</div>
				<div class="readout" style:opacity={loaded}>
					<span>AAPL · {fine ? '5m' : '1D'}</span>
					<span>O{readout.o.toFixed(2)}</span>
					<span>H{readout.h.toFixed(2)}</span>
					<span>L{readout.l.toFixed(2)}</span>
					<span class={readout.c >= readout.o ? 'up' : 'down'}>C{readout.c.toFixed(2)}</span>
				</div>
				<div class="plot">
					<Chart {history} width={CHART_PANE.w - 10} height={CHART_PANE.h - 50} {reveal} {lines} {cursor} {markers} />
				</div>
				<div class="empty" style:opacity={1 - loaded}>Type a ticker and press GO</div>
			</div>

			{#if options > 0}
				<div
					class={['pane side-pane', { focused: options > 0.5 }]}
					style:left="{SIDE_PANE.x}px"
					style:top="{SIDE_PANE.y}px"
					style:width="{SIDE_PANE.w}px"
					style:height="{SIDE_PANE.h}px"
					style:opacity={options}
					style:transform="translateY({(1 - options) * 14}px)"
				>
					<div class="tabs">
						<span class="code">OMON</span>
						<span class="title">NVDA Options</span>
						<span class="spacer"></span>
						<span class="chip">Nov 21 ’26</span>
						<span class="chip">NVDA {SPOT}</span>
					</div>
					<div class="chain">
						<div class="chain-row head">
							<span>Bid</span><span>Ask</span><span>IV</span><span class="strike">Strike</span><span>Bid</span><span>Ask</span><span>IV</span>
						</div>
						{#each CHAIN as r, i (r.k)}
							<div
								class={['chain-row', { atm: r.k === 215 }]}
								style:opacity={row(i)}
								style:transform="translateY({(1 - row(i)) * 6}px)"
							>
								<span class={{ itm: r.k < SPOT }}>{r.call.bid}</span>
								<span class={{ itm: r.k < SPOT }}>{r.call.ask}</span>
								<span class={['iv', { itm: r.k < SPOT }]}>{r.iv}%</span>
								<span class="strike">{r.k}</span>
								<span class={{ itm: r.k > SPOT }}>{r.put.bid}</span>
								<span class={{ itm: r.k > SPOT }}>{r.put.ask}</span>
								<span class={['iv', { itm: r.k > SPOT }]}>{r.iv}%</span>
							</div>
						{/each}
					</div>
				</div>
			{/if}
		</div>
	</div>

	<div class="menubar">
		<svg class="apple" viewBox="0 0 17 21" aria-hidden="true">
			<path
				d="M11.2 4.3c.6-.8 1-1.8.9-2.8-.9 0-2 .6-2.6 1.4-.6.7-1.1 1.7-.9 2.7 1 .1 2-.5 2.6-1.3zM12.1 5.8c-1.4-.1-2.7.8-3.3.8-.7 0-1.7-.8-2.9-.8C4.4 5.9 3 6.8 2.2 8.1c-1.6 2.7-.4 6.8 1.1 9 .8 1.1 1.7 2.3 2.8 2.3 1.1 0 1.6-.7 2.9-.7 1.4 0 1.8.7 2.9.7 1.2 0 2-1.1 2.7-2.2.9-1.3 1.2-2.5 1.2-2.6 0 0-2.4-.9-2.4-3.6 0-2.2 1.8-3.3 1.9-3.4-1-1.5-2.6-1.7-3.2-1.7z"
			/>
		</svg>
		<b>Terminal</b>
		<span>File</span><span>Edit</span><span>View</span><span>Window</span><span>Help</span>
		<span class="spacer"></span>
		<span>Sun Oct 18&nbsp;&nbsp;9:55 AM</span>
	</div>

	{#if notice > 0}
		<div
			class="notice glass"
			style:opacity={span(t, T.notify, T.notify + 200)}
			style:transform="translateX({(1 - easeOutBack(notice)) * 380}px)"
		>
			<Logo size={34} />
			<div class="notice-text">
				<div class="notice-head"><b>Congress alert</b><span>now</span></div>
				<div>A member you track filed a new NVDA trade</div>
			</div>
		</div>
	{/if}

	{#if paletteShown > 0}
		<div
			class="palette glass"
			style:opacity={paletteShown}
			style:transform="translateY({(1 - paletteShown) * 8}px) scale({0.98 + paletteShown * 0.02})"
		>
			{#each rows as s, i (s.code + s.title)}
				<div class={['suggestion', { selected: i === 0 }]}>
					<span class="s-code">{s.code}</span>
					<span class="s-title">{s.title}</span>
					{#if s.kind}<span class="s-kind">{s.kind}</span>{/if}
				</div>
			{/each}
		</div>
	{/if}

	{#if barShown > 0}
		<div
			class="bar glass"
			style:opacity={barShown}
			style:transform="translateY({(1 - barShown) * 18}px)"
			style:filter="blur({(1 - barShown) * 6}px)"
		>
			<svg class="search" viewBox="0 0 16 16" aria-hidden="true">
				<circle cx="7" cy="7" r="5.25" />
				<path d="M11 11l3.2 3.2" />
			</svg>
			<span class="query">{query}</span>
			<span class="caret" style:opacity={caret ? 1 : 0}></span>
			<span class="spacer"></span>
			<span class={['go', { pressed }]}>GO</span>
		</div>
	{/if}

	{#if caption && captionShown > 0}
		<div class="caption" style:opacity={captionShown} style:transform="translateY({(1 - captionShown) * 6}px)">
			{#each caption.words as w, i (i)}
				<span style:opacity={0.32 + 0.68 * span(t, w.at - 60, w.at + 140)}>{w.text}</span>
			{/each}
		</div>
	{/if}
</div>

<style>
	.scene {
		--panel: #0c0c0d;
		--header: #1c1c1e;
		--text: #ececee;
		--muted: #8e8e93;
		--grid: #1d1d20;
		--line: rgba(255, 255, 255, 0.08);
		--up: #30d158;
		--down: #ff453a;
		--hl: #ffb000;
		--sma1: #0a84ff;
		--sma2: #ff375f;
		--buy: #0a84ff;
		--sell: #ff453a;
		--focus: #0a84ff;
		--itm: rgba(255, 255, 255, 0.045);
		position: relative;
		width: 1200px;
		height: 720px;
		overflow: hidden;
		color: var(--text);
		font-family: var(--sans);
		user-select: none;
	}

	:global(:root[data-theme='light']) .scene {
		--panel: #ffffff;
		--header: #ececee;
		--text: #1d1d1f;
		--muted: #6e6e73;
		--grid: #ededf0;
		--line: rgba(0, 0, 0, 0.1);
		--up: #1d1d1f;
		--down: #9a9aa0;
		--hl: #b26800;
		--sma1: #1d1d1f;
		--sma2: #8e8e93;
		--buy: #0a6cff;
		--sell: #d72828;
		--focus: #0a6cff;
		--itm: rgba(0, 0, 0, 0.035);
	}

	.wallpaper {
		position: absolute;
		inset: 0;
		background-color: #0b0d18;
		background-image:
			radial-gradient(circle farthest-corner at 50% 50%, rgba(64, 110, 255, 0.42), transparent 62%),
			radial-gradient(circle farthest-corner at 4% 112%, rgba(40, 200, 200, 0.26), transparent 60%),
			radial-gradient(circle farthest-corner at 104% 96%, rgba(130, 90, 255, 0.3), transparent 62%),
			radial-gradient(circle farthest-corner at 18% 8%, rgba(60, 90, 220, 0.3), transparent 64%),
			linear-gradient(158deg, #121830 0%, #0a0d1a 46%, #0e1222 100%);
	}

	.camera {
		position: absolute;
		inset: 0;
		transform-origin: 0 0;
		will-change: transform;
	}

	.window {
		position: absolute;
		left: 80px;
		top: 62px;
		width: 1040px;
		height: 624px;
		overflow: hidden;
		border-radius: 12px;
		background: center / cover url('/demo-dark.webp');
		box-shadow:
			0 0 0 0.5px rgba(255, 255, 255, 0.18),
			0 30px 80px -10px rgba(0, 0, 0, 0.7);
	}

	/* Not an <img>: only the current theme's capture loads, and it is right before hydration. */
	:global(:root[data-theme='light']) .window {
		background-image: url('/demo-light.webp');
	}

	.pane {
		position: absolute;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		border-radius: 4px;
		background: var(--panel);
		box-shadow: 0 0 0 1px var(--line);
		font-family: var(--mono);
	}

	.pane.focused {
		box-shadow: 0 0 0 1px var(--focus);
	}

	.tabs {
		display: flex;
		flex-shrink: 0;
		align-items: center;
		gap: 5px;
		height: 20px;
		padding: 0 6px;
		background: var(--header);
		font-size: 8.5px;
	}

	.code {
		color: var(--hl);
		font-weight: 700;
	}

	.title {
		font-family: var(--sans);
		font-size: 9.5px;
	}

	.spacer {
		flex: 1;
	}

	.intervals {
		position: relative;
		display: flex;
	}

	.thumb {
		position: absolute;
		top: 0;
		left: 0;
		width: 22px;
		height: 14px;
		border-radius: 3px;
		background: var(--line);
		box-shadow: inset 0 0 0 0.5px var(--line);
	}

	.interval {
		position: relative;
		width: 22px;
		color: var(--muted);
		line-height: 14px;
		text-align: center;
	}

	.interval.on {
		color: var(--text);
	}

	.chip {
		padding: 1px 5px;
		border-radius: 3px;
		background: var(--line);
		color: var(--muted);
	}

	.readout {
		display: flex;
		gap: 7px;
		padding: 4px 6px 0;
		color: var(--muted);
		font-size: 8.5px;
		font-variant-numeric: tabular-nums;
	}

	.up {
		color: var(--up);
	}

	.down {
		color: var(--down);
	}

	.plot {
		padding: 4px 4px 0 6px;
	}

	.empty {
		position: absolute;
		inset: 20px 0 0;
		display: grid;
		place-items: center;
		color: var(--muted);
		font-size: 10px;
	}

	.chain {
		display: flex;
		flex-direction: column;
		padding: 2px 0;
		font-size: 9px;
		font-variant-numeric: tabular-nums;
	}

	.chain-row {
		display: grid;
		grid-template-columns: repeat(3, 1fr) 1.1fr repeat(3, 1fr);
		height: 24px;
		align-items: center;
		border-bottom: 1px solid var(--line);
		text-align: right;
	}

	.chain-row span {
		padding-right: 8px;
		line-height: 23px;
	}

	.chain-row .itm {
		background: var(--itm);
	}

	.chain-row .iv {
		color: var(--muted);
	}

	.chain-row .strike {
		color: var(--text);
		font-weight: 700;
		text-align: center;
		padding: 0;
	}

	.chain-row.head {
		height: 18px;
		color: var(--muted);
		font-size: 8px;
	}

	.chain-row.head span {
		line-height: 17px;
	}

	.chain-row.atm {
		box-shadow: inset 0 0 0 1px var(--hl);
	}

	.menubar {
		position: absolute;
		inset: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 18px;
		height: 28px;
		padding: 0 18px;
		background: rgba(10, 10, 14, 0.32);
		backdrop-filter: blur(20px) saturate(180%);
		color: #f4f4f5;
		font-size: 13px;
	}

	.apple {
		width: 13px;
		height: 16px;
		fill: #f4f4f5;
	}

	.glass {
		background: rgba(28, 28, 34, 0.42);
		backdrop-filter: blur(22px) saturate(190%) brightness(106%);
		box-shadow:
			inset 0 0.5px 0 rgba(255, 255, 255, 0.32),
			inset 0 0 0 0.5px rgba(255, 255, 255, 0.14),
			0 24px 60px -12px rgba(0, 0, 0, 0.7);
		color: #f4f4f5;
	}

	.notice {
		position: absolute;
		top: 40px;
		right: 18px;
		display: flex;
		align-items: center;
		gap: 12px;
		width: 340px;
		padding: 12px 14px;
		border-radius: 18px;
		font-size: 13px;
		line-height: 17px;
	}

	.notice-text {
		display: flex;
		flex: 1;
		flex-direction: column;
		gap: 1px;
	}

	.notice-head {
		display: flex;
		justify-content: space-between;
	}

	.notice-head span {
		color: rgba(244, 244, 245, 0.6);
		font-size: 12px;
	}

	.palette {
		position: absolute;
		left: 390px;
		bottom: 214px;
		display: flex;
		flex-direction: column;
		gap: 2px;
		width: 420px;
		padding: 6px;
		border-radius: 16px;
		transform-origin: 50% 100%;
	}

	.suggestion {
		display: flex;
		align-items: center;
		gap: 12px;
		height: 38px;
		padding: 0 10px;
		border-radius: 10px;
		font-size: 14px;
	}

	.suggestion.selected {
		background: linear-gradient(168deg, rgba(255, 255, 255, 0.2), rgba(255, 255, 255, 0.07) 46%, rgba(255, 255, 255, 0.12));
		box-shadow:
			inset 0 0.5px 0 rgba(255, 255, 255, 0.5),
			inset 0 0 0 0.5px rgba(255, 255, 255, 0.16);
	}

	.s-code {
		width: 46px;
		color: #8e8e93;
		font: 700 12px var(--mono);
		text-align: center;
	}

	.selected .s-code {
		color: #f4f4f5;
	}

	.s-title {
		flex: 1;
		color: #d1d1d6;
	}

	.selected .s-title {
		color: #f4f4f5;
	}

	.s-kind {
		color: #8e8e93;
		font-size: 12px;
	}

	.bar {
		position: absolute;
		left: 410px;
		bottom: 132px;
		display: flex;
		align-items: center;
		gap: 10px;
		width: 380px;
		height: 62px;
		padding: 0 10px 0 20px;
		border-radius: 999px;
	}

	.search {
		width: 18px;
		height: 18px;
		fill: none;
		stroke: #8e8e93;
		stroke-width: 1.6;
		stroke-linecap: round;
	}

	.query {
		font: 20px var(--mono);
		letter-spacing: 0.02em;
		white-space: pre;
	}

	.caret {
		width: 2px;
		height: 22px;
		margin-left: -8px;
		border-radius: 1px;
		background: #f4f4f5;
	}

	.go {
		display: grid;
		place-items: center;
		height: 42px;
		padding: 0 18px;
		border-radius: 999px;
		background: linear-gradient(180deg, #ffffff 0%, #f2f2f7 55%, #e2e2e8 100%);
		box-shadow:
			inset 0 1px 0 #fff,
			inset 0 -1px 0 rgba(0, 0, 0, 0.08),
			0 6px 18px rgba(0, 0, 0, 0.35);
		color: #1c1c1e;
		font: 700 15px var(--mono);
		letter-spacing: 0.06em;
		transition: transform 0.12s;
	}

	.go.pressed {
		transform: scale(0.9);
	}

	.caption {
		position: absolute;
		left: 50%;
		bottom: 30px;
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 0 0.28em;
		max-width: 760px;
		padding: 9px 18px;
		border-radius: 14px;
		background: rgba(8, 8, 10, 0.62);
		backdrop-filter: blur(16px);
		color: #fff;
		font-size: 21px;
		font-weight: 600;
		letter-spacing: -0.01em;
		line-height: 28px;
		translate: -50% 0;
		white-space: nowrap;
	}
</style>
