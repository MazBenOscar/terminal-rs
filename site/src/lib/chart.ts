import { random } from '#lib/ease.js';

export type Candle = { o: number; h: number; l: number; c: number; v: number };

/** A seeded random walk bent so it ends exactly on `last`. */
export function series(seed: number, count: number, first: number, last: number, vol: number): Candle[] {
	const rand = random(seed);
	const walk: number[] = [];
	let p = first;
	for (let i = 0; i < count; i++) {
		p *= 1 + (rand() - 0.48) * vol;
		walk.push(p);
	}
	const bend = last / walk[count - 1];
	const closes = walk.map((c, i) => c * bend ** (i / (count - 1)));
	return closes.map((c, i) => {
		const o = i ? closes[i - 1] * (1 + (rand() - 0.5) * vol * 0.3) : first;
		return {
			o,
			c,
			h: Math.max(o, c) * (1 + rand() * vol * 0.5),
			l: Math.min(o, c) * (1 - rand() * vol * 0.5),
			v: 0.25 + rand() ** 2 * 0.75
		};
	});
}

export type History = { candles: Candle[]; averages: (number | null)[][] };

const WARMUP = 50;

/** `count` candles with 20- and 50-bar moving averages that include the bars before them. */
function history(seed: number, count: number, first: number, last: number, vol: number): History {
	const all = series(seed, WARMUP + count, first, last, vol);
	return {
		candles: all.slice(WARMUP),
		averages: [sma(all, 20), sma(all, 50)].map((avg) => avg.slice(WARMUP))
	};
}

export const daily = history(11, 64, 205, 343.46, 0.024);
export const intraday = history(5, 78, 337.8, 343.46, 0.0028);

/** Simple moving average of closes; null until there are `n` candles. */
export function sma(candles: Candle[], n: number) {
	let sum = 0;
	return candles.map((k, i) => {
		sum += k.c - (i >= n ? candles[i - n].c : 0);
		return i >= n - 1 ? sum / n : null;
	});
}

/** Round axis steps: 1, 2 or 5 times a power of ten. */
export function ticks(lo: number, hi: number, count: number) {
	const raw = (hi - lo) / count;
	const mag = 10 ** Math.floor(Math.log10(raw));
	const step = [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? raw;
	const out: number[] = [];
	for (let v = Math.ceil(lo / step) * step; v <= hi; v += step) out.push(v);
	return out;
}
