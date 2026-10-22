//! Level-of-detail pyramids. Level `k` merges `2^k` consecutive bars, so drawing any
//! zoom level costs O(plot width) instances instead of O(visible bars): at 1M bars fully
//! zoomed out we render ~800 merged candles, not a million.

use bytemuck::{Pod, Zeroable};
use tr_core::Bar;

/// One instance as uploaded to the GPU storage buffer (std430: 8 x f32).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct GpuBar {
    /// Index of the first level-0 bar covered.
    pub x: f32,
    /// Number of level-0 bars covered.
    pub w: f32,
    pub open: f32,
    pub high: f32,
    pub low: f32,
    pub close: f32,
    pub volume: f32,
    pub _pad: f32,
}

/// Byte ranges changed by the most recent tail patch.
pub type DirtyRanges = Vec<std::ops::Range<usize>>;

fn padded<T: Copy + Zeroable>(data: Vec<T>, offsets: &mut [usize], len: usize) -> Vec<T> {
    let mut out = Vec::new();
    let mut n = len;
    for offset in offsets {
        let start = *offset;
        *offset = out.len();
        out.extend_from_slice(&data[start..start + n]);
        out.resize(out.len() + n.max(16).div_ceil(4), T::zeroed());
        n = n.div_ceil(2);
    }
    out
}

fn can_patch(offsets: &[usize], data_len: usize, old_len: usize, len: usize, from: usize) -> bool {
    let tail = 1024.min(old_len.div_ceil(4).max(1));
    if len < old_len || from > old_len || from < old_len.saturating_sub(tail) {
        return false;
    }
    let mut n = len;
    for (k, start) in offsets.iter().enumerate() {
        let end = offsets.get(k + 1).copied().unwrap_or(data_len);
        if n > end - start || k + 1 == offsets.len() && n > 1 {
            return false;
        }
        n = n.div_ceil(2);
    }
    true
}

/// Target minimum on-screen width of one drawn instance.
pub const MIN_INSTANCE_PX: f64 = 2.0;

/// Pick the pyramid level for a zoom of `bar_px` pixels per level-0 bar.
pub fn level_for(bar_px: f64, levels: usize) -> usize {
    if bar_px >= MIN_INSTANCE_PX || levels <= 1 {
        return 0;
    }
    let k = (MIN_INSTANCE_PX / bar_px).log2().ceil().max(0.0) as usize;
    k.min(levels - 1)
}

#[derive(Debug, Clone, Default)]
pub struct BarPyramid {
    /// All levels concatenated, level 0 first, with spare capacity after each level.
    pub data: Vec<GpuBar>,
    /// Start offset of each level within `data`.
    pub offsets: Vec<usize>,
    pub len0: usize,
    pub dirty: DirtyRanges,
    pub version: u64,
}

impl BarPyramid {
    pub fn build(bars: &[Bar]) -> Self {
        let mut data: Vec<GpuBar> = bars
            .iter()
            .enumerate()
            .map(|(i, b)| GpuBar {
                x: i as f32,
                w: 1.0,
                open: b.open as f32,
                high: b.high as f32,
                low: b.low as f32,
                close: b.close as f32,
                volume: b.volume as f32,
                _pad: 0.0,
            })
            .collect();
        let mut offsets = vec![0];
        let (mut start, mut n) = (0, bars.len());
        while n > 1 {
            let next_start = data.len();
            for j in (0..n).step_by(2) {
                let a = data[start + j];
                let merged = if j + 1 < n {
                    let b = data[start + j + 1];
                    GpuBar {
                        x: a.x,
                        w: a.w + b.w,
                        open: a.open,
                        high: a.high.max(b.high),
                        low: a.low.min(b.low),
                        close: b.close,
                        volume: a.volume + b.volume,
                        _pad: 0.0,
                    }
                } else {
                    a
                };
                data.push(merged);
            }
            offsets.push(next_start);
            start = next_start;
            n = n.div_ceil(2);
        }
        let data = padded(data, &mut offsets, bars.len());
        Self { data, offsets, len0: bars.len(), dirty: Vec::new(), version: 1 }
    }

    pub fn levels(&self) -> usize {
        self.offsets.len()
    }

    pub fn level(&self, k: usize) -> &[GpuBar] {
        let start = self.offsets[k];
        let end = start + self.len0.div_ceil(1 << k);
        &self.data[start..end]
    }

    pub(crate) fn can_update_tail(&self, len: usize, from: usize) -> bool {
        can_patch(&self.offsets, self.data.len(), self.len0, len, from)
    }

