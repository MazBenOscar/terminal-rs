<script lang="ts">
	import { random, span } from '#lib/ease.js';
	import { loop } from '#lib/motion.svelte.js';
	import Fit from './Fit.svelte';

	const POSITIONS = [
		{ symbol: 'AAPL', qty: 40, last: 343.46, day: 0.0043 },
		{ symbol: 'NVDA', qty: 60, last: 214.13, day: 0.017 },
		{ symbol: 'MSFT', qty: 12, last: 512.3, day: -0.005 }
	].map((p) => ({ ...p, prev: p.last / (1 + p.day) }));
	const CASH = 17_154.47;
	const DURATION = 8400;
	const STEP = 700;
	const FLASH = 650;

	const rand = random(3);
	const TICKS = Array.from({ length: DURATION / STEP }, (_, k) => {
		const row = [0, 1, 2, 1, 0, 1][k % 6];
		return { t: k * STEP + 300, row, move: (rand() - 0.42) * 0.0016 };
	});

	const clock = loop(DURATION, 0);
	let t = $derived(clock.t);

	let rows = $derived.by(() => {
		const scale = POSITIONS.map(() => 1);
		const flash = POSITIONS.map(() => ({ up: true, a: 0 }));
		for (const tick of TICKS) {
			if (tick.t > t) break;
			scale[tick.row] *= 1 + tick.move;
			flash[tick.row] = { up: tick.move >= 0, a: 1 - span(t, tick.t, tick.t + FLASH) };
		}
		return POSITIONS.map((p, i) => {
			const last = p.last * scale[i];
			return { ...p, last, value: last * p.qty, change: last / p.prev - 1, flash: flash[i] };
		});
	});

	let total = $derived(rows.reduce((sum, r) => sum + r.value, CASH));
	let dayChange = $derived(rows.reduce((sum, r) => sum + (r.last - r.prev) * r.qty, 0));
	let dayPct = $derived((dayChange / (total - dayChange)) * 100);

	const money = (v: number) => v.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
	const signed = (v: number, text: string) => `${v >= 0 ? '+' : '−'}${text}`;
	const tint = (f: { up: boolean; a: number }) =>
		f.a > 0 ? `color-mix(in srgb, ${f.up ? 'var(--up)' : 'var(--down)'} ${Math.round(f.a * 30)}%, transparent)` : null;
</script>

<div class="card-visual" {@attach clock.observe}>
	<Fit width={506} height={304}>
		<div class="pane app">
			<div class="tabs">
				<span class="code">PORT</span>
				<span class="title">Portfolio</span>
				<span class="account">Individual ···4821</span>
			</div>
			<div class="summary">
				<div>
					<div class="label">Net liquidation</div>
					<div class="total">${money(total)}</div>
					<div class={dayChange >= 0 ? 'up' : 'down'}>
						{signed(dayChange, `$${money(Math.abs(dayChange))}`)}
						{signed(dayPct, `${Math.abs(dayPct).toFixed(2)}%`)} today
					</div>
				</div>
				<div class="segments">
					<span class="on">Positions</span>
					<span>Orders</span>
					<span>Activity</span>
				</div>
			</div>
			<div class="row head">
				{#each ['SYMBOL', 'QTY', 'LAST', 'MKT VALUE', 'DAY'] as c, i (c)}
					<span class={{ num: i > 0 }}>{c}</span>
				{/each}
			</div>
			{#each rows as r (r.symbol)}
				<div class="row">
					<span class="symbol">{r.symbol}</span>
					<span class="num">{r.qty}</span>
					<span class="num cell" style:background-color={tint(r.flash)}>{money(r.last)}</span>
					<span class="num cell" style:background-color={tint(r.flash)}>{money(r.value)}</span>
					<span class={['num', r.change >= 0 ? 'up' : 'down']}>
						{signed(r.change, `${Math.abs(r.change * 100).toFixed(2)}%`)}
					</span>
				</div>
			{/each}
			<div class="row cash">
				<span>CASH</span><span></span><span></span><span class="num">{money(CASH)}</span><span></span>
			</div>
		</div>
	</Fit>
</div>

<style>
	.pane {
		position: absolute;
		left: 16px;
		top: 24px;
		width: 474px;
		height: 300px;
		overflow: hidden;
		border-radius: 10px 10px 0 0;
		background: var(--panel);
		box-shadow:
			0 0 0 1px var(--line),
			0 20px 40px -12px var(--drop);
		color: var(--text);
		font-family: var(--sans);
		font-variant-numeric: tabular-nums;
	}

	.tabs {
		display: flex;
		align-items: center;
		gap: 7px;
		height: 28px;
		padding: 0 14px;
		background: var(--header);
	}

	.code {
		color: var(--hl);
		font: 700 9px var(--mono);
	}

	.title {
		flex: 1;
		font-size: 12px;
	}

	.account {
		color: var(--muted);
		font-size: 11px;
	}

	.summary {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		padding: 16px 16px 12px;
	}

	.label {
		color: var(--muted);
		font-size: 12px;
	}

	.total {
		margin: 4px 0 3px;
		font-size: 28px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}

	.summary .up,
	.summary .down {
		font-size: 12.5px;
	}

	.segments {
		display: flex;
		gap: 2px;
		padding: 3px;
		border-radius: 999px;
		background: var(--control);
		box-shadow: inset 0 0 0 0.5px var(--line);
		color: var(--muted);
		font-size: 12.5px;
	}

	.segments span {
		padding: 6px 12px;
		border-radius: 999px;
	}

	.segments .on {
		background: linear-gradient(168deg, rgba(255, 255, 255, 0.2), rgba(255, 255, 255, 0.07));
		box-shadow:
			inset 0 0.5px 0 rgba(255, 255, 255, 0.5),
			inset 0 0 0 0.5px rgba(255, 255, 255, 0.16);
		color: var(--text);
	}

	:global(:root[data-theme='light']) .segments .on {
		background: #fff;
		box-shadow:
			0 0 0 0.5px rgba(0, 0, 0, 0.06),
			0 1px 3px rgba(0, 0, 0, 0.14);
	}

	.row {
		display: grid;
		grid-template-columns: 1fr 50px 90px 110px 80px;
		align-items: center;
		height: 30px;
		margin: 0 16px;
		border-bottom: 1px solid var(--line);
		font: 11.5px var(--mono);
	}

	.head {
		height: 26px;
		color: var(--muted);
		font-size: 9.5px;
	}

	.num {
		padding-right: 4px;
		text-align: right;
	}

	.cell {
		margin-left: 14px;
		padding: 3px 4px;
		border-radius: 3px;
	}

	.symbol {
		color: var(--hl);
	}

	.cash {
		border-bottom: 0;
		color: var(--muted);
	}

	.cash .num {
		color: var(--text);
	}

	.up {
		color: var(--up);
	}

	.down {
		color: var(--down);
	}
</style>
