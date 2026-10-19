<script lang="ts">
	import { easeInOut, span } from '#lib/ease.js';
	import { loop } from '#lib/motion.svelte.js';
	import Fit from './Fit.svelte';

	type Filing = [filed: string, traded: string, lag: string, name: string, party: string, type: string, ticker: string, amount: string];

	const FILINGS: Filing[] = [
		['10/15/26', '10/09/26', '6d', 'David J. Taylor', 'R-OH', 'BUY', 'RPM', '$1K–$15K'],
		['10/15/26', '10/12/26', '3d', 'David J. Taylor', 'R-OH', 'SELL', 'PG', '$1K–$15K'],
		['10/15/26', '10/09/26', '6d', 'David J. Taylor', 'R-OH', 'BUY', 'FITB', '$1K–$15K'],
		['10/15/26', '10/09/26', '6d', 'David J. Taylor', 'R-OH', 'BUY', 'T', '$1K–$15K'],
		['10/15/26', '09/24/26', '21d', 'Scott H. Peters', 'D-CA', 'SELL', '—', '$250K–$500K'],
		['10/15/26', '09/25/26', '20d', 'Scott H. Peters', 'D-CA', 'BUY', '—', '$250K–$500K'],
		['10/15/26', '09/03/26', '42d', 'Scott H. Peters', 'D-CA', 'BUY', '—', '$100K–$250K'],
		['10/14/26', '09/18/26', '26d', 'Richard McCormick', 'R-GA', 'BUY', 'BRK.B', '$1K–$15K'],
		['10/14/26', '08/31/26', '44d', 'Daniel Meuser', 'R-PA', 'SL-P', 'NVDA', '$1K–$15K'],
		['10/14/26', '09/18/26', '26d', 'Rick W. Allen', 'R-GA', 'SELL', 'COST', '$1K–$15K'],
		['10/14/26', '09/22/26', '22d', 'Rick W. Allen', 'R-GA', 'SELL', 'AZO', '$15K–$50K']
	];
	const COLUMNS = ['FILED', 'TRADED', 'LAG', 'POLITICIAN', 'PTY', 'C', 'TYPE', 'TICKER', 'AMOUNT'];
	const FILTERS = ['ALL', 'DEM', 'REP', 'IND', 'HOUSE', 'SENATE', 'BUYS', 'SELLS', 'TICKERS'];
	const ROW = 19;
	const STEPS = [3200, 3700, 4200];

	const clock = loop(8200, 2200);
	let t = $derived(clock.t);

	let arrive = $derived(easeInOut(span(t, 500, 950)) - easeInOut(span(t, 7300, 7800)));
	let glow = $derived(1 - span(t, 1000, 2900));
	let cursor = $derived(t < STEPS[0] || t > 6800 ? -1 : STEPS.filter((s) => t >= s).length);
	let pulse = $derived(0.35 + 0.65 * Math.abs(Math.sin(t / 420)));
</script>

<div class="card-visual" {@attach clock.observe}>
	<Fit width={506} height={304}>
		<div class="pane app">
			<div class="tabs">
				<span class="code">CONG</span>
				<span class="title">Congress</span>
				<span class="close">×</span>
			</div>
			<div class="filters">
				{#each FILTERS as f (f)}
					<span class={{ on: f === 'ALL' }}>{f}</span>
				{/each}
				<span class="status"><span class="dot" style:opacity={pulse}></span>checking Senate eFD + House</span>
			</div>
			<div class="row head">
				{#each COLUMNS as c (c)}
					<span>{c}</span>
				{/each}
			</div>
			<div class="body">
				<div class="rows" style:transform="translateY({(arrive - 1) * ROW}px)">
					{#each FILINGS as [filed, traded, lag, name, party, type, ticker, amount], i (i)}
						<div
							class={['row', { selected: i === cursor }]}
							style:background-color={i === 0 && glow > 0
								? `rgba(110, 155, 255, ${(glow * 0.22 * arrive).toFixed(3)})`
								: null}
							style:opacity={i === 0 ? arrive : null}
						>
							<span>{filed}</span>
							<span>{traded}</span>
							<span class="muted">{lag}</span>
							<span class={party.startsWith('D') ? 'dem' : 'rep'}>{name}</span>
							<span class={party.startsWith('D') ? 'dem' : 'rep'}>{party}</span>
							<span class="muted">H</span>
							<span class={type === 'BUY' ? 'buy' : 'sell'}>{type}</span>
							<span class="ticker">{ticker}</span>
							<span>{amount}</span>
						</div>
					{/each}
				</div>
			</div>
		</div>
	</Fit>
</div>

<style>
	.pane {
		position: absolute;
		left: 16px;
		top: 24px;
		width: 490px;
		height: 300px;
		overflow: hidden;
		border-radius: 10px 0 0 0;
		background: var(--panel);
		box-shadow:
			0 0 0 1px var(--line),
			0 20px 40px -12px var(--drop);
		color: var(--dim);
		font: 9.5px/1 var(--mono);
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

	.code,
	.ticker {
		color: var(--hl);
	}

	.tabs .code {
		font-weight: 700;
		font-size: 8.5px;
	}

	.title {
		color: var(--text);
		font: 11px var(--sans);
	}

	.close {
		color: var(--muted);
		font-size: 11px;
	}

	.filters {
		display: flex;
		align-items: center;
		gap: 4px;
		height: 24px;
		padding: 0 8px;
		color: var(--text);
		font-size: 9px;
		white-space: nowrap;
	}

	.filters > span {
		padding: 3px 6px;
		border-radius: 3px;
	}

	.filters .on {
		background: var(--accent);
		color: #fff;
	}

	.status {
		display: flex;
		align-items: center;
		gap: 5px;
		color: var(--muted);
	}

	.dot {
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: var(--up);
	}

	.row {
		display: grid;
		grid-template-columns: 50px 50px 26px 1fr 34px 12px 32px 38px 74px;
		align-items: center;
		gap: 4px;
		height: 19px;
		padding: 0 8px;
		white-space: nowrap;
	}

	.head {
		background: var(--header);
		color: var(--hl);
		font-size: 9px;
	}

	.body {
		overflow: hidden;
	}

	.rows {
		will-change: transform;
	}

	.row.selected {
		background: var(--control);
		box-shadow: inset 2px 0 var(--accent);
	}

	.muted {
		color: var(--muted);
	}

	.dem {
		color: #64a8ff;
	}

	.rep {
		color: #ff6961;
	}

	:global(:root[data-theme='light']) .dem {
		color: #0060df;
	}

	:global(:root[data-theme='light']) .rep {
		color: #c4291c;
	}

	.buy {
		color: var(--up);
	}

	.sell {
		color: var(--down);
	}
</style>