    /// Patch a tail and its ancestors, or leave everything untouched if a rebuild is needed.
    pub fn update_tail(&mut self, bars: &[Bar], from: usize) -> Option<DirtyRanges> {
        if !self.can_update_tail(bars.len(), from) {
            return None;
        }
        self.len0 = bars.len();
        let mut dirty = Vec::new();
        for k in 0..self.levels() {
            let off = self.offsets[k];
            let n = bars.len().div_ceil(1 << k);
            let first = from >> k;
            for j in first..n {
                self.data[off + j] = if k == 0 {
                    let b = &bars[j];
                    GpuBar {
                        x: j as f32,
                        w: 1.0,
                        open: b.open as f32,
                        high: b.high as f32,
                        low: b.low as f32,
                        close: b.close as f32,
                        volume: b.volume as f32,
                        _pad: 0.0,
                    }
                } else {
                    let prev = self.offsets[k - 1];
                    let a = self.data[prev + j * 2];
                    if j * 2 + 1 < bars.len().div_ceil(1 << (k - 1)) {
                        let b = self.data[prev + j * 2 + 1];
                        GpuBar {
                            x: a.x,
                            w: a.w + b.w,
                            open: a.open,
                            high: a.high.max(b.high),
                            low: a.low.min(b.low),
                            close: b.close,
                            volume: a.volume + b.volume,
                            _pad: 0.0,
                        }
                    } else {
                        a
                    }
                };
            }
            if first < n {
                dirty.push((off + first) * 32..(off + n) * 32);
            }
        }
        self.version += 1;
        self.dirty = dirty.clone();
        Some(dirty)
    }

    /// Absolute instance range in `data` covering level-0 indices `[lo, hi)` at level `k`.
    pub fn instance_range(&self, k: usize, lo: usize, hi: usize) -> (u32, u32) {
        let n = self.level(k).len();
        let a = (lo >> k).min(n);
        let b = hi.div_ceil(1 << k).min(n);
        let off = self.offsets[k];
        ((off + a) as u32, (off + b.max(a)) as u32)
    }

    /// Low/high and max volume over level-0 range `[lo, hi)` using level `k`.
    pub fn extent(&self, k: usize, lo: usize, hi: usize) -> Option<(f64, f64, f64)> {
        let (a, b) = self.instance_range(k, lo, hi);
        let slice = &self.data[a as usize..b as usize];
        if slice.is_empty() {
            return None;
        }
        let (mut l, mut h, mut v) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f32);
        for g in slice {
            l = l.min(g.low);
            h = h.max(g.high);
            v = v.max(g.volume);
        }
        Some((l as f64, h as f64, v as f64))
    }
}

/// A value series (indicator, line chart) decimated per level; each point is
/// `(level-0 index of the point's bar centre, value)`. NaN values break the line.
#[derive(Debug, Clone, Default)]
pub struct LinePyramid {
    markers: bool,
    pub data: Vec<[f32; 2]>,
    pub offsets: Vec<usize>,
    pub len0: usize,
    pub dirty: DirtyRanges,
    pub version: u64,
}

impl LinePyramid {
    /// Keeps the later point of each pair (its value is the group's close).
    pub fn build(values: &[f64]) -> Self {
        Self::reduce(values, false, |_, later| later)
    }

    /// For sparse markers: a pair whose later point is blank keeps the earlier one, so
    /// markers survive zooming out.
    pub fn build_markers(values: &[f64]) -> Self {
        Self::reduce(values, true, |earlier, later| if later[1].is_nan() { earlier } else { later })
    }

    fn reduce(values: &[f64], markers: bool, merge: impl Fn([f32; 2], [f32; 2]) -> [f32; 2]) -> Self {
        let mut data: Vec<[f32; 2]> = values.iter().enumerate().map(|(i, v)| [i as f32 + 0.5, *v as f32]).collect();
        let mut offsets = vec![0];
        let (mut start, mut n) = (0, values.len());
        while n > 1 {
            let next_start = data.len();
            for j in (0..n).step_by(2) {
                let p = if j + 1 < n { merge(data[start + j], data[start + j + 1]) } else { data[start + j] };
                data.push(p);
            }
            offsets.push(next_start);
            start = next_start;
            n = n.div_ceil(2);
        }
        let data = padded(data, &mut offsets, values.len());
        Self { markers, data, offsets, len0: values.len(), dirty: Vec::new(), version: 1 }
    }

    pub(crate) fn can_update_tail(&self, len: usize, from: usize) -> bool {
        can_patch(&self.offsets, self.data.len(), self.len0, len, from)
    }

