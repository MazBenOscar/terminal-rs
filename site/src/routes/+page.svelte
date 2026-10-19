<script lang="ts">
	import { asset, resolve } from '$app/paths';
	import { ARCH, DESCRIPTION, MIN_MACOS, SITE_URL, TITLE, VERSION } from '#lib/site.js';

	const sizes = '(min-width: 1080px) 1032px, calc(100vw - 40px)';
	const narrow = '(max-width: 640px)';
	const shots = [
		{
			theme: 'dark',
			alt: 'terminal-rs on macOS in the dark theme: a watchlist and an AAPL chart with congressional trade markers'
		},
		{
			theme: 'light',
			alt: 'terminal-rs on macOS in the light theme: a watchlist and an AAPL chart with congressional trade markers'
		}
	] as const;
</script>

<svelte:head>
	<title>{TITLE}</title>
	<meta name="description" content={DESCRIPTION} />
	<link rel="canonical" href={SITE_URL} />
	<meta property="og:type" content="website" />
	<meta property="og:url" content={SITE_URL} />
	<meta property="og:title" content={TITLE} />
	<meta property="og:description" content={DESCRIPTION} />
	<meta property="og:image" content="{SITE_URL}/og.jpg" />
	<meta property="og:image:width" content="1200" />
	<meta property="og:image:height" content="630" />
	<meta property="og:image:alt" content="terminal-rs: the market, one command away" />
	<meta name="twitter:card" content="summary_large_image" />
	<meta name="twitter:title" content={TITLE} />
	<meta name="twitter:description" content={DESCRIPTION} />
	<meta name="twitter:image" content="{SITE_URL}/og.jpg" />
</svelte:head>

<div class="wrap">
	<header>
		<button type="button" class="theme-toggle" data-theme-toggle aria-label="Switch theme">
			<svg class="sun" viewBox="0 0 24 24" aria-hidden="true">
				<circle cx="12" cy="12" r="4" />
				<path
					d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41"
				/>
			</svg>
			<svg class="moon" viewBox="0 0 24 24" aria-hidden="true">
				<path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
			</svg>
		</button>

		<img class="icon" src={asset('icon.png')} width="112" height="112" alt="terminal-rs app icon" />
		<h1>The market,<br /> one command away.</h1>
		<p class="lede">
			A fast, keyboard-driven market terminal for the Mac, written in Rust. GPU charts, dense tiled
			panes and a command line for stocks, crypto, FX, Treasuries, futures and options.
		</p>
		<a class="btn" href={resolve('/download')}>
			<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v12M6 11l6 6 6-6M5 21h14" /></svg>
			Download for Mac
		</a>
		<p class="meta">Version {VERSION} · {ARCH} · Free</p>

		<div class="shot">
			{#each shots as shot (shot.theme)}
				<picture class={shot.theme}>
					<source
						media={narrow}
						type="image/avif"
						srcset={asset(`hero-${shot.theme}-crop.avif`)}
						width="1080"
						height="587"
					/>
					<source
						media={narrow}
						type="image/webp"
						srcset={asset(`hero-${shot.theme}-crop.webp`)}
						width="1080"
						height="587"
					/>
					<source
						type="image/avif"
						srcset="{asset(`hero-${shot.theme}-1080.avif`)} 1080w, {asset(
							`hero-${shot.theme}-2160.avif`
						)} 2160w"
						{sizes}
					/>
					<img
						src={asset(`hero-${shot.theme}-1080.webp`)}
						srcset="{asset(`hero-${shot.theme}-1080.webp`)} 1080w, {asset(
							`hero-${shot.theme}-2160.webp`
						)} 2160w"
						{sizes}
						width="2160"
						height="1296"
						alt={shot.alt}
						loading="lazy"
						decoding="async"
					/>
				</picture>
			{/each}
		</div>
	</header>

	<section class="features" aria-labelledby="features-title">
		<h2 id="features-title" class="visually-hidden">Features</h2>
		<article>
			<span class="code">&lt;GO&gt;</span>
			<h3>Command line</h3>
			<p>
				Start typing anywhere. <code>AAPL &lt;GO&gt;</code> loads a chart, <code>5</code> switches
				to five-minute bars and <code>NVDA OMON</code> opens the option chain, with matches across every
				market as you type.
			</p>
		</article>
		<article>
			<span class="code">GP</span>
			<h3>GPU charts</h3>
			<p>
				Candles, bars, line and area drawn on the GPU, with moving averages, VWAP, Bollinger bands,
				RSI, MACD, drawings, compare and log scale. Pans and zooms smoothly across a million bars.
			</p>
		</article>
		<article>
			<span class="code">WEI</span>
			<h3>Multi-asset</h3>
			<p>
				Stocks, ETFs, indices, FX, crypto, US Treasuries, futures and options from free public
				sources, with no API keys. World markets, the yield curve, headlines and live crypto order
				books.
			</p>
		</article>
		<article>
			<span class="code">CONG</span>
			<h3>Congress trades</h3>
			<p>
				Stock trades by members of Congress, read from the official House and Senate disclosure
				filings. See who traded a ticker, their buys and sells marked on the chart, and an alert when
				someone you track files.
			</p>
		</article>
		<article>
			<span class="code">PORT</span>
			<h3>Your brokerage</h3>
			<p>
				Connect your brokers through SnapTrade to see accounts, balances, positions, orders and
				activity, with holdings repriced at the terminal's live quotes. Keys stay in your Mac's
				Keychain.
			</p>
		</article>
		<article>
			<span class="code">LAYOUT</span>
			<h3>Your desk</h3>
			<p>
				Dock any panel anywhere, link panes by colour so one ticker drives them all, and save named
				layouts. Eight themes, from amber on black to Solarized and Nord, with a live editor.
			</p>
		</article>
	</section>

	<footer>
		<p>
			Requires macOS {MIN_MACOS} or later on {ARCH}. Made by <a href="https://rasmic.xyz">Micky</a>.
		</p>
		<p class="fine">Quotes come from free public sources and may be delayed. Not investment advice.</p>
	</footer>
