//! Per-SoC quirk table and matching.

use std::fmt;

use crate::error::MediaCodecError;

/// Embedded quirk source.
const SOURCE: &str = include_str!("../quirks.toml");

/// Section header opening one quirk.
const SECTION: &str = "[[quirk]]";

/// Codec scope of a quirk.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CodecScope {
    /// Every codec.
    Any,
    /// H.264.
    H264,
    /// H.265.
    H265,
    /// AV1.
    Av1,
    /// VP9.
    Vp9,
}

impl CodecScope {
    /// Scope for a mime type, if one of the four tier codecs.
    fn of_mime(mime: &str) -> Option<Self> {
        match mime {
            crate::format::FormatSpec::MIME_H264 => Some(Self::H264),
            crate::format::FormatSpec::MIME_H265 => Some(Self::H265),
            crate::format::FormatSpec::MIME_AV1 => Some(Self::Av1),
            crate::format::FormatSpec::MIME_VP9 => Some(Self::Vp9),
            _ => None,
        }
    }

    /// Scope for a table codec token, if known.
    fn of_token(token: &str) -> Option<Self> {
        match token.to_lowercase().as_str() {
            "*" => Some(Self::Any),
            "h264" => Some(Self::H264),
            "h265" => Some(Self::H265),
            "av1" => Some(Self::Av1),
            "vp9" => Some(Self::Vp9),
            _ => None,
        }
    }

    /// True when `self` covers `other`.
    fn covers(self, other: Self) -> bool {
        self == Self::Any || self == other
    }
}

/// Proof state of a quirk.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QuirkStatus {
    /// Reproduced on two chips.
    Confirmed,
    /// Awaiting device proof.
    Suspected,
}

/// Quirk table rejection cause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuirkSyntax {
    /// Content outside a section.
    ExpectedSection,
    /// Line without an assignment.
    ExpectedAssignment,
    /// Key outside the known seven.
    UnknownKey(String),
    /// Key repeated in one section.
    DuplicateKey(&'static str),
    /// Value of the wrong shape.
    BadValue {
        /// Key being assigned.
        key: &'static str,
        /// Rejected raw value.
        value: String,
    },
    /// Required key absent at section end.
    MissingField(&'static str),
}

impl fmt::Display for QuirkSyntax {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedSection => write!(f, "expected {SECTION}"),
            Self::ExpectedAssignment => write!(f, "expected key = value"),
            Self::UnknownKey(key) => write!(f, "unknown key {key}"),
            Self::DuplicateKey(key) => write!(f, "duplicate key {key}"),
            Self::BadValue { key, value } => write!(f, "bad value for {key}: {value}"),
            Self::MissingField(key) => write!(f, "missing field {key}"),
        }
    }
}

/// Quirk table under construction.
#[derive(Debug, Default)]
struct PartialQuirk {
    /// Stable identifier.
    id: Option<String>,
    /// `SoC` patterns.
    soc: Option<Vec<String>>,
    /// Codec scopes.
    codecs: Option<Vec<CodecScope>>,
    /// Proof state.
    status: Option<QuirkStatus>,
    /// Origin of the entry.
    source: Option<String>,
    /// Misbehavior without the workaround.
    effect: Option<String>,
    /// Behavior when the quirk does not match.
    fallback: Option<String>,
}

impl PartialQuirk {
    /// Complete entry, or the first missing field.
    fn build(self, line: u64) -> Result<Quirk, MediaCodecError> {
        let missing = |key: &'static str| MediaCodecError::Quirk {
            line,
            reason: QuirkSyntax::MissingField(key),
        };
        Ok(Quirk {
            id: self.id.ok_or_else(|| missing("id"))?,
            soc: self.soc.ok_or_else(|| missing("soc"))?,
            codecs: self.codecs.ok_or_else(|| missing("codec"))?,
            status: self.status.ok_or_else(|| missing("status"))?,
            source: self.source.ok_or_else(|| missing("source"))?,
            effect: self.effect.ok_or_else(|| missing("effect"))?,
            fallback: self.fallback.ok_or_else(|| missing("fallback"))?,
        })
    }
}

