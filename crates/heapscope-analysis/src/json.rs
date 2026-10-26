//! The machine-readable summary of an analysis (`heapscope analyze -o summary.json`).
//!
//! Schema version 1 is frozen: fields may be added in a later version only with a new
//! `schema_version`, never renamed, removed or changed in meaning. `docs/json-schema.md` describes
//! every field, and `tests/summary_json.rs` pins the key set.
//!
//! All counts and byte figures are scaled by `capture.sample_rate` for a sampled capture, so they
//! are estimates; `capture.estimated` says so. Timestamps are nanoseconds since the capture began.
//! Hand-written on purpose: the workspace has no JSON dependency and the schema is small.

use crate::identity;
use crate::leaks::{LeakEntry, LeakReport, ReportKind};
use crate::model::{Analysis, NodeId};

pub const SCHEMA_VERSION: u64 = 1;

/// Frames of the call path listed for each site, innermost first.
const PATH_FRAMES: usize = 16;

/// A JSON value. Objects keep insertion order so the output is stable.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Uint(u64),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Json::Uint(n) => Some(n),
            Json::Int(n) => u64::try_from(n).ok(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Keys of an object, in order (empty for anything else).
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Json::Object(fields) => fields.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    /// Two-space indented text ending in a newline.
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    /// No whitespace at all, for data embedded in a page.
    pub fn to_compact(&self) -> String {
        let mut out = String::new();
        self.write_compact(&mut out);
        out
    }

    fn write_compact(&self, out: &mut String) {
        match self {
            Json::Array(items) => {
                out.push('[');
                for (n, item) in items.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    item.write_compact(out);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                for (n, (k, v)) in fields.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    write_string(out, k);
                    out.push(':');
                    v.write_compact(out);
                }
                out.push('}');
            }
            scalar => scalar.write(out, 0),
        }
    }

    fn write(&self, out: &mut String, indent: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Uint(n) => out.push_str(&n.to_string()),
            Json::Int(n) => out.push_str(&n.to_string()),
            // JSON has no NaN or infinity.
            Json::Float(f) if f.is_finite() && f.fract() == 0.0 => {
                out.push_str(&format!("{f:.1}"));
            }
            Json::Float(f) if f.is_finite() => out.push_str(&format!("{f}")),
            Json::Float(_) => out.push_str("null"),
            Json::Str(s) => write_string(out, s),
            Json::Array(items) if items.is_empty() => out.push_str("[]"),
            Json::Array(items) => {
                // Arrays of scalars stay on one line.
                if items
                    .iter()
                    .all(|i| !matches!(i, Json::Array(_) | Json::Object(_)))
                {
                    out.push('[');
                    for (n, item) in items.iter().enumerate() {
                        if n > 0 {
                            out.push_str(", ");
                        }
                        item.write(out, indent);
                    }
                    out.push(']');
                    return;
                }
                out.push_str("[\n");
                for (n, item) in items.iter().enumerate() {
                    pad(out, indent + 1);
                    item.write(out, indent + 1);
                    out.push_str(if n + 1 < items.len() { ",\n" } else { "\n" });
                }
                pad(out, indent);
                out.push(']');
            }
            Json::Object(fields) if fields.is_empty() => out.push_str("{}"),
            Json::Object(fields) => {
                out.push_str("{\n");
                for (n, (k, v)) in fields.iter().enumerate() {
                    pad(out, indent + 1);
                    write_string(out, k);
                    out.push_str(": ");
                    v.write(out, indent + 1);
                    out.push_str(if n + 1 < fields.len() { ",\n" } else { "\n" });
                }
                pad(out, indent);
                out.push('}');
            }
        }
    }

    /// Parse JSON text. Used by tests and by consumers of the summary such as the CI gate.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            at: 0,
        };
        p.skip_ws();
        let v = p.value(0)?;
        p.skip_ws();
        if p.at != p.s.len() {
            return Err(p.error("trailing characters"));
        }
        Ok(v)
    }
}

