import type { Sound } from './sound.js';

/** The scene is laid out at this size and scaled to fit. */
export const STAGE = { w: 1200, h: 720 };
export const WINDOW = { x: 80, y: 62, w: 1040, h: 624 };

/** Panes in window coordinates: the 3200×1920 screenshots at 0.325 scale. */
export const CHART_PANE = { x: 154, y: 33, w: 440, h: 288 };
export const SIDE_PANE = { x: 599, y: 33, w: 439, h: 288 };

export const DURATION = 20_000;
/** With reduced motion the scene holds here, where everything it animates is on screen. */
export const SETTLED = 17_900;

function typing(text: string, start: number, step: number) {
	return { text, start, keys: [...text].map((_, i) => start + i * step) };
}

const AAPL = typing('AAPL', 1000, 220);
const FIVE = typing('5', 7650, 0);
const OMON = typing('NVDA OMON', 11_000, 110);

export const T = {
	bar: [
		{ in: 700, go: 3500, out: 3900, typing: AAPL },
		{ in: 7300, go: 8050, out: 8350, typing: FIVE },
		{ in: 10_700, go: 12_400, out: 12_700, typing: OMON }
	],
	load: 3650,
	zoomIn: [5000, 5900],
	cursor: [
		[6100, 7100],
		[9200, 9900]
	],
	interval: 8100,
	reflowOut: [8100, 8380],
	reflowIn: [8380, 9100],
	zoomOut: [10_000, 10_800],
	options: [12_550, 12_950],
	rows: 12_850,
	rowStep: 85,
	notify: 15_300,
	markers: [4350, 4550, 4750]
} as const;

export const CHAPTERS = [
	{ label: 'Type', t: 0 },
	{ label: 'GO', t: 3400 },
	{ label: 'Chart', t: 4900 },
	{ label: 'Options', t: 10_000 },
	{ label: 'Alert', t: 15_000 }
] as const;

export type Caption = { start: number; end: number; words: { text: string; at: number }[] };

/** Spreads the words across [start, end], giving longer words more time. */
function caption(text: string, start: number, end: number): Caption {
	const parts = text.split(' ');
	const weights = parts.map((w) => w.length + 2);
	const total = weights.reduce((a, b) => a + b, 0);
	let at = start;
	const words = parts.map((w, i) => {
		const word = { text: w, at };
		at += ((end - start) * weights[i]) / total;
		return word;
	});
	return { start, end, words };
}

export const CAPTIONS = [
	caption('Type a ticker anywhere.', 1100, 2700),
	caption('Press GO, and the chart is already there.', 3450, 5300),
	caption('Five-minute bars? Just type five.', 6900, 9000),
	caption('Option chains are one command away.', 10_900, 13_200),
	caption('And when Congress trades, you’ll know.', 15_400, 17_600)
];

export type Suggestion = { code: string; title: string; kind?: string };

const SUGGESTIONS: [string, Suggestion[]][] = [
	['NVDA OM', [{ code: 'OMON', title: 'Option chain', kind: 'NVDA US Equity' }]],
	[
		'NVDA O',
		[
			{ code: 'OMON', title: 'Option chain', kind: 'NVDA US Equity' },
			{ code: 'ORD', title: 'Orders', kind: 'NVDA US Equity' }
		]
	],
	[
		'NV',
		[
			{ code: 'NVDA', title: 'NVIDIA Corp', kind: 'US Equity' },
			{ code: 'GP', title: 'Price chart' },
			{ code: 'OMON', title: 'Option chain' },
			{ code: 'CONG', title: 'Congress trades' }
		]
	],
	[
		'N',
		[
			{ code: 'NVDA', title: 'NVIDIA Corp', kind: 'US Equity' },
			{ code: 'NFLX', title: 'Netflix Inc', kind: 'US Equity' },
			{ code: 'NKE', title: 'Nike Inc', kind: 'US Equity' },
			{ code: 'NOW', title: 'ServiceNow Inc', kind: 'US Equity' }
		]
	],
	[
		'AAPL',
		[
			{ code: 'AAPL', title: 'Apple Inc', kind: 'US Equity' },
			{ code: 'GP', title: 'Price chart' },
			{ code: 'OMON', title: 'Option chain' },
			{ code: 'CN', title: 'News' }
		]
	],
	[
		'AAP',
		[
			{ code: 'AAPL', title: 'Apple Inc', kind: 'US Equity' },
			{ code: 'AAP', title: 'Advance Auto Parts', kind: 'US Equity' }
		]
	],
	[
		'AA',
		[
			{ code: 'AAPL', title: 'Apple Inc', kind: 'US Equity' },
			{ code: 'AAL', title: 'American Airlines', kind: 'US Equity' }
		]
	],
	[
		'A',
		[
			{ code: 'AAPL', title: 'Apple Inc', kind: 'US Equity' },
			{ code: 'AMZN', title: 'Amazon.com Inc', kind: 'US Equity' },
			{ code: 'AMD', title: 'Advanced Micro Devices', kind: 'US Equity' },
			{ code: 'ABNB', title: 'Airbnb Inc', kind: 'US Equity' }
		]
	],
	['5', [{ code: '5', title: 'Interval', kind: '5 minutes' }]]
];

/** What the palette lists for a query: the entry for its longest matching prefix. */
export function suggestions(query: string) {
	return SUGGESTIONS.find(([prefix]) => query.startsWith(prefix))?.[1] ?? [];
}

export type Cue = { t: number; sound: Sound; variant?: number };

const cue = (t: number, sound: Sound, variant?: number): Cue => ({ t, sound, variant });

export const CUES: Cue[] = [
	...T.bar.flatMap((bar) => [
		cue(bar.in, 'whoosh'),
		...bar.typing.keys.map((t, i) => cue(t, bar.typing.text[i] === ' ' ? 'space' : 'key', i)),
		cue(bar.go, 'enter')
	]),
	cue(T.load, 'sweep'),
	cue(T.zoomIn[0], 'whoosh'),
	...T.cursor.flatMap(([start, end]) =>
		Array.from({ length: 6 }, (_, i) => cue(start + ((end - start) * i) / 6, 'tick'))
	),
	cue(T.interval, 'switch'),
	cue(T.reflowIn[0], 'sweep'),
	cue(T.zoomOut[0], 'whoosh'),
	...Array.from({ length: 10 }, (_, i) => cue(T.rows + i * T.rowStep, 'tick')),
	cue(T.notify, 'chime'),
	...T.markers.map((t) => cue(t, 'pop'))
].sort((a, b) => a.t - b.t);
