//! A bounded, explicitly documented PCF subset. No dynamic code or future references.
use super::{registry::Input, *};

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Word(String),
    Op(String),
    Left,
    Right,
    Comma,
    End,
}
#[derive(Debug)]
enum Expr {
    Number(f64),
    Field(char, usize),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}
struct Parser {
    tokens: Vec<Token>,
    at: usize,
    nodes: usize,
}
fn lex(source: &str) -> Result<Vec<Token>, String> {
    if source.len() > 4096 {
        return Err("Formula exceeds 4096 bytes".into());
    }
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut out = Vec::new();
    while at < bytes.len() {
        let c = bytes[at];
        if c.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if c.is_ascii_digit() || c == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
            let start = at;
            at += 1;
            while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b'.') {
                at += 1;
            }
            if bytes.get(at).is_some_and(|c| matches!(c, b'e' | b'E')) {
                at += 1;
                if bytes.get(at).is_some_and(|c| matches!(c, b'+' | b'-')) {
                    at += 1;
                }
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
            }
            let n = source[start..at].parse::<f64>().map_err(|_| format!("Invalid number at byte {start}"))?;
            if !n.is_finite() {
                return Err("Numeric constants must be finite".into());
            }
            out.push(Token::Number(n));
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = at;
            at += 1;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'.')) {
                at += 1;
            }
            let word = source[start..at].to_ascii_uppercase();
            out.push(if matches!(word.as_str(), "AND" | "OR" | "NOT") { Token::Op(word) } else { Token::Word(word) });
        } else {
            at += 1;
            out.push(match c {
                b'(' => Token::Left,
                b')' => Token::Right,
                b',' => Token::Comma,
                b'+' | b'-' | b'*' | b'/' | b'^' | b'=' | b'<' | b'>' | b'!' => {
                    let mut op = (c as char).to_string();
                    if bytes.get(at).is_some_and(|next| {
                        *next == b'=' && matches!(c, b'=' | b'<' | b'>' | b'!') || c == b'<' && *next == b'>'
                    }) {
                        op.push(bytes[at] as char);
                        at += 1;
                    }
                    Token::Op(op)
                }
                _ => return Err(format!("Unsupported character at byte {}", at - 1)),
            });
        }
        if out.len() > 512 {
            return Err("Formula exceeds 512 tokens".into());
        }
    }
    out.push(Token::End);
    Ok(out)
}
impl Parser {
    fn take(&mut self) -> Token {
        let token = self.tokens[self.at].clone();
        if token != Token::End {
            self.at += 1;
        }
        token
    }
    fn expression(&mut self, min: u8, depth: usize) -> Result<Expr, String> {
        self.nodes += 1;
        if depth > 32 || self.nodes > 128 {
            return Err("Formula exceeds 32 nesting levels or 128 expressions".into());
        }
        let mut lhs = match self.take() {
            Token::Number(n) => Expr::Number(n),
            Token::Op(op) if matches!(op.as_str(), "+" | "-" | "NOT") => {
                Expr::Unary(op, Box::new(self.expression(6, depth + 1)?))
            }
            Token::Left => {
                let e = self.expression(0, depth + 1)?;
                if self.take() != Token::Right {
                    return Err("Expected closing parenthesis".into());
                }
                e
            }
            Token::Word(name) => {
                if self.tokens[self.at] == Token::Left {
                    self.take();
                    let mut args = Vec::new();
                    if self.tokens[self.at] != Token::Right {
                        loop {
                            args.push(self.expression(0, depth + 1)?);
                            if self.tokens[self.at] != Token::Comma {
                                break;
                            }
                            self.take();
                        }
                    }
                    if self.take() != Token::Right {
                        return Err("Expected closing parenthesis after function arguments".into());
                    }
                    Expr::Call(name, args)
                } else {
                    compact(&name)?
                }
            }
            _ => return Err("Expected a price field, number or function".into()),
        };
        while let Token::Op(op) = &self.tokens[self.at] {
            let precedence = match op.as_str() {
                "OR" => 1,
                "AND" => 2,
                "=" | "==" | "!=" | "<>" | "<" | ">" | "<=" | ">=" => 3,
                "+" | "-" => 4,
                "*" | "/" => 5,
                "^" => 6,
                _ => break,
            };
            if precedence < min {
                break;
            }
            let op = op.clone();
            self.take();
            let rhs = self.expression(if op == "^" { precedence } else { precedence + 1 }, depth + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
}
fn compact(name: &str) -> Result<Expr, String> {
    if let Some(rest) = name.strip_prefix("RSI") {
        let parts = rest
            .split('.')
            .map(|n| n.parse::<f64>().map_err(|_| "Invalid compact RSI parameters"))
            .collect::<Result<Vec<_>, _>>()?;
        if parts.is_empty() || parts.len() > 3 {
            return Err("RSI uses period, optional output SMA and optional offset".into());
        }
        return Ok(Expr::Call(
            "RSI".into(),
            vec![
                Expr::Number(parts[0]),
                Expr::Number(parts.get(1).copied().unwrap_or(1.)),
                Expr::Number(parts.get(2).copied().unwrap_or(0.)),
            ],
        ));
    }
    if name == "TRUE" {
        return Ok(Expr::Number(1.));
    }
    if name == "FALSE" {
        return Ok(Expr::Number(0.));
    }
    let (head, lag) = name.split_once('.').map_or((name, None), |(a, b)| (a, Some(b)));
    let lag = lag.map(|n| n.parse::<usize>().map_err(|_| "Invalid historical offset")).transpose()?.unwrap_or(0);
    if lag > 1000 {
        return Err("Historical offsets are limited to 1000 bars".into());
    }
    let field = match head {
        "OPEN" => Some('O'),
        "HIGH" => Some('H'),
        "LOW" => Some('L'),
        "CLOSE" => Some('C'),
        "VOLUME" => Some('V'),
        _ => None,
    };
    let expression = if let Some(field) = field {
        Expr::Field(field, 0)
    } else if !head.is_empty()
        && matches!(head.as_bytes()[0], b'O' | b'H' | b'L' | b'C' | b'V')
        && head[1..].chars().all(|c| c.is_ascii_digit())
    {
        let offset =
            if head.len() == 1 { 0 } else { head[1..].parse::<usize>().map_err(|_| "Invalid historical offset")? };
        if offset > 1000 {
            return Err("Historical offsets are limited to 1000 bars".into());
        }
        Expr::Field(head.as_bytes()[0] as char, offset)
    } else {
        let mut result = None;
        for (prefix, function) in
            [("XAVG", "XAVG"), ("WAVG", "WAVG"), ("HAVG", "HAVG"), ("AVG", "AVG"), ("MAX", "MAX"), ("MIN", "MIN")]
        {
            if let Some(rest) = head.strip_prefix(prefix)
                && rest.len() > 1
                && matches!(rest.as_bytes()[0], b'O' | b'H' | b'L' | b'C' | b'V')
                && let Ok(n) = rest[1..].parse::<f64>()
            {
                result = Some(Expr::Call(
                    function.into(),
                    vec![Expr::Field(rest.as_bytes()[0] as char, 0), Expr::Number(n)],
                ));
                break;
            }
        }
        if result.is_none() {
            for prefix in ["WRSI", "RSI", "ATR"] {
                if let Some(rest) = head.strip_prefix(prefix)
                    && let Ok(n) = rest.parse::<f64>()
                {
                    result = Some(Expr::Call(prefix.into(), vec![Expr::Number(n)]));
                    break;
                }
            }
        }
        result.ok_or_else(|| format!("Unsupported field or compact expression: {name}"))?
    };
    Ok(if lag == 0 { expression } else { Expr::Call("REF".into(), vec![expression, Expr::Number(lag as f64)]) })
}
fn period(expr: &Expr, zero: bool) -> Result<usize, String> {
    if let Expr::Number(n) = expr
        && n.fract() == 0.
        && *n >= if zero { 0. } else { 1. }
        && *n <= 1000.
    {
        return Ok(*n as usize);
    }
    Err("Periods must be literal integers from 1 to 1000; REF also accepts zero".into())
}
fn lag(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; values.len()];
    if n < values.len() {
        out[n..].copy_from_slice(&values[..values.len() - n]);
    }
    out
}
fn eval(expr: &Expr, input: &Input<'_>) -> Result<Vec<f64>, String> {
    let values = match expr {
        Expr::Number(n) => vec![*n; input.bars.len()],
        Expr::Field(field, n) => lag(
            match field {
                'O' => &input.d.open,
                'H' => &input.d.high,
                'L' => &input.d.low,
                'V' => &input.d.volume,
                _ => &input.d.close,
            },
            *n,
        ),
        Expr::Unary(op, value) => eval(value, input)?
            .into_iter()
            .map(|v| {
                if !v.is_finite() {
                    f64::NAN
                } else {
                    match op.as_str() {
                        "-" => -v,
                        "NOT" => f64::from(v == 0.),
                        _ => v,
                    }
                }
            })
            .collect(),
        Expr::Binary(op, a, b) => eval(a, input)?
            .into_iter()
            .zip(eval(b, input)?)
            .map(|(a, b)| {
                if !a.is_finite() || !b.is_finite() {
                    return f64::NAN;
                }
                match op.as_str() {
                    "+" => a + b,
                    "-" => a - b,
                    "*" => a * b,
                    "/" => {
                        if b == 0. {
                            f64::NAN
                        } else {
                            a / b
                        }
                    }
                    "^" => a.powf(b),
                    "=" | "==" => f64::from(a == b),
                    "!=" | "<>" => f64::from(a != b),
                    "<" => f64::from(a < b),
                    ">" => f64::from(a > b),
                    "<=" => f64::from(a <= b),
                    ">=" => f64::from(a >= b),
                    "AND" => f64::from(a != 0. && b != 0.),
                    "OR" => f64::from(a != 0. || b != 0.),
                    _ => f64::NAN,
                }
            })
            .collect(),
        Expr::Call(name, args) => match (name.as_str(), args.as_slice()) {
            ("RSI", [n, smoothing]) => {
                let n = period(n, false)?;
                let smoothing = period(smoothing, false)?;
                sma(&formula::plain_rsi(&input.d.close, n), smoothing)
            }
            ("RSI", [n, smoothing, offset]) => {
                let n = period(n, false)?;
                let smoothing = period(smoothing, false)?;
                let offset = period(offset, true)?;
                lag(&sma(&formula::plain_rsi(&input.d.close, n), smoothing), offset)
            }
            ("WRSI" | "ATR" | "WATR", [n, offset]) => {
                let offset = period(offset, true)?;
                lag(&eval(&Expr::Call(name.clone(), vec![Expr::Number(period(n, false)? as f64)]), input)?, offset)
            }
            (
                "AVG" | "SMA" | "XAVG" | "EMA" | "WAVG" | "HAVG" | "RMA" | "SUM" | "MAX" | "MIN" | "STDDEV" | "REF",
                [value, n],
            ) => {
                let n = period(n, name == "REF")?;
                let values = eval(value, input)?;
                match name.as_str() {
                    "AVG" | "SMA" => sma(&values, n),
                    "XAVG" | "EMA" => ema(&values, n),
                    "WAVG" => wma(&values, n),
                    "HAVG" => hma(&values, n),
                    "RMA" => rma(&values, n),
                    "SUM" => sum(&values, n),
                    "MAX" => highest(&values, n),
                    "MIN" => lowest(&values, n),
                    "STDDEV" => stdev(&values, n),
                    _ => lag(&values, n),
                }
            }
            ("RSI" | "WRSI" | "ATR" | "WATR", [n]) => {
                let n = period(n, false)?;
                match name.as_str() {
                    "RSI" => formula::plain_rsi(&input.d.close, n),
                    "WRSI" => rsi(&input.d.close, n),
                    "ATR" => sma(&true_range(&input.d), n),
                    _ => rma(&true_range(&input.d), n),
                }
            }
            ("ABS" | "SQRT" | "LOG" | "EXP", [value]) => eval(value, input)?
                .into_iter()
                .map(|v| match name.as_str() {
                    "ABS" => v.abs(),
                    "SQRT" => v.sqrt(),
                    "LOG" => v.ln(),
                    _ => v.exp(),
                })
                .collect(),
            ("IIF" | "IF", [condition, yes, no]) => eval(condition, input)?
                .into_iter()
                .zip(eval(yes, input)?)
                .zip(eval(no, input)?)
                .map(|((c, y), n)| {
                    if !c.is_finite() {
                        f64::NAN
                    } else if c != 0. {
                        y
                    } else {
                        n
                    }
                })
                .collect(),
            ("CROSS", [a, b]) => {
                let a = eval(a, input)?;
                let b = eval(b, input)?;
                (0..a.len())
                    .map(|i| {
                        if i == 0 || ![a[i], b[i], a[i - 1], b[i - 1]].iter().all(|v| v.is_finite()) {
                            f64::NAN
                        } else {
                            f64::from(a[i] > b[i] && a[i - 1] <= b[i - 1])
                        }
                    })
                    .collect()
            }
            _ => return Err(format!("Unsupported function or argument count: {name}")),
        },
    };
    Ok(values.into_iter().map(|v| if v.is_finite() { v } else { f64::NAN }).collect())
}
pub fn evaluate(source: &str, input: &Input<'_>) -> Result<Vec<f64>, String> {
    let mut parser = Parser { tokens: lex(source)?, at: 0, nodes: 0 };
    let expr = parser.expression(0, 0)?;
    if parser.tokens[parser.at] != Token::End {
        return Err("Unexpected trailing formula input".into());
    }
    eval(&expr, input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bar, Timeframe};
    fn input(bars: &[Bar]) -> Input<'_> {
        Input::new(bars, Timeframe::D1)
    }
    #[test]
    fn arithmetic_compact_fields_and_conditions() {
        let bars: Vec<_> = [1., 2., 4., 2., 8.]
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar::new(i as i64, c, c + 1., c - 1., c, 10.))
            .collect();
        let v = evaluate("C > AVGC3 AND V >= 2 * AVG(V,2)", &input(&bars)).unwrap();
        assert_eq!(v[4], 0.);
        let v = evaluate("IIF(C > C1, C-C1, 0)", &input(&bars)).unwrap();
        assert_eq!(&v[1..], &[1., 2., 0., 6.]);
        assert_eq!(evaluate("XAVGC3.1", &input(&bars)).unwrap()[4], ema(&input(&bars).d.close, 3)[3]);
        assert_eq!(evaluate("2+3*4^2", &input(&bars)).unwrap()[0], 50.);
        assert_eq!(evaluate("-2^2", &input(&bars)).unwrap()[0], -4.);
        let rsi = formula::plain_rsi(&input(&bars).d.close, 2);
        let smoothed = sma(&rsi, 2);
        assert_eq!(evaluate("RSI2.2", &input(&bars)).unwrap()[4], smoothed[4]);
        assert_eq!(evaluate("RSI2.2.1", &input(&bars)).unwrap()[4], smoothed[3]);
        assert_eq!(evaluate("RSI(2,2,1)", &input(&bars)).unwrap()[4], smoothed[3]);
    }
    #[test]
    fn malformed_and_future_references_are_rejected_without_panicking() {
        for text in ["", "C +", "AVG(C,0)", "REF(C,-1)", "BOP(C)", "C1001", "AVG(C,2.5)", "C; DROP", "1e999", "((((C)"]
        {
            assert!(evaluate(text, &input(&[])).is_err(), "{text}");
        }
        assert!(evaluate(&format!("{}C{}", "(".repeat(40), ")".repeat(40)), &input(&[])).is_err());
    }
    #[test]
    fn formula_outputs_are_causal_and_preserve_undefined_values() {
        let bars: Vec<_> = (0..40).map(|i| Bar::new(i, 1., 2., 0., i as f64, 1.)).collect();
        let text = "CROSS(XAVG(C,3), AVG(C,7)) OR WRSI(5)>70";
        let full = evaluate(text, &input(&bars)).unwrap();
        for n in 1..40 {
            let prefix = evaluate(text, &input(&bars[..n])).unwrap();
            for (a, b) in prefix.iter().zip(&full) {
                assert!(a == b || a.is_nan() && b.is_nan());
            }
        }
        assert!(evaluate("C/0", &input(&bars)).unwrap().iter().all(|v| v.is_nan()));
    }
}