fn pad(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

const MAX_DEPTH: usize = 64;

impl Parser<'_> {
    fn error(&self, what: &str) -> String {
        format!("invalid JSON at byte {}: {what}", self.at)
    }

    fn skip_ws(&mut self) {
        while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.s.get(self.at) == Some(&c) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", c as char)))
        }
    }

    fn literal(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.s[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(v)
        } else {
            Err(self.error("unknown literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        match self.s.get(self.at) {
            None => Err(self.error("unexpected end")),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.skip_ws();
                if self.s.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                loop {
                    self.skip_ws();
                    items.push(self.value(depth + 1)?);
                    self.skip_ws();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Json::Array(items));
                        }
                        _ => return Err(self.error("expected ',' or ']'")),
                    }
                }
            }
            Some(b'{') => {
                self.at += 1;
                let mut fields = Vec::new();
                self.skip_ws();
                if self.s.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Json::Object(fields));
                }
                loop {
                    self.skip_ws();
                    let k = self.string()?;
                    self.skip_ws();
                    self.eat(b':')?;
                    self.skip_ws();
                    fields.push((k, self.value(depth + 1)?));
                    self.skip_ws();
                    match self.s.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Json::Object(fields));
                        }
                        _ => return Err(self.error("expected ',' or '}'")),
                    }
                }
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => Err(self.error("unexpected character")),
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        while self
            .s
            .get(self.at)
            .is_some_and(|c| matches!(c, b'-' | b'+' | b'.' | b'e' | b'E') || c.is_ascii_digit())
        {
            self.at += 1;
        }
        let text =
            std::str::from_utf8(&self.s[start..self.at]).map_err(|_| self.error("number"))?;
        if let Ok(n) = text.parse::<u64>() {
            Ok(Json::Uint(n))
        } else if let Ok(n) = text.parse::<i64>() {
            Ok(Json::Int(n))
        } else {
            text.parse::<f64>()
                .map(Json::Float)
                .map_err(|_| self.error("bad number"))
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .s
            .get(self.at..self.at + 4)
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|t| u32::from_str_radix(t, 16).ok())
            .ok_or_else(|| self.error("bad \\u escape"))?;
        self.at += 4;
        Ok(digits)
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.at) else {
                return Err(self.error("unterminated string"));
            };
            self.at += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.at) else {
                        return Err(self.error("unterminated escape"));
                    };
                    self.at += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp)
                                && self.s.get(self.at..self.at + 2) == Some(b"\\u")
                            {
                                self.at += 2;
                                let low = self.hex4()?;
                                cp = 0x10000
                                    + ((cp - 0xD800) << 10)
                                    + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            char::from_u32(cp).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(self.error("bad escape")),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.error("string is not UTF-8"))
    }
}

