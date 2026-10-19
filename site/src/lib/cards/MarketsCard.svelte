<script lang="ts">
	import { series } from '#lib/chart.js';
	import { random, span } from '#lib/ease.js';
	import { loop } from '#lib/motion.svelte.js';
	import Fit from './Fit.svelte';

	type Row = { code: string; name: string; last: number; chg: number };

	const GROUPS: { region: string; rows: Row[] }[] = [
		{
			region: 'AMERICAS',
			rows: [
				{ code: 'SPX', name: 'S&P 500 Index', last: 7659.44, chg: -84.62 },
				{ code: 'INDU', name: 'Dow Jones Industrial Average', last: 52002.41, chg: -175.28 },
				{ code: 'CCMP', name: 'NASDAQ Composite Index', last: 26140.03, chg: -573.0 },
				{ code: 'NDX', name: 'NASDAQ-100 Index', last: 29187.55, chg: -748.05 },
				{ code: 'RTY', name: 'Russell 2000 Index', last: 2721.37, chg: -45.9 },
				{ code: 'VIX', name: 'Cboe Volatility Index', last: 19.99, chg: 3.53 },
				{ code: 'SPTSX', name: 'S&P/TSX Composite', last: 36176.6, chg: 39.5 },
				{ code: 'IBOV', name: 'Brazil Ibovespa', last: 205430.0, chg: 1106.0 },
				{ code: 'MEXBOL', name: 'Mexico IPC', last: 68929.72, chg: 1266.43 }
			]
		},
		{
			region: 'EMEA',
			rows: [
				{ code: 'UKX', name: 'FTSE 100 Index', last: 10881.9, chg: 41.5 },
				{ code: 'DAX', name: 'DAX Index', last: 25622.61, chg: 239.99 },
				{ code: 'CAC', name: 'CAC 40 Index', last: 7956.7, chg: 22.0 },
				{ code: 'SX5E', name: 'Euro Stoxx 50', last: 6204.23, chg: 35.97 },
				{ code: 'SMI', name: 'Swiss Market Index', last: 14391.31, chg: 41.8 }
			]
		}
	];

	const ROWS = GROUPS.flatMap((g) => g.rows);
	const DURATION = 9000;
	const STEP = 450;
	const FLASH = 700;

	const lines = ROWS.map((r, i) => {
		const closes = series(31 + i, 26, r.last - r.chg, r.last, 0.004).map((k) => k.c);
		const lo = Math.min(...closes);
		const hi = Math.max(...closes);
		return closes.map((c, j) => `${(j * 2.4).toFixed(1)},${(12 - ((c - lo) / (hi - lo || 1)) * 11).toFixed(1)}`).join(' ');
	});

	const rand = random(7);
	const TICKS = Array.from({ length: DURATION / STEP }, (_, k) => {
		const row = (k * 7 + 3) % ROWS.length;
		return { t: k * STEP + 200, row, delta: (rand() - 0.45) * 0.0014 * ROWS[row].last };
	});

	const clock = loop(DURATION, 0);
	let t = $derived(clock.t);

	let live = $derived.by(() => {
		const moved = ROWS.map(() => 0);
		const flash: { up: boolean; a: number }[] = ROWS.map(() => ({ up: true, a: 0 }));
		for (const tick of TICKS) {
			if (tick.t > t) break;
			moved[tick.row] += tick.delta;
			flash[tick.row] = { up: tick.delta >= 0, a: 1 - span(t, tick.t, tick.t + FLASH) };
		}
		return ROWS.map((r, i) => {
			const last = r.last + moved[i];
			const chg = r.chg + moved[i];
			return { last, chg, pct: (chg / (r.last - r.chg)) * 100, flash: flash[i] };
		});
	});

	let clockText = $derived.by(() => {
		const s = 30 + Math.floor(t / 1000);
		return `09:55:${String(s).padStart(2, '0')}`;
	});

	const sign = (v: number) => (v >= 0 ? '+' : '−');
	const money = (v: number) => v.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
	const indexOf = (row: Row) => ROWS.indexOf(row);
</script>

<div class="card-visual" {@attach clock.observe}>
	<Fit width={506} height={395}>
		<div class="pane app">
			<div class="tabs">
				<span class="code">WEI</span>
				<span class="title">World Markets</span>
				<span class="close">×</span>
			</div>
			<div class="status">
				<span class="code">WORLD MARKETS</span> updated {clockText} click a row to chart it
			</div>
			{#each GROUPS as group (group.region)}
				<div class="group">
					<span>{group.region}</span>
					<span class="num">LAST</span>
					<span class="num">NET CHG</span>
					<span class="num">%CHG</span>
				</div>
				{#each group.rows as row (row.code)}
					{@const i = indexOf(row)}
					{@const v = live[i]}
					{@const up = v.chg >= 0}
					<div class="row">
						<span class="code">{row.code}</span>
						<span class="name">{row.name}</span>
						<svg viewBox="0 0 60 13" class={up ? 'up' : 'down'} aria-hidden="true">
							<polyline points={lines[i]} />
						</svg>
						<span
							class="num last"
							style:background-color={v.flash.a > 0
								? `color-mix(in srgb, ${v.flash.up ? 'var(--up)' : 'var(--down)'} ${Math.round(v.flash.a * 34)}%, transparent)`
								: null}
						>
							{money(v.last)}
						</span>
						<span class={['num', up ? 'up' : 'down']}>{sign(v.chg)}{money(Math.abs(v.chg))}</span>
						<span class={['num', up ? 'up' : 'down']}>{sign(v.pct)}{Math.abs(v.pct).toFixed(2)}%</span>
					</div>
				{/each}
			{/each}
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
		width: 474px;
		height: 330px;
		overflow: hidden;
		border-radius: 10px;
		background: var(--panel);
		box-shadow:
			0 0 0 1px var(--line),
			0 20px 40px -12px var(--drop);
		color: var(--dim);
		font: 8.5px/1 var(--mono);
		font-variant-numeric: tabular-nums;
	}

	.tabs {
		display: flex;
		align-items: center;
		gap: 6px;
		height: 24px;
		padding: 0 10px;
		background: var(--header);
	}

	.title {
		color: var(--text);
		font: 11px var(--sans);
	}

	.close {
		color: var(--muted);
		font-size: 11px;
	}

	.code {
		color: var(--hl);
	}

	.tabs .code {
		font-weight: 700;
	}

	.status {
		padding: 6px 10px 5px;
		color: var(--muted);
	}

	.group,
	.row {
		display: grid;
		grid-template-columns: 46px 1fr 62px 62px 58px 50px;
		align-items: center;
		gap: 4px;
		padding: 0 10px;
	}

	.group {
		height: 15px;
		margin-top: 3px;
		background: var(--header);
		color: var(--text);
		font-size: 8px;
	}

	.group .num {
		color: var(--muted);
	}

	.group span:first-child {
		grid-column: 1 / 4;
	}

	.row {
		height: 15px;
	}

	.name {
		overflow: hidden;
		color: var(--dim);
		white-space: nowrap;
		text-overflow: ellipsis;
	}

	.num {
		text-align: right;
	}

	.last {
		margin: 0 -3px;
		padding: 2px 3px;
		border-radius: 2px;
		color: var(--text);
	}

	svg {
		width: 60px;
		height: 13px;
		fill: none;
		stroke-width: 1;
	}

	svg.up {
		stroke: var(--up);
	}

	svg.down {
		stroke: var(--down);
	}

	.up {
		color: var(--up);
	}

	.down {
		color: var(--down);
	}
</style>