/// One quirk entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Quirk {
    /// Stable identifier.
    id: String,
    /// Lowercase `SoC` substrings, or `*`.
    soc: Vec<String>,
    /// Covered codecs.
    codecs: Vec<CodecScope>,
    /// Proof state.
    status: QuirkStatus,
    /// Origin of the entry.
    source: String,
    /// Misbehavior without the workaround.
    effect: String,
    /// Behavior when the quirk does not match.
    fallback: String,
}

impl Quirk {
    /// Stable identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Proof state.
    #[must_use]
    pub fn status(&self) -> QuirkStatus {
        self.status
    }

    /// Origin of the entry.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Misbehavior without the workaround.
    #[must_use]
    pub fn effect(&self) -> &str {
        &self.effect
    }

    /// Behavior when the quirk does not match.
    #[must_use]
    pub fn fallback(&self) -> &str {
        &self.fallback
    }

    /// True when the quirk covers lowercased `soc` and `scope`.
    fn matches(&self, soc: &str, scope: CodecScope) -> bool {
        let soc_hit = self
            .soc
            .iter()
            .any(|pattern| pattern == "*" || soc.contains(pattern));
        soc_hit && self.codecs.iter().any(|codec| codec.covers(scope))
    }
}

/// Parsed quirk table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuirkTable {
    /// Entries in file order.
    quirks: Vec<Quirk>,
}

impl QuirkTable {
    /// Table parsed from the embedded `quirks.toml`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the embedded table breaks the strict shape.
    pub fn load() -> Result<Self, MediaCodecError> {
        Self::parse(SOURCE)
    }

    /// Table parsed from `text`.
    ///
    /// # Arguments
    ///
    /// * `text` - quirk source in the strict `[[quirk]]` shape.
    ///
    /// # Errors
    ///
    /// Returns `Err` naming the first line that breaks the shape.
    pub fn parse(text: &str) -> Result<Self, MediaCodecError> {
        let mut table = Self { quirks: Vec::new() };
        let mut current: Option<PartialQuirk> = None;
        let mut line = 0_u64;
        for raw in text.lines() {
            line += 1;
            let stripped = raw.trim();
            if stripped.is_empty() || stripped.starts_with('#') {
                continue;
            }
            if stripped == SECTION {
                if let Some(partial) = current.take() {
                    table.quirks.push(partial.build(line)?);
                }
                current = Some(PartialQuirk::default());
                continue;
            }
            let Some(partial) = current.as_mut() else {
                return Err(MediaCodecError::Quirk {
                    line,
                    reason: QuirkSyntax::ExpectedSection,
                });
            };
            let Some((key, value)) = stripped.split_once('=') else {
                return Err(MediaCodecError::Quirk {
                    line,
                    reason: QuirkSyntax::ExpectedAssignment,
                });
            };
            assign(partial, line, key.trim(), value.trim())?;
        }
        if let Some(partial) = current.take() {
            table.quirks.push(partial.build(line)?);
        }
        Ok(table)
    }

    /// Entries covering `soc` and `mime`, in file order.
    #[must_use]
    pub fn matching(&self, soc: &str, mime: &str) -> Vec<&Quirk> {
        let Some(scope) = CodecScope::of_mime(mime) else {
            return Vec::new();
        };
        let lowered = soc.trim().to_lowercase();
        self.quirks
            .iter()
            .filter(|quirk| quirk.matches(&lowered, scope))
            .collect()
    }

    /// Entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.quirks.len()
    }

    /// True when the table holds no entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.quirks.is_empty()
    }
}

/// Quoted string with `\"` and `\\` escapes.
fn parse_string(raw: &str) -> Option<String> {
    let mut chars = raw.chars();
    if chars.next() != Some('"') {
        return None;
    }
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    chars.next().is_none().then_some(out)
}

/// Comma-separated quoted strings in brackets.
fn parse_list(raw: &str) -> Option<Vec<String>> {
    let inner = raw.strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    inner
        .split(',')
        .map(|item| parse_string(item.trim()))
        .collect()
}

/// Rejection for a misshapen `key` holding raw `value` at `line`.
fn bad_value(line: u64, key: &'static str, value: &str) -> MediaCodecError {
    MediaCodecError::Quirk {
        line,
        reason: QuirkSyntax::BadValue {
            key,
            value: value.to_owned(),
        },
    }
}