    pub fn update_tail(&mut self, values: &[f64], from: usize) -> Option<DirtyRanges> {
        self.update_values(values.len(), from, |i| values[i])
    }

    pub fn update_closes(&mut self, bars: &[Bar], from: usize) -> Option<DirtyRanges> {
        self.update_values(bars.len(), from, |i| bars[i].close)
    }

    fn update_values(&mut self, len: usize, from: usize, value: impl Fn(usize) -> f64) -> Option<DirtyRanges> {
        if !can_patch(&self.offsets, self.data.len(), self.len0, len, from) {
            return None;
        }
        self.len0 = len;
        let mut dirty = Vec::new();
        for k in 0..self.levels() {
            let off = self.offsets[k];
            let n = len.div_ceil(1 << k);
            let first = from >> k;
            for j in first..n {
                self.data[off + j] = if k == 0 {
                    [j as f32 + 0.5, value(j) as f32]
                } else {
                    let prev = self.offsets[k - 1];
                    let last = (j * 2 + 1).min(len.div_ceil(1 << (k - 1)) - 1);
                    let later = self.data[prev + last];
                    if self.markers && later[1].is_nan() { self.data[prev + j * 2] } else { later }
                };
            }
            if first < n {
                dirty.push((off + first) * 8..(off + n) * 8);
            }
        }
        self.version += 1;
        self.dirty = dirty.clone();
        Some(dirty)
    }

    pub fn levels(&self) -> usize {
        self.offsets.len()
    }

    pub fn level_len(&self, k: usize) -> usize {
        self.len0.div_ceil(1 << k)
    }

    /// Absolute segment-instance range: segment `i` joins points `i` and `i+1`.
    /// One extra point on each side keeps lines continuous to the plot edges.
    pub fn segment_range(&self, k: usize, lo: usize, hi: usize) -> (u32, u32) {
        let n = self.level_len(k);
        if n < 2 {
            return (0, 0);
        }
        let a = (lo >> k).saturating_sub(1).min(n - 1);
        let b = (hi.div_ceil(1 << k) + 1).min(n - 1);
        let off = self.offsets[k];
        ((off + a) as u32, (off + b.max(a)) as u32)
    }