</div>

<style>
	.wrap {
		max-width: 1080px;
		margin: 0 auto;
		padding: 0 24px;
	}

	header {
		position: relative;
		text-align: center;
		padding: 88px 0 40px;
	}

	.theme-toggle {
		position: absolute;
		top: 24px;
		right: 0;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 38px;
		height: 38px;
		padding: 0;
		border: 1px solid var(--line);
		border-radius: 10px;
		background: var(--card);
		color: var(--muted);
		cursor: pointer;
		transition:
			color 0.15s,
			border-color 0.15s;
	}

	.theme-toggle:hover {
		color: var(--fg);
	}

	.theme-toggle svg {
		width: 18px;
		height: 18px;
		fill: none;
		stroke: currentColor;
		stroke-width: 2;
		stroke-linecap: round;
		stroke-linejoin: round;
	}

	.theme-toggle .moon,
	:global(:root[data-theme='dark']) .theme-toggle .sun {
		display: block;
	}

	.theme-toggle .sun,
	:global(:root[data-theme='dark']) .theme-toggle .moon {
		display: none;
	}

	.icon {
		display: block;
		width: 112px;
		height: 112px;
		margin: 0 auto;
		filter: drop-shadow(0 12px 28px rgba(0, 0, 0, 0.35));
	}

	h1 {
		margin: 18px 0 14px;
		font-family: var(--mono);
		font-size: clamp(30px, 6.2vw, 58px);
		font-weight: 600;
		line-height: 1.08;
		letter-spacing: -0.035em;
		text-wrap: balance;
	}

	.lede {
		max-width: 600px;
		margin: 0 auto 32px;
		color: var(--muted);
		font-size: 19px;
		text-wrap: pretty;
	}

	.btn {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		padding: 14px 26px;
		border-radius: 12px;
		background: var(--accent);
		color: var(--accent-ink);
		font-size: 16px;
		font-weight: 650;
		text-decoration: none;
		box-shadow: 0 6px 22px rgba(255, 176, 0, 0.24);
		transition:
			transform 0.15s,
			box-shadow 0.15s;
	}

	.btn:hover {
		transform: translateY(-1px);
		box-shadow: 0 10px 28px rgba(255, 176, 0, 0.34);
	}

	.btn svg {
		width: 18px;
		height: 18px;
		fill: none;
		stroke: currentColor;
		stroke-width: 2.2;
		stroke-linecap: round;
		stroke-linejoin: round;
	}

	.meta {
		margin: 14px 0 0;
		color: var(--muted);
		font: 13px var(--mono);
		font-variant-numeric: tabular-nums;
	}

	.shot {
		margin: 44px auto 0;
	}

	.shot picture {
		display: block;
	}

	.shot img {
		display: block;
		width: 100%;
		height: auto;
		border-radius: 1% / 1.67%;
		box-shadow: var(--shot-shadow);
	}

	.shot .light,
	:global(:root[data-theme='light']) .shot .dark {
		display: none;
	}

	:global(:root[data-theme='light']) .shot .light {
		display: block;
	}

	.features {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
		gap: 16px;
		padding: 72px 0 24px;
	}

	article {
		padding: 20px 22px 22px;
		border: 1px solid var(--line);
		border-radius: 12px;
		background: var(--card);
	}

	.code {
		display: inline-block;
		padding: 3px 7px;
		border-radius: 5px;
		background: var(--accent-wash);
		color: var(--accent-text);
		font: 600 12px/1.4 var(--mono);
		letter-spacing: 0.04em;
	}

	h3 {
		margin: 12px 0 6px;
		font: 600 17px/1.3 var(--mono);
		letter-spacing: -0.01em;
	}

	article p {
		margin: 0;
		color: var(--muted);
		font-size: 15px;
	}

	code {
		padding: 1px 5px;
		border-radius: 4px;
		background: var(--chip);
		color: var(--fg);
		font: 0.88em var(--mono);
		white-space: nowrap;
	}

	footer {
		padding: 48px 0 64px;
		color: var(--muted);
		font-size: 13px;
		text-align: center;
	}

	footer p {
		margin: 0;
	}

	footer .fine {
		margin-top: 6px;
		opacity: 0.8;
	}

	.visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}

	@media (max-width: 640px) {
		.wrap {
			padding: 0 20px;
		}

		header {
			padding-top: 76px;
		}

		.lede {
			font-size: 17px;
		}

		.shot {
			margin-top: 36px;
		}

		.shot img {
			border-radius: 6px;
		}

		.features {
			grid-template-columns: 1fr;
			padding-top: 48px;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.btn,
		.theme-toggle {
			transition: none;
		}

		.btn:hover {
			transform: none;
		}
	}
</style>