/// Rejection for a repeated `key` at `line`.
fn duplicate(line: u64, key: &'static str) -> MediaCodecError {
    MediaCodecError::Quirk {
        line,
        reason: QuirkSyntax::DuplicateKey(key),
    }
}

/// One assignment into the quirk under construction.
fn assign(
    partial: &mut PartialQuirk,
    line: u64,
    key: &str,
    value: &str,
) -> Result<(), MediaCodecError> {
    match key {
        "id" => {
            if partial.id.is_some() {
                return Err(duplicate(line, "id"));
            }
            partial.id = Some(parse_string(value).ok_or_else(|| bad_value(line, "id", value))?);
        }
        "soc" => {
            if partial.soc.is_some() {
                return Err(duplicate(line, "soc"));
            }
            partial.soc = Some(parse_list(value).ok_or_else(|| bad_value(line, "soc", value))?);
        }
        "codec" => {
            if partial.codecs.is_some() {
                return Err(duplicate(line, "codec"));
            }
            let tokens = parse_list(value).ok_or_else(|| bad_value(line, "codec", value))?;
            let mut scopes = Vec::with_capacity(tokens.len());
            for token in &tokens {
                scopes.push(
                    CodecScope::of_token(token).ok_or_else(|| bad_value(line, "codec", value))?,
                );
            }
            partial.codecs = Some(scopes);
        }
        "status" => {
            if partial.status.is_some() {
                return Err(duplicate(line, "status"));
            }
            let token = parse_string(value).ok_or_else(|| bad_value(line, "status", value))?;
            partial.status = Some(match token.as_str() {
                "confirmed" => QuirkStatus::Confirmed,
                "suspected" => QuirkStatus::Suspected,
                _ => return Err(bad_value(line, "status", value)),
            });
        }
        "source" => {
            if partial.source.is_some() {
                return Err(duplicate(line, "source"));
            }
            partial.source =
                Some(parse_string(value).ok_or_else(|| bad_value(line, "source", value))?);
        }
        "effect" => {
            if partial.effect.is_some() {
                return Err(duplicate(line, "effect"));
            }
            partial.effect =
                Some(parse_string(value).ok_or_else(|| bad_value(line, "effect", value))?);
        }
        "fallback" => {
            if partial.fallback.is_some() {
                return Err(duplicate(line, "fallback"));
            }
            partial.fallback =
                Some(parse_string(value).ok_or_else(|| bad_value(line, "fallback", value))?);
        }
        _ => {
            return Err(MediaCodecError::Quirk {
                line,
                reason: QuirkSyntax::UnknownKey(key.to_owned()),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_list, parse_string, QuirkStatus, QuirkSyntax, QuirkTable};

    use crate::error::MediaCodecError;

    const ENTRY: &str = concat!(
        "[[quirk]]\n",
        "id = \"q\"\n",
        "soc = [\"snapdragon\"]\n",
        "codec = [\"h264\", \"*\"]\n",
        "status = \"confirmed\"\n",
        "source = \"s\"\n",
        "effect = \"e\"\n",
        "fallback = \"f\"\n",
    );

    #[test]
    fn embedded_table_loads_with_four_entries() {
        let table = QuirkTable::load().expect("embedded quirks.toml parses");
        assert_eq!(table.len(), 4);
        assert!(!table.is_empty());
    }

    #[test]
    fn embedded_entries_carry_fallbacks() {
        let table = QuirkTable::load().expect("embedded quirks.toml parses");
        for quirk in table.matching("", "video/avc") {
            assert!(!quirk.fallback().is_empty(), "{}", quirk.id());
            assert!(!quirk.effect().is_empty(), "{}", quirk.id());
            assert!(!quirk.source().is_empty(), "{}", quirk.id());
        }
    }

    #[test]
    fn matching_covers_soc_families_and_codecs() {
        let table = QuirkTable::load().expect("embedded quirks.toml parses");
        let qcom: Vec<&str> = table
            .matching("SM8850", "video/hevc")
            .iter()
            .map(|quirk| quirk.id())
            .collect();
        assert!(qcom.contains(&"qcom-stride-padding"));
        assert!(qcom.contains(&"async-null-callback-segfault"));
        let mtk: Vec<&str> = table
            .matching("MT6989", "video/avc")
            .iter()
            .map(|quirk| quirk.id())
            .collect();
        assert!(mtk.contains(&"mtk-csd-out-of-band"));
        let exynos: Vec<&str> = table
            .matching("Exynos 2500", "video/x-vnd.on2.vp9")
            .iter()
            .map(|quirk| quirk.id())
            .collect();
        assert!(exynos.contains(&"exynos-drain-before-flush"));
        assert!(table.matching("SM8850", "video/unknown").is_empty());
    }

    #[test]
    fn codec_scope_filters_entries() {
        let h264_only = ENTRY.replace("[\"h264\", \"*\"]", "[\"h264\"]");
        let table = QuirkTable::parse(&h264_only).expect("entry parses");
        assert_eq!(table.matching("snapdragon", "video/avc").len(), 1);
        assert!(table.matching("snapdragon", "video/av01").is_empty());
        assert!(table.matching("exynos", "video/avc").is_empty());
    }

    #[test]
    fn statuses_parse() {
        let table = QuirkTable::parse(ENTRY).expect("single entry parses");
        let quirk = table.matching("snapdragon", "video/avc");
        assert_eq!(quirk[0].status(), QuirkStatus::Confirmed);
        let suspected = ENTRY.replace("\"confirmed\"", "\"suspected\"");
        let table = QuirkTable::parse(&suspected).expect("suspected parses");
        let quirk = table.matching("snapdragon", "video/avc");
        assert_eq!(quirk[0].status(), QuirkStatus::Suspected);
    }

    #[test]
    fn strings_take_escapes_and_reject_tails() {
        assert_eq!(parse_string("\"a\""), Some("a".to_owned()));
        assert_eq!(parse_string("\"\""), Some(String::new()));
        assert_eq!(parse_string("\"a\\\"b\""), Some("a\"b".to_owned()));
        assert_eq!(parse_string("\"a\\\\b\""), Some("a\\b".to_owned()));
        assert_eq!(parse_string("\"a"), None);
        assert_eq!(parse_string("a\""), None);
        assert_eq!(parse_string("\"a\"b"), None);
        assert_eq!(parse_string("\"a\\qb\""), None);
    }

    #[test]
    fn lists_take_strings_and_empty() {
        assert_eq!(parse_list("[]"), Some(Vec::new()));
        assert_eq!(
            parse_list("[\"a\", \"b\"]"),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(parse_list("[\"a\""), None);
        assert_eq!(parse_list("\"a\""), None);
        assert_eq!(parse_list("[a]"), None);
    }

    #[test]
    fn rejects_shape_breaks_with_lines() {
        let error = QuirkTable::parse("id = \"x\"\n").expect_err("stray content rejected");
        assert_eq!(
            error,
            MediaCodecError::Quirk {
                line: 1,
                reason: QuirkSyntax::ExpectedSection,
            }
        );
        let error = QuirkTable::parse("[[quirk]]\nbare\n").expect_err("bare line rejected");
        assert_eq!(
            error,
            MediaCodecError::Quirk {
                line: 2,
                reason: QuirkSyntax::ExpectedAssignment,
            }
        );
        let error =
            QuirkTable::parse("[[quirk]]\nnope = \"x\"\n").expect_err("unknown key rejected");
        assert_eq!(
            error,
            MediaCodecError::Quirk {
                line: 2,
                reason: QuirkSyntax::UnknownKey("nope".to_owned()),
            }
        );
        let error = QuirkTable::parse("[[quirk]]\nid = \"a\"\nid = \"b\"\n")
            .expect_err("duplicate rejected");
        assert_eq!(
            error,
            MediaCodecError::Quirk {
                line: 3,
                reason: QuirkSyntax::DuplicateKey("id"),
            }
        );
        let error =
            QuirkTable::parse("[[quirk]]\nstatus = \"maybe\"\n").expect_err("status rejected");
        assert!(matches!(
            error,
            MediaCodecError::Quirk {
                line: 2,
                reason: QuirkSyntax::BadValue { .. },
            }
        ));
        let error = QuirkTable::parse("[[quirk]]\nid = \"a\"\n").expect_err("partial rejected");
        assert_eq!(
            error,
            MediaCodecError::Quirk {
                line: 2,
                reason: QuirkSyntax::MissingField("soc"),
            }
        );
    }
}