    /// Finite min/max over level-0 range `[lo, hi)` at level `k`.
    pub fn extent(&self, k: usize, lo: usize, hi: usize) -> Option<(f64, f64)> {
        let n = self.level_len(k);
        let a = (lo >> k).min(n);
        let b = hi.div_ceil(1 << k).min(n);
        let off = self.offsets[k];
        let mut out: Option<(f64, f64)> = None;
        for p in &self.data[off + a..off + b] {
            let v = p[1] as f64;
            if v.is_finite() {
                out = Some(out.map_or((v, v), |(l, h)| (l.min(v), h.max(v))));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(n: usize) -> Vec<Bar> {
        (0..n).map(|i| Bar::new(i as i64, i as f64, i as f64 + 2.0, i as f64 - 1.0, i as f64 + 1.0, 1.0)).collect()
    }

    #[test]
    fn pyramid_levels_merge_ohlcv() {
        let p = BarPyramid::build(&bars(5));
        assert_eq!(p.levels(), 4); // 5, 3, 2, 1
        let l1 = p.level(1);
        assert_eq!(l1.len(), 3);
        assert_eq!(
            l1[0],
            GpuBar { x: 0.0, w: 2.0, open: 0.0, high: 3.0, low: -1.0, close: 2.0, volume: 2.0, _pad: 0.0 }
        );
        assert_eq!(l1[2].x, 4.0);
        assert_eq!(l1[2].w, 1.0);
        let top = p.level(3);
        assert_eq!(top.len(), 1);
        assert_eq!((top[0].w, top[0].low, top[0].high, top[0].volume), (5.0, -1.0, 6.0, 5.0));
    }

    #[test]
    fn level_choice_bounds_instance_count() {
        let p = BarPyramid::build(&bars(1 << 20));
        let width = 1600.0;
        for bar_px in [0.0015, 0.01, 0.3, 1.0, 1.99, 2.0, 25.0] {
            let k = level_for(bar_px, p.levels());
            let vis = (width / bar_px) as usize;
            let (a, b) = p.instance_range(k, 0, vis.min(p.len0));
            assert!((b - a) as f64 <= width / MIN_INSTANCE_PX + 2.0 || k == 0, "bar_px {bar_px}: {}", b - a);
            if bar_px >= 2.0 {
                assert_eq!(k, 0);
            }
        }
    }

    #[test]
    fn extent_matches_brute_force() {
        let p = BarPyramid::build(&bars(1000));
        let (l, h, _) = p.extent(0, 100, 200).unwrap();
        assert_eq!((l, h), (99.0, 201.0));
        // Coarser levels may widen the range slightly but always contain it.
        let (l3, h3, _) = p.extent(3, 100, 200).unwrap();
        assert!(l3 <= 99.0 && h3 >= 201.0);
    }

    #[test]
    fn line_pyramid() {
        let mut v: Vec<f64> = (0..9).map(|i| i as f64).collect();
        v[0] = f64::NAN;
        let p = LinePyramid::build(&v);
        assert_eq!(p.level_len(1), 5);
        assert_eq!(p.data[p.offsets[1]], [1.5, 1.0]);
        assert_eq!(p.extent(0, 0, 9), Some((1.0, 8.0)));
        let (a, b) = p.segment_range(0, 3, 6);
        assert_eq!((a, b), (2, 7));
        assert_eq!(p.segment_range(0, 0, 0), (0, 1));
    }

    #[test]
    fn seeded_tail_patches_match_fresh_pyramids_and_bytes() {
        let mut seed = 17u64;
        let mut rebuilds = 0;
        for n in [1, 2, 3, 1000, 100000] {
            let mut bars = bars(n);
            let mut p = BarPyramid::build(&bars);
            let mut line = LinePyramid::build(&bars.iter().map(|b| b.close).collect::<Vec<_>>());
            for _ in 0..80 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let from = bars.len() - 1;
                let b = bars.last_mut().unwrap();
                b.close = (seed >> 32) as f64 / 12345.;
                b.high = b.high.max(b.close);
                b.low = b.low.min(b.close);
                b.volume += 0.25;
                if seed & (1 << 40) != 0 {
                    bars.push(Bar::new(bars.len() as i64, 1., 3., 0., 2., 1.));
                }
                let mut old = bytemuck::cast_slice::<GpuBar, u8>(&p.data).to_vec();
                let mut old_line = bytemuck::cast_slice::<[f32; 2], u8>(&line.data).to_vec();
                if let Some(ranges) = p.update_tail(&bars, from) {
                    let bytes = bytemuck::cast_slice::<GpuBar, u8>(&p.data);
                    for r in ranges {
                        old[r.clone()].copy_from_slice(&bytes[r]);
                    }
                    assert_eq!(old, bytes);
                    let ranges = line.update_closes(&bars, from).unwrap();
                    let bytes = bytemuck::cast_slice::<[f32; 2], u8>(&line.data);
                    for r in ranges {
                        old_line[r.clone()].copy_from_slice(&bytes[r]);
                    }
                    assert_eq!(old_line, bytes);
                } else {
                    rebuilds += 1;
                    p = BarPyramid::build(&bars);
                    line = LinePyramid::build(&bars.iter().map(|b| b.close).collect::<Vec<_>>());
                }
                let fresh = BarPyramid::build(&bars);
                let fresh_line = LinePyramid::build(&bars.iter().map(|b| b.close).collect::<Vec<_>>());
                for k in 0..fresh.levels() {
                    assert_eq!(p.level(k), fresh.level(k));
                    let (a, b) = (line.offsets[k], fresh_line.offsets[k]);
                    assert_eq!(&line.data[a..a + line.level_len(k)], &fresh_line.data[b..b + fresh_line.level_len(k)]);
                }
            }
            if n > 1024 {
                assert!(p.update_tail(&bars, 0).is_none());
                assert!(line.update_closes(&bars, 0).is_none());
            }
        }
        assert!(rebuilds > 0);
    }

    #[test]
    fn marker_pyramid_keeps_sparse_points() {
        let mut v = vec![f64::NAN; 16];
        v[2] = 5.0;
        v[9] = 7.0;
        let lines = LinePyramid::build(&v);
        assert_eq!(lines.extent(1, 0, 16), Some((7.0, 7.0)), "lines keep the later point");
        let marks = LinePyramid::build_markers(&v);
        // Both survive every level where they fall in different groups, at their own bars.
        for k in 0..marks.levels() - 1 {
            assert_eq!(marks.extent(k, 0, 16), Some((5.0, 7.0)), "level {k}");
            let level = &marks.data[marks.offsets[k]..marks.offsets[k + 1]];
            assert!(level.contains(&[2.5, 5.0]) && level.contains(&[9.5, 7.0]), "level {k}");
        }
        assert_eq!(marks.data[marks.offsets[marks.levels() - 1]], [9.5, 7.0]);
    }
}
