//! Chart study instances. Calculation dependencies, pane membership and scale
//! policies are independent. Old registry labels remain valid saved studies.

use serde::{Deserialize, Serialize};

use super::registry::{Input, Output, Spec};
use crate::Bar;

/// Extra history for study initialization: four times the largest numeric
/// parameter (conservatively treated as a length), bounded to 300..=5,000 bars.
pub fn warmup_bars(specs: &[Spec]) -> usize {
    let longest = specs.iter().flat_map(|spec| spec.params.iter()).copied().fold(0.0, f64::max);
    (4.0 * longest).ceil().clamp(300.0, 5_000.0) as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlotStyle {
    Line,
    Dots,
    Steps,
    Histogram,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputStyle {
    pub visible: bool,
    pub color: Option<[u8; 3]>,
    pub width: f32,
    pub plot: Option<PlotStyle>,
}
impl Default for OutputStyle {
    fn default() -> Self {
        Self { visible: true, color: None, width: 1.3, plot: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BandFill {
    pub upper: FillEdge,
    pub lower: FillEdge,
    pub color: [u8; 3],
    pub opacity: u8,
}

/// An absent study ID addresses an output of the declaring study.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "SavedFillEdge")]
pub struct FillEdge {
    pub study: Option<u32>,
    pub output: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SavedFillEdge {
    Legacy(String),
    Edge {
        #[serde(default)]
        study: Option<u32>,
        output: String,
    },
}
impl From<SavedFillEdge> for FillEdge {
    fn from(value: SavedFillEdge) -> Self {
        match value {
            SavedFillEdge::Legacy(output) => Self { study: None, output },
            SavedFillEdge::Edge { study, output } => Self { study, output },
        }
    }
}
impl From<String> for FillEdge {
    fn from(output: String) -> Self {
        Self { study: None, output }
    }
}
impl From<&str> for FillEdge {
    fn from(output: &str) -> Self {
        output.to_string().into()
    }
}

/// Validate display references independently of calculation dependencies. A bad
/// fill must never suppress either study's lines or its calculation consumers.
pub fn validate_fill_edge(specs: &[Spec], owner: usize, edge: &FillEdge) -> Result<(), String> {
    let Some(id) = edge.study else { return Ok(()) };
    let target = specs.iter().position(|s| s.chart.as_ref().is_some_and(|c| c.id == id));
    let label = target.map_or_else(|| id.to_string(), |i| specs[i].label());
    let target = target
        .filter(|&i| specs[i].chart.as_ref().is_some_and(|c| c.visible))
        .ok_or_else(|| format!("Fill target {label} is not available"))?;
    if pane_key(specs, owner)? != pane_key(specs, target)? {
        return Err("Fill target must be on the same pane".into());
    }
    let owner_scale = specs[owner].chart.as_ref().map_or(Scale::Shared, |c| c.scale);
    let target_scale = specs[target].chart.as_ref().map_or(Scale::Shared, |c| c.scale);
    if owner_scale == Scale::Independent || target_scale == Scale::Independent || owner_scale != target_scale {
        return Err("Fill target must share the price/pane scale (not an Independent scale)".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Source {
    #[default]
    Bars,
    Open,
    High,
    Low,
    Close,
    Volume,
    Hlc3,
    Output {
        study: u32,
        output: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pane {
    Price,
    Own,
    With(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum Scale {
    #[default]
    Shared,
    Independent,
    Fixed {
        lo: f64,
        hi: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub id: u32,
    #[serde(default)]
    pub source: Source,
    pub pane: Pane,
    #[serde(default)]
    pub scale: Scale,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default = "height")]
    pub height: f32,
    #[serde(default)]
    pub formula: super::formula::Formula,
    /// Display-only shift. Calculation consumers always see undisplaced values.
    #[serde(default)]
    pub displacement: i16,
    #[serde(default)]
    pub outputs: std::collections::BTreeMap<String, OutputStyle>,
    #[serde(default)]
    pub fill: Option<BandFill>,
    #[serde(default)]
    pub timeframe: Option<crate::Timeframe>,
    #[serde(default)]
    pub expression: Option<String>,
    #[serde(default)]
    pub scan_plot: bool,
}

fn yes() -> bool {
    true
}
fn height() -> f32 {
    120.0
}

impl Settings {
    /// Normalize display settings from untrusted saves. Unavailable external
    /// references stay intact so the editor can explain them instead of silently
    /// retargeting a fill to a different study.
    pub fn sanitized(mut self) -> Self {
        if let Some(fill) = &mut self.fill {
            for edge in [&mut fill.upper, &mut fill.lower] {
                if edge.study == Some(self.id) {
                    edge.study = None;
                }
            }
        }
        self
    }

    /// Standalone presets cannot retain instance IDs. Reset external edges to
    /// an available local port, or remove the fill if there are no local ports.
    pub fn reset_fill_targets(&mut self, output: Option<&str>) {
        if output.is_none()
            && self.fill.as_ref().is_some_and(|fill| fill.upper.study.is_some() || fill.lower.study.is_some())
        {
            self.fill = None;
            return;
        }
        if let (Some(fill), Some(output)) = (&mut self.fill, output) {
            for edge in [&mut fill.upper, &mut fill.lower] {
                if edge.study.is_some() {
                    *edge = output.into();
                }
            }
        }
    }

    pub fn new(id: u32, spec: &Spec) -> Self {
        Self {
            id,
            source: Source::Bars,
            pane: if spec.def().is_some_and(|d| d.pane) { Pane::Own } else { Pane::Price },
            scale: Scale::Shared,
            visible: true,
            color: None,
            height: height(),
            formula: Default::default(),
            displacement: 0,
            outputs: Default::default(),
            fill: None,
            timeframe: None,
            scan_plot: false,
            expression: (spec.code == "PCF").then(|| "C".into()),
        }
    }
}

/// Give legacy and newly added specs unique IDs without renumbering existing ports.
pub fn assign_ids(specs: &mut [Spec]) {
    for spec in specs.iter_mut() {
        spec.chart = spec.chart.take().map(Settings::sanitized);
    }
    let mut reserved: std::collections::HashSet<_> =
        specs.iter().filter_map(|s| s.chart.as_ref().map(|c| c.id)).filter(|id| *id != 0).collect();
    for c in specs.iter().filter_map(|s| s.chart.as_ref()) {
        if let Some(fill) = &c.fill {
            reserved.extend([fill.upper.study, fill.lower.study].into_iter().flatten());
        }
    }
    let mut used = std::collections::HashSet::new();
    for spec in specs.iter_mut() {
        if let Some(c) = &spec.chart
            && c.id != 0
            && used.insert(c.id)
        {
            continue;
        }
        let id = (1..=u32::MAX).find(|id| !reserved.contains(id)).expect("study ID space");
        reserved.insert(id);
        used.insert(id);
        if let Some(c) = &mut spec.chart {
            c.id = id;
        } else {
            spec.chart = Some(Settings::new(id, spec));
        }
    }
}

/// `None` identifies the price pane. Pane links may chain, but must not cycle.
pub fn pane_key(specs: &[Spec], index: usize) -> Result<Option<u32>, String> {
    let mut at = index;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(at) {
            return Err("Pane links form a cycle".into());
        }
        let spec = &specs[at];
        let Some(c) = &spec.chart else {
            return Ok(spec.def().is_some_and(|d| d.pane).then_some(at as u32 + 1));
        };
        match c.pane {
            Pane::Price => return Ok(None),
            Pane::Own => return Ok(Some(c.id)),
            Pane::With(id) => {
                at = specs
                    .iter()
                    .position(|s| s.chart.as_ref().is_some_and(|c| c.id == id))
                    .ok_or_else(|| "The selected pane no longer exists".to_string())?
            }
        }
    }
}

/// Only these scalar-input studies currently accept a selectable input. Other
/// registry studies retain their original OHLCV semantics, never fabricated bars.
pub fn accepts_source(code: &str) -> bool {
    matches!(
        code,
        "SMA"
            | "EMA"
            | "WMA"
            | "HMA"
            | "RMA"
            | "BB"
            | "BBW"
            | "PCTB"
            | "ENV"
            | "RSI"
            | "MACD"
            | "DEMA"
            | "TEMA"
            | "TRIMA"
            | "KAMA"
            | "ZLEMA"
            | "ALMA"
            | "VWMA"
            | "EVWMA"
            | "T3"
            | "MCGINLEY"
            | "VIDYA"
            | "SUPERSMOOTHER"
            | "DECYCLER"
            | "MAMA"
            | "ITREND"
            | "LSMA"
            | "GMMA"
            | "STDEV"
            | "VAR"
            | "ZSCORE"
            | "SLOPE"
            | "R2"
            | "STDERR"
            | "TSF"
            | "MEDIAN"
            | "PRANK"
    )
}

/// Whether adding this source edge would be acyclic. Evaluation also validates
/// imported graphs; the editor uses this to omit invalid choices in advance.
pub fn can_use_source(specs: &[Spec], consumer: u32, mut parent: u32) -> bool {
    let mut seen = std::collections::HashSet::new();
    loop {
        if parent == consumer || !seen.insert(parent) {
            return false;
        }
        let Some(settings) = specs.iter().filter_map(|s| s.chart.as_ref()).find(|c| c.id == parent) else {
            return false;
        };
        match settings.source {
            Source::Output { study, .. } => parent = study,
            _ => return true,
        }
    }
}

/// Independently fetched study histories, including load errors.
pub type StudyHistories = std::collections::HashMap<crate::Timeframe, Result<std::sync::Arc<Vec<crate::Bar>>, String>>;

/// Evaluate in dependency order, retaining independent successes when a node fails.
/// Missing output ports, cycles and displaced inputs are explicit errors.
pub fn evaluate(specs: &[Spec], input: &Input<'_>) -> Vec<Result<Vec<Output>, String>> {
    evaluate_with_session(specs, input, crate::session::Session::Provider)
}

pub fn evaluate_with_session(
    specs: &[Spec],
    input: &Input<'_>,
    session: crate::session::Session,
) -> Vec<Result<Vec<Output>, String>> {
    evaluate_with_histories(specs, input, session, &StudyHistories::new())
}

/// Evaluate using independent histories where available, aligned at bar close.
pub fn evaluate_with_histories(
    specs: &[Spec],
    input: &Input<'_>,
    session: crate::session::Session,
    histories: &StudyHistories,
) -> Vec<Result<Vec<Output>, String>> {
    if input.session != session {
        super::multiframe::evaluate(specs, &Input::new(input.bars, input.tf).with_session(session), session, histories)
    } else {
        super::multiframe::evaluate(specs, input, session, histories)
    }
}

pub(super) fn evaluate_native(specs: &[Spec], input: &Input<'_>) -> Vec<Result<Vec<Output>, String>> {
    fn node(
        index: usize,
        specs: &[Spec],
        input: &Input<'_>,
        visiting: &mut [bool],
        done: &mut [Option<Result<Vec<Output>, String>>],
    ) -> Result<Vec<Output>, String> {
        if let Some(value) = &done[index] {
            return value.clone();
        }
        if visiting[index] {
            return Err("Indicator sources form a cycle".into());
        }
        visiting[index] = true;
        let spec = &specs[index];
        let source = spec.chart.as_ref().map(|c| &c.source).unwrap_or(&Source::Bars);
        let result = (|| {
            pane_key(specs, index)?;
            if let Some(Settings { scale: Scale::Fixed { lo, hi }, .. }) = &spec.chart
                && (!lo.is_finite() || !hi.is_finite() || lo >= hi)
            {
                return Err("Fixed scale minimum must be smaller than its maximum".into());
            }
            if spec.def().is_none() {
                return Err(format!("Unknown indicator: {}", spec.code));
            }
            if matches!(source, Source::Bars) {
                return super::formula::compute(spec, input);
            }
            if !accepts_source(&spec.code) {
                return Err("This indicator requires its original OHLCV input".into());
            }
            let values = match source {
                Source::Output { study, output } => {
                    let parent = specs
                        .iter()
                        .position(|s| s.chart.as_ref().is_some_and(|c| c.id == *study))
                        .ok_or_else(|| "The source indicator no longer exists".to_string())?;
                    let outputs = node(parent, specs, input, visiting, done)?;
                    let values = outputs
                        .into_iter()
                        .find(|o| o.name == output)
                        .ok_or_else(|| format!("Source output {output} is unavailable"))?
                        .values;
                    if values.len() != input.bars.len() {
                        return Err("Displaced source outputs are not supported yet".into());
                    }
                    values
                }
                _ => input
                    .bars
                    .iter()
                    .map(|b| match source {
                        Source::Open => b.open,
                        Source::High => b.high,
                        Source::Low => b.low,
                        Source::Volume => b.volume,
                        Source::Hlc3 => (b.high + b.low + b.close) / 3.0,
                        _ => b.close,
                    })
                    .collect(),
            };
            // This adapter is restricted to scalar calculations above. Timestamps
            // and volume remain intact; scalar values feed the close-based kernels.
            let bars: Vec<Bar> = input
                .bars
                .iter()
                .zip(values)
                .map(|(b, value)| {
                    let mut b = *b;
                    b.close = value;
                    b
                })
                .collect();
            super::formula::compute(spec, &Input::new(&bars, input.tf).with_session(input.session))
        })()
        .and_then(|outputs| {
            if let Some(settings) = &spec.chart {
                if settings.outputs.values().any(|style| !style.width.is_finite() || !(0.5..=8.).contains(&style.width))
                {
                    return Err("Output stroke width must be between 0.5 and 8".into());
                }
                if let Some(fill) = &settings.fill
                    && [&fill.upper, &fill.lower]
                        .iter()
                        .any(|edge| edge.study.is_none() && !outputs.iter().any(|o| o.name == edge.output))
                {
                    return Err("Band fill refers to an unavailable output".into());
                }
            }
            Ok(outputs)
        });
        visiting[index] = false;
        done[index] = Some(result.clone());
        result
    }
    let mut done = vec![None; specs.len()];
    let mut visiting = vec![false; specs.len()];
    (0..specs.len()).map(|i| node(i, specs, input, &mut visiting, &mut done)).collect()
}

/// Streaming kernels retain a checkpoint immediately before the mutable last bar.
/// `from` must include every changed bar; callers must pass zero after a history
/// replacement, prepend or an untracked rewrite. Earlier edits restart the kernel.
pub struct StreamingStudy {
    spec: Spec,
    tf: crate::Timeframe,
    session: crate::session::Session,
    params: Vec<f64>,
    checkpoint: StreamState,
    checkpoint_at: usize,
    pub outputs: Vec<Output>,
}

#[derive(Clone, Default)]
struct SmoothState {
    seen: usize,
    acc: f64,
}

impl SmoothState {
    fn push(&mut self, v: f64, n: usize, alpha: f64) -> f64 {
        if n == 0 || !v.is_finite() {
            return f64::NAN;
        }
        if self.seen < n {
            self.acc += v;
            self.seen += 1;
            if self.seen < n {
                return f64::NAN;
            }
            self.acc /= n as f64;
        } else {
            self.acc = alpha * v + (1.0 - alpha) * self.acc;
        }
        self.acc
    }
}

#[derive(Clone, Default)]
struct StreamState {
    smooth: [SmoothState; 3],
    reference: Option<f64>,
    sum: f64,
    squares: f64,
    bad: usize,
    session: Option<i64>,
    volume: f64,
}

impl StreamingStudy {
    pub fn new(spec: &Spec, tf: crate::Timeframe, session: crate::session::Session) -> Option<Self> {
        if !matches!(spec.code.as_str(), "EMA" | "SMA" | "RSI" | "MACD" | "BB" | "ATR" | "VWAP")
            || spec.chart.as_ref().is_some_and(|c| {
                matches!(c.source, Source::Output { .. })
                    || (c.source != Source::Bars && matches!(spec.code.as_str(), "ATR" | "VWAP"))
                    || c.formula != super::formula::Formula::default()
                    || c.displacement != 0
                    || c.timeframe.is_some()
                    || c.fill.is_some()
                    || matches!(c.pane, Pane::With(_))
                    || matches!(c.scale, Scale::Fixed { lo, hi } if !lo.is_finite() || !hi.is_finite() || lo >= hi)
                    || c.outputs.values().any(|s| !s.width.is_finite() || !(0.5..=8.).contains(&s.width))
            })
        {
            return None;
        }
        let params = spec.def()?.normalize(&spec.params);
        let outputs = spec.compute(&Input::new(&[], tf).with_session(session));
        Some(Self {
            spec: spec.clone(),
            tf,
            session,
            params,
            checkpoint: StreamState::default(),
            checkpoint_at: 0,
            outputs,
        })
    }

    /// Returns the first recomputed output index. No prefix values are copied.
    pub fn update(&mut self, bars: &[Bar], from: usize) -> usize {
        let from = if from < self.checkpoint_at || bars.len() < self.checkpoint_at {
            self.checkpoint = StreamState::default();
            self.checkpoint_at = 0;
            0
        } else {
            self.checkpoint_at
        };
        let mut state = self.checkpoint.clone();
        for o in &mut self.outputs {
            o.values.truncate(from);
        }
        for i in from..bars.len() {
            if i + 1 == bars.len() {
                self.checkpoint = state.clone();
                self.checkpoint_at = i;
            }
            let values = self.push(&mut state, bars, i);
            for (o, v) in self.outputs.iter_mut().zip(values) {
                o.values.push(v);
            }
        }
        from
    }

    fn scalar(&self, b: &Bar) -> f64 {
        match self.spec.chart.as_ref().map(|c| &c.source) {
            Some(Source::Open) => b.open,
            Some(Source::High) => b.high,
            Some(Source::Low) => b.low,
            Some(Source::Volume) => b.volume,
            Some(Source::Hlc3) => (b.high + b.low + b.close) / 3.,
            _ => b.close,
        }
    }

    fn push(&self, s: &mut StreamState, bars: &[Bar], i: usize) -> Vec<f64> {
        let b = bars[i];
        let close = self.scalar(&b);
        let n = self.params.first().copied().unwrap_or(1.) as usize;
        let ema = |s: &mut SmoothState, v: f64, n: usize| s.push(v, n, 2. / (n as f64 + 1.));
        match self.spec.code.as_str() {
            "EMA" => vec![ema(&mut s.smooth[0], close, n)],
            "ATR" => {
                let p = i.checked_sub(1).map_or(f64::NAN, |j| bars[j].close);
                let tr = if p.is_nan() { b.high - b.low } else { b.high.max(p) - b.low.min(p) };
                vec![s.smooth[0].push(tr, n, 1. / n as f64)]
            }
            "RSI" => {
                let d = i.checked_sub(1).map_or(f64::NAN, |j| close - self.scalar(&bars[j]));
                let (g, l) = if d.is_finite() { (d.max(0.), (-d).max(0.)) } else { (f64::NAN, f64::NAN) };
                let g = s.smooth[0].push(g, n, 1. / n as f64);
                let l = s.smooth[1].push(l, n, 1. / n as f64);
                vec![if !g.is_finite() || !l.is_finite() {
                    f64::NAN
                } else if l == 0. {
                    if g == 0. { 50. } else { 100. }
                } else {
                    100. - 100. / (1. + g / l)
                }]
            }
            "MACD" => {
                let f = ema(&mut s.smooth[0], close, n);
                let slow = ema(&mut s.smooth[1], close, self.params[1] as usize);
                let m = f - slow;
                let signal = ema(&mut s.smooth[2], m, self.params[2] as usize);
                vec![m, signal, m - signal]
            }
            "VWAP" => {
                let ts = if self.session != crate::session::Session::Provider && self.tf.is_intraday() {
                    crate::session::Zone::NewYork.local(b.ts).and_utc().timestamp()
                } else {
                    b.ts
                };
                let key = super::levels::session_key(self.tf)(ts);
                if s.session != Some(key) {
                    s.session = Some(key);
                    s.sum = 0.;
                    s.volume = 0.;
                }
                s.sum += b.hlc3() * b.volume;
                s.volume += b.volume;
                vec![if s.volume > 0. { s.sum / s.volume } else { b.hlc3() }]
            }
            _ => {
                // Same centred rolling sum and periodic re-sum order as stats::windows.
                if s.reference.is_none() && close.is_finite() {
                    s.reference = Some(close);
                }
                let r = s.reference.unwrap_or(0.);
                let y = |j: usize| {
                    let v = self.scalar(&bars[j]);
                    if v.is_finite() { v - r } else { 0. }
                };
                if !close.is_finite() {
                    s.bad += 1;
                }
                if i >= n {
                    if !self.scalar(&bars[i - n]).is_finite() {
                        s.bad -= 1;
                    }
                    let old = y(i - n);
                    s.sum -= old;
                    s.squares -= old * old;
                }
                let v = y(i);
                s.sum += v;
                s.squares += v * v;
                if i + 1 >= n && (i + 1).is_multiple_of(n) {
                    s.sum = 0.;
                    s.squares = 0.;
                    for j in i + 1 - n..=i {
                        let v = y(j);
                        s.sum += v;
                        s.squares += v * v;
                    }
                }
                let (middle, sd) = if i + 1 >= n && s.bad == 0 {
                    let k = n as f64;
                    let m = s.sum / k;
                    ((s.sum + r * k) / k, (s.squares / k - m * m).max(0.).sqrt())
                } else {
                    (f64::NAN, f64::NAN)
                };
                if self.spec.code == "BB" {
                    let width = self.params[1] * sd;
                    vec![middle + width, middle, middle - width]
                } else {
                    vec![middle]
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timeframe;

    #[test]
    fn legacy_band_fill_loads_and_cross_study_template_round_trips() {
        let legacy: BandFill =
            serde_json::from_str(r#"{"upper":"Upper","lower":"Lower","color":[1,2,3],"opacity":35}"#).unwrap();
        assert_eq!(legacy.upper, FillEdge { study: None, output: "Upper".into() });
        let mut specs = vec![Spec::new("EMA", &[]).unwrap(), Spec::new("BB", &[]).unwrap()];
        assign_ids(&mut specs);
        specs[0].chart.as_mut().unwrap().fill = Some(BandFill {
            upper: "EMA".into(),
            lower: FillEdge { study: Some(2), output: "Lower".into() },
            ..legacy
        });
        let json = serde_json::to_string(&specs).unwrap();
        let mut restored: Vec<Spec> = serde_json::from_str(&json).unwrap();
        assign_ids(&mut restored);
        assert_eq!(restored, specs);
    }

    #[test]
    fn fill_targets_require_visible_same_pane_shared_scale() {
        let mut specs = vec![Spec::new("EMA", &[]).unwrap(), Spec::new("BB", &[]).unwrap()];
        assign_ids(&mut specs);
        let edge = FillEdge { study: Some(2), output: "Lower".into() };
        assert_eq!(validate_fill_edge(&specs, 0, &edge), Ok(()));
        specs[1].chart.as_mut().unwrap().timeframe = Some(Timeframe::H1);
        assert_eq!(validate_fill_edge(&specs, 0, &edge), Ok(()));
        specs[1].chart.as_mut().unwrap().pane = Pane::Own;
        assert_eq!(validate_fill_edge(&specs, 0, &edge).unwrap_err(), "Fill target must be on the same pane");
        specs[1].chart.as_mut().unwrap().pane = Pane::Price;
        for index in [0, 1] {
            specs[index].chart.as_mut().unwrap().scale = Scale::Independent;
            assert_eq!(
                validate_fill_edge(&specs, 0, &edge).unwrap_err(),
                "Fill target must share the price/pane scale (not an Independent scale)"
            );
            specs[index].chart.as_mut().unwrap().scale = Scale::Shared;
        }
        specs[1].chart.as_mut().unwrap().visible = false;
        assert_eq!(
            validate_fill_edge(&specs, 0, &edge).unwrap_err(),
            format!("Fill target {} is not available", specs[1].label())
        );
        specs.pop();
        assert_eq!(validate_fill_edge(&specs, 0, &edge).unwrap_err(), "Fill target 2 is not available");
    }

    #[test]
    fn loaded_fill_settings_are_sanitized_without_retargeting_missing_ids() {
        let spec = Spec::new("EMA", &[]).unwrap();
        let mut settings = Settings::new(1, &spec);
        settings.fill = Some(BandFill {
            upper: FillEdge { study: Some(1), output: "EMA".into() },
            lower: FillEdge { study: Some(999), output: "Lower".into() },
            color: [1, 2, 3],
            opacity: 255,
        });
        let clean = settings.sanitized();
        let fill = clean.fill.unwrap();
        assert_eq!(fill.upper.study, None);
        assert_eq!(fill.lower.study, Some(999));
        assert_eq!(fill.opacity, 255);
    }

    #[test]
    fn warmup_scales_with_parameters_and_is_bounded() {
        assert_eq!(warmup_bars(&[]), 300);
        let defaults = [Spec::new("EMA", &[]).unwrap(), Spec::new("RSI", &[]).unwrap()];
        assert_eq!(warmup_bars(&defaults), 300);
        let ema = Spec::new("EMA", &[200.]).unwrap();
        assert_eq!(warmup_bars(std::slice::from_ref(&ema)), 800);
        let mut large = ema.clone();
        // Exercise the helper's cap independently of registry parameter clamping.
        large.params = vec![2_000.];
        assert_eq!(warmup_bars(&[ema, large]), 5_000);
    }

    #[test]
    fn session_vwap_does_not_reset_at_utc_midnight_inside_the_ny_evening() {
        let ts = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
        let bars = [
            Bar::new(ts("2026-01-05T23:30:00Z"), 10., 10., 10., 10., 1.),
            Bar::new(ts("2026-01-06T00:30:00Z"), 20., 20., 20., 20., 1.),
        ];
        let specs = [Spec::new("VWAP", &[]).unwrap()];
        let output =
            evaluate_with_session(&specs, &Input::new(&bars, Timeframe::H1), crate::session::Session::UsExtended)[0]
                .clone()
                .unwrap();
        assert_eq!(output[0].values[1], 15.);
        let utc = evaluate(&specs, &Input::new(&bars, Timeframe::H1))[0].clone().unwrap();
        assert_eq!(utc[0].values[1], 20.);
    }

    fn specs() -> Vec<Spec> {
        let mut specs = vec![Spec::new("RSI", &[3.0]).unwrap(), Spec::new("EMA", &[2.0]).unwrap()];
        assign_ids(&mut specs);
        specs[1].chart.as_mut().unwrap().source = Source::Output { study: 1, output: "RSI".into() };
        specs
    }

    #[test]
    fn dependencies_are_ordered_and_hidden_sources_still_compute() {
        let bars: Vec<_> = [1., 3., 2., 4., 3., 6., 4., 7.]
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar::new(i as i64, c, c, c, c, 1.))
            .collect();
        let input = Input::new(&bars, Timeframe::D1);
        let mut specs = specs();
        specs[0].chart.as_mut().unwrap().visible = false;
        specs.reverse();
        let got = evaluate(&specs, &input);
        let expected = super::super::ema(&super::super::rsi(&input.d.close, 3), 2);
        assert!(
            got[0].as_ref().unwrap()[0].values.iter().zip(expected).all(|(a, b)| a == &b || a.is_nan() && b.is_nan())
        );
    }

    #[test]
    fn missing_ports_cycles_and_pane_cycles_fail_explicitly() {
        let bars = [];
        let input = Input::new(&bars, Timeframe::D1);
        let mut specs = specs();
        assert!(!can_use_source(&specs, 1, 2), "RSI cannot consume its own EMA child");
        assert!(can_use_source(&specs, 2, 1));
        specs[0].chart.as_mut().unwrap().source = Source::Output { study: 2, output: "EMA".into() };
        assert!(evaluate(&specs, &input).iter().all(Result::is_err));
        specs[0].chart.as_mut().unwrap().source = Source::Bars;
        specs[1].chart.as_mut().unwrap().source = Source::Output { study: 1, output: "missing".into() };
        assert!(evaluate(&specs, &input)[1].is_err());
        specs[0].chart.as_mut().unwrap().pane = Pane::With(2);
        specs[1].chart.as_mut().unwrap().pane = Pane::With(1);
        assert!(pane_key(&specs, 0).is_err());
    }

    #[test]
    fn legacy_and_configured_studies_round_trip() {
        let mut legacy: Vec<Spec> = serde_json::from_str(r#"["EMA 20",{"Rsi":14}]"#).unwrap();
        assign_ids(&mut legacy);
        assert_eq!(legacy[0].chart.as_ref().unwrap().id, 1);
        legacy[1].chart.as_mut().unwrap().color = Some([20, 40, 60]);
        let json = serde_json::to_string(&legacy).unwrap();
        assert_eq!(legacy, serde_json::from_str::<Vec<Spec>>(&json).unwrap());
    }

    #[test]
    fn scalar_inputs_and_distinct_instances_do_not_change_price_input() {
        let bars: Vec<_> = (1..=10).map(|i| Bar::new(i, 10., 12., 8., 11., i as f64)).collect();
        let input = Input::new(&bars, Timeframe::D1);
        let mut specs = vec![Spec::new("BB", &[3., 2.]).unwrap(), Spec::new("BB", &[5., 1.]).unwrap()];
        assign_ids(&mut specs);
        specs[0].chart.as_mut().unwrap().source = Source::Volume;
        let outputs = evaluate(&specs, &input);
        assert_eq!(outputs[0].as_ref().unwrap()[1].values[2], 2.);
        assert_eq!(outputs[1].as_ref().unwrap()[1].values[4], 11.);
        assert_ne!(specs[0].chart.as_ref().unwrap().id, specs[1].chart.as_ref().unwrap().id);
        specs[0].chart.as_mut().unwrap().scale = Scale::Fixed { lo: 100., hi: 0. };
        assert!(evaluate(&specs, &input)[0].is_err());
    }

    #[test]
    fn missing_sources_and_unsupported_scalar_consumers_fail() {
        let mut specs = specs();
        let bars = [];
        let input = Input::new(&bars, Timeframe::D1);
        specs.remove(0);
        assert!(evaluate(&specs, &input)[0].is_err());
        let mut atr = Spec::new("ATR", &[]).unwrap();
        atr.chart = Some(Settings::new(3, &atr));
        atr.chart.as_mut().unwrap().source = Source::Volume;
        assert!(evaluate(&[atr], &input)[0].is_err());
    }

    #[test]
    fn migrating_legacy_entries_cannot_steal_existing_port_ids() {
        let configured = specs();
        let mut mixed = vec![Spec::new("BB", &[]).unwrap(), configured[0].clone(), configured[1].clone()];
        assign_ids(&mut mixed);
        assert_eq!(mixed[0].chart.as_ref().unwrap().id, 3);
        assert_eq!(mixed[1].chart.as_ref().unwrap().id, 1);
        assert_eq!(mixed[2].chart.as_ref().unwrap().source, Source::Output { study: 1, output: "RSI".into() });
    }

    #[test]
    fn fill_targets_must_not_rebind_when_a_new_study_reuses_a_dangling_id() {
        let mut specs = vec![Spec::new("EMA", &[]).unwrap()];
        assign_ids(&mut specs);
        let edge = FillEdge { study: Some(2), output: "Lower".into() };
        specs[0].chart.as_mut().unwrap().fill =
            Some(BandFill { upper: "EMA".into(), lower: edge.clone(), color: [1, 2, 3], opacity: 35 });
        // Settings::sanitized keeps this dangling target so the editor can explain it.
        assert_eq!(validate_fill_edge(&specs, 0, &edge).unwrap_err(), "Fill target 2 is not available");
        specs.push(Spec::new("BB", &[]).unwrap());
        assign_ids(&mut specs);
        let added = specs[1].chart.as_ref().unwrap().id;
        // The new study must not absorb the referenced ID: the edge has to stay dangling
        // (or be dropped) instead of silently pointing at a different study.
        match specs[0].chart.as_ref().unwrap().fill.as_ref() {
            None => {}
            Some(fill) => assert!(
                fill.lower.study != Some(added),
                "the new study took id {added} and the dangling fill edge silently rebinds to it"
            ),
        }
    }

    #[test]
    fn display_displacement_cannot_introduce_lookahead_into_children() {
        let bars: Vec<_> = (0..20).map(|i| Bar::new(i, i as f64, i as f64, i as f64, (i * i) as f64, 1.)).collect();
        let mut specs = vec![Spec::new("SMA", &[2.]).unwrap(), Spec::new("EMA", &[2.]).unwrap()];
        assign_ids(&mut specs);
        specs[1].chart.as_mut().unwrap().source = Source::Output { study: 1, output: "SMA".into() };
        let before = evaluate(&specs, &Input::new(&bars, Timeframe::D1));
        specs[0].chart.as_mut().unwrap().displacement = -5;
        let after = evaluate(&specs, &Input::new(&bars, Timeframe::D1));
        let short = evaluate(&specs, &Input::new(&bars[..12], Timeframe::D1));
        let a = &before[1].as_ref().unwrap()[0].values;
        let b = &after[1].as_ref().unwrap()[0].values;
        assert!(a.iter().zip(b).all(|(a, b)| a == b || a.is_nan() && b.is_nan()));
        assert!(b.iter().zip(&short[1].as_ref().unwrap()[0].values).all(|(a, b)| a == b || a.is_nan() && b.is_nan()));
        specs[0].chart.as_mut().unwrap().formula.bars_ago = 1;
        let lag = evaluate(&specs, &Input::new(&bars, Timeframe::D1));
        assert_ne!(lag[1].as_ref().unwrap()[0].values[10], b[10]);
    }

    #[test]
    fn streaming_edits_appends_and_checkpoint_invalidation_are_bit_exact() {
        let mut seed = 42u64;
        let mut random = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as f64 / u32::MAX as f64
        };
        for size in [1, 2, 3, 13, 14, 15, 19, 20, 21, 49, 50, 51, 99, 100, 101, 199, 200, 201, 1000, 100_000] {
            let original: Vec<_> = (0..size)
                .map(|i| {
                    let c = 100. + random() * 10.;
                    Bar::new(1_790_000_000 + i as i64 * 60, c, c + 1., c - 1., c, random() * 100.)
                })
                .collect();
            for code in ["EMA", "SMA", "RSI", "MACD", "BB", "ATR", "VWAP"] {
                let params: &[f64] = match code {
                    "EMA" => &[50.],
                    "SMA" => &[200.],
                    _ => &[],
                };
                let spec = Spec::new(code, params).unwrap();
                for session in [crate::session::Session::Provider, crate::session::Session::UsRegular] {
                    let mut bars = original.clone();
                    let mut stream = StreamingStudy::new(&spec, Timeframe::M1, session).unwrap();
                    stream.update(&bars, 0);
                    for tick in 0..24 {
                        let from = if tick == 23 { 0 } else { bars.len() - 1 };
                        bars[from].close += random() - 0.5;
                        bars[from].volume += random();
                        if tick % 3 == 0 {
                            let mut b = *bars.last().unwrap();
                            b.ts += 60;
                            bars.push(b);
                        }
                        let start = stream.update(&bars, from);
                        if from == 0 {
                            assert_eq!(start, 0);
                        }
                        let full = evaluate_with_session(
                            std::slice::from_ref(&spec),
                            &Input::new(&bars, Timeframe::M1),
                            session,
                        );
                        for (got, want) in stream.outputs.iter().zip(full[0].as_ref().unwrap()) {
                            assert_eq!(got.name, want.name);
                            assert_eq!(got.values.len(), want.values.len());
                            for (i, (a, b)) in got.values.iter().zip(&want.values).enumerate() {
                                assert!(
                                    a.to_bits() == b.to_bits() || a.is_nan() && b.is_nan(),
                                    "{code} size {size} tick {tick} output {} index {i}: {a} != {b}",
                                    got.name
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn streaming_scalar_sources_and_missing_values_match_full_evaluation() {
        for source in [Source::Open, Source::High, Source::Low, Source::Close, Source::Hlc3, Source::Volume] {
            for code in ["EMA", "SMA", "RSI", "MACD", "BB"] {
                let mut spec = Spec::new(code, &[]).unwrap();
                spec.chart = Some(Settings::new(1, &spec));
                spec.chart.as_mut().unwrap().source = source.clone();
                let mut stream = StreamingStudy::new(&spec, Timeframe::M1, crate::session::Session::Provider).unwrap();
                let mut bars = Vec::new();
                for i in 0..130 {
                    let c = if i % 17 == 0 { f64::NAN } else { 100. + (i as f64).sin() };
                    let from = bars.len().saturating_sub(1);
                    bars.push(Bar::new(i * 60, c, c + 1., c - 1., c, c * 10.));
                    stream.update(&bars, from);
                    let full = evaluate(std::slice::from_ref(&spec), &Input::new(&bars, Timeframe::M1));
                    for (got, want) in stream.outputs.iter().zip(full[0].as_ref().unwrap()) {
                        assert!(
                            got.values
                                .iter()
                                .zip(&want.values)
                                .all(|(a, b)| a.to_bits() == b.to_bits() || a.is_nan() && b.is_nan()),
                            "{code} {source:?} at {i}"
                        );
                    }
                }
                spec.chart.as_mut().unwrap().displacement = 1;
                assert!(StreamingStudy::new(&spec, Timeframe::M1, crate::session::Session::Provider).is_none());
            }
        }
    }
}