fn obj(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

fn s(text: impl Into<String>) -> Json {
    Json::Str(text.into())
}

fn opt_s(text: &Option<String>) -> Json {
    text.as_ref().map_or(Json::Null, |t| s(t.clone()))
}

/// `file:line` of a site, or `None` when the frame has no source location.
pub fn site_location(a: &Analysis, node: NodeId) -> Option<String> {
    let f = &a.tree.node(node).frame;
    let file = f.file.as_ref()?;
    Some(if f.line > 0 {
        format!("{file}:{}", f.line)
    } else {
        file.clone()
    })
}

/// The part of a site that identifies it: stable id, function, module, location and call path.
fn site_fields(a: &Analysis, node: NodeId) -> Vec<(&'static str, Json)> {
    let frame = &a.tree.node(node).frame;
    vec![
        (
            "id",
            s(identity::path_key(&a.tree, node, identity::DEFAULT_DEPTH)),
        ),
        ("function", s(frame.name.clone())),
        ("module", opt_s(&frame.module)),
        ("file", opt_s(&frame.file)),
        ("line", Json::Uint(u64::from(frame.line))),
        (
            "path",
            Json::Array(
                a.tree
                    .path(node)
                    .iter()
                    .take(PATH_FRAMES)
                    .map(|f| s(f.name.clone()))
                    .collect(),
            ),
        ),
    ]
}

fn with(mut base: Vec<(&'static str, Json)>, more: Vec<(&'static str, Json)>) -> Json {
    base.extend(more);
    obj(base)
}

fn leak_entry(a: &Analysis, e: &LeakEntry) -> Json {
    let mut more = vec![
        ("kind", s(e.kind.label())),
        ("allocs", Json::Uint(e.allocs)),
        ("bytes", Json::Uint(e.bytes)),
        ("oldest_ts_ns", Json::Uint(e.oldest_ts_ns)),
        ("newest_ts_ns", Json::Uint(e.newest_ts_ns)),
    ];
    if let Some(by) = &e.suppressed_by {
        more.push((
            "suppressed_by",
            obj(vec![
                ("rule", s(by.rule.clone())),
                ("reason", s(by.reason.clone())),
                ("builtin", Json::Bool(by.builtin)),
            ]),
        ));
    }
    with(site_fields(a, e.node), more)
}

/// The `capture` object of the summary: how the trace was recorded.
pub fn capture(c: &crate::model::CaptureInfo) -> Json {
    obj(vec![
        ("mode", s(c.mode())),
        ("sample_rate", Json::Uint(u64::from(c.sample_rate))),
        ("min_lifetime_ms", Json::Uint(u64::from(c.min_lifetime_ms))),
        ("attached", Json::Bool(c.attached)),
        ("estimated", Json::Bool(c.is_estimate())),
        ("complete", Json::Bool(c.complete)),
        ("exe", s(c.exe_path.clone())),
        ("command_line", s(c.command_line.clone())),
        ("pid", Json::Uint(u64::from(c.pid))),
        ("pointer_size", Json::Uint(u64::from(c.pointer_size))),
        (
            "dropped",
            obj(vec![
                ("overflow", Json::Uint(c.dropped_overflow)),
                ("min_lifetime", Json::Uint(c.dropped_min_lifetime)),
                ("sampling", Json::Uint(c.dropped_sampling)),
            ]),
        ),
    ])
}

/// Build the version 1 summary of `analysis` and its `leaks` report.
pub fn summary(a: &Analysis, leaks: &LeakReport) -> Json {
    let scale = a.scale();
    let c = &a.capture;
    let t = &a.totals;

    let capture = capture(c);

    let totals = obj(vec![
        ("allocs", Json::Uint(t.allocs.saturating_mul(scale))),
        (
            "alloc_bytes",
            Json::Uint(t.alloc_bytes.saturating_mul(scale)),
        ),
        ("frees", Json::Uint(t.frees.saturating_mul(scale))),
        (
            "freed_bytes",
            Json::Uint(t.freed_bytes.saturating_mul(scale)),
        ),
        (
            "live_allocs",
            Json::Uint(t.live_allocs.saturating_mul(scale)),
        ),
        ("live_bytes", Json::Uint(t.live_bytes.saturating_mul(scale))),
        ("unmatched_frees", Json::Uint(t.unmatched_frees)),
        ("missed_frees", Json::Uint(t.missed_frees)),
    ]);

    let peak = obj(vec![
        ("ts_ns", Json::Uint(a.peak.ts_ns)),
        (
            "live_bytes",
            Json::Uint(a.peak.live_bytes.saturating_mul(scale)),
        ),
        (
            "live_allocs",
            Json::Uint(a.peak.live_allocs.saturating_mul(scale)),
        ),
    ]);

    let mut ids: Vec<NodeId> = (1..a.tree.len() as NodeId)
        .filter(|&i| a.stats[i as usize].allocs > 0)
        .collect();
    let keyed: std::collections::HashMap<NodeId, String> = ids
        .iter()
        .map(|&i| (i, identity::path_key(&a.tree, i, identity::DEFAULT_DEPTH)))
        .collect();
    ids.sort_by(|&x, &y| {
        let (sx, sy) = (&a.stats[x as usize], &a.stats[y as usize]);
        sy.alloc_bytes
            .cmp(&sx.alloc_bytes)
            .then(sy.allocs.cmp(&sx.allocs))
            .then(keyed[&x].cmp(&keyed[&y]))
            .then(x.cmp(&y))
    });
    let sites = Json::Array(
        ids.iter()
            .map(|&i| {
                let st = &a.stats[i as usize];
                with(
                    site_fields(a, i),
                    vec![
                        ("allocs", Json::Uint(st.allocs.saturating_mul(scale))),
                        (
                            "alloc_bytes",
                            Json::Uint(st.alloc_bytes.saturating_mul(scale)),
                        ),
                        ("frees", Json::Uint(st.frees.saturating_mul(scale))),
                        (
                            "freed_bytes",
                            Json::Uint(st.freed_bytes.saturating_mul(scale)),
                        ),
                        (
                            "live_allocs",
                            Json::Uint(st.live_allocs.saturating_mul(scale)),
                        ),
                        (
                            "live_bytes",
                            Json::Uint(st.live_bytes.saturating_mul(scale)),
                        ),
                        (
                            "peak_live_bytes",
                            Json::Uint(st.peak_live_bytes.saturating_mul(scale)),
                        ),
                        (
                            "short_lived_allocs",
                            Json::Uint(st.short_allocs.saturating_mul(scale)),
                        ),
                    ],
                )
            })
            .collect(),
    );

    let leaks_json = obj(vec![
        (
            "kind",
            s(match leaks.kind {
                ReportKind::Leaks => "leaks",
                ReportKind::NetGrowth => "net_growth",
            }),
        ),
        ("title", s(leaks.title())),
        ("estimated", Json::Bool(leaks.estimated)),
        ("total_allocs", Json::Uint(leaks.total_allocs)),
        ("total_bytes", Json::Uint(leaks.total_bytes)),
        ("suppressed_allocs", Json::Uint(leaks.suppressed_allocs)),
        ("suppressed_bytes", Json::Uint(leaks.suppressed_bytes)),
        (
            "by_kind",
            Json::Array(
                leaks
                    .by_kind
                    .iter()
                    .map(|k| {
                        obj(vec![
                            ("kind", s(k.kind.label())),
                            ("allocs", Json::Uint(k.allocs)),
                            ("bytes", Json::Uint(k.bytes)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "entries",
            Json::Array(leaks.entries.iter().map(|e| leak_entry(a, e)).collect()),
        ),
        (
            "suppressed",
            Json::Array(leaks.suppressed.iter().map(|e| leak_entry(a, e)).collect()),
        ),
        (
            "notes",
            Json::Array(leaks.notes.iter().map(|n| s(n.clone())).collect()),
        ),
    ]);

    let churn = Json::Array(
        a.churn
            .iter()
            .map(|e| {
                with(
                    site_fields(a, e.node),
                    vec![
                        ("allocs", Json::Uint(e.allocs.saturating_mul(scale))),
                        ("bytes", Json::Uint(e.bytes.saturating_mul(scale))),
                        ("avg_lifetime_ns", Json::Uint(e.avg_lifetime_ns)),
                        ("max_concurrent", Json::Uint(e.max_concurrent)),
                        ("share", Json::Float(e.share)),
                    ],
                )
            })
            .collect(),
    );

    let markers = Json::Array(
        a.markers
            .iter()
            .map(|m| {
                obj(vec![
                    ("label", s(m.label.clone())),
                    ("ts_ns", Json::Uint(m.ts_ns)),
                    ("live_bytes", Json::Uint(m.live_bytes.saturating_mul(scale))),
                    (
                        "live_allocs",
                        Json::Uint(m.live_allocs.saturating_mul(scale)),
                    ),
                ])
            })
            .collect(),
    );

    obj(vec![
        ("schema_version", Json::Uint(SCHEMA_VERSION)),
        ("tool", s("heapscope")),
        ("capture", capture),
        ("totals", totals),
        ("peak", peak),
        ("sites", sites),
        ("leaks", leaks_json),
        ("churn", churn),
        ("markers", markers),
        (
            "warnings",
            Json::Array(a.warnings.iter().map(|w| s(w.clone())).collect()),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_awkward_strings_and_numbers() {
        let v = obj(vec![
            ("text", s("a \"quoted\" \\ path\n\ttab \u{1} é 😀")),
            ("big", Json::Uint(u64::MAX)),
            ("neg", Json::Int(-5)),
            ("f", Json::Float(0.25)),
            ("none", Json::Null),
            ("list", Json::Array(vec![Json::Bool(true), Json::Uint(1)])),
            ("empty", Json::Array(vec![])),
            ("nested", obj(vec![("k", Json::Array(vec![obj(vec![])]))])),
        ]);
        let text = v.to_pretty();
        assert_eq!(Json::parse(&text).unwrap(), v);
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "tru", "\"abc", "1 2", "[1 2]"] {
            assert!(Json::parse(bad).is_err(), "{bad:?}");
        }
        let deep = "[".repeat(200);
        assert!(Json::parse(&deep).is_err());
    }

    #[test]
    fn parses_surrogate_pairs() {
        assert_eq!(
            Json::parse("\"\\ud83d\\ude00\"").unwrap(),
            Json::Str("😀".into())
        );
    }
}
