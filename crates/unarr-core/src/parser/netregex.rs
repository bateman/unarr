//! .NET-flavoured regular expressions on top of `fancy-regex`.
//!
//! The originals' parser is a few hundred .NET patterns, which Unarr keeps
//! verbatim so the ported test vectors stay the specification. Two .NET
//! behaviours have no counterpart in Rust engines and are emulated here:
//!
//! - **Duplicate group names.** `(?<title>.+?) \((?<title>.+?)\)` is one
//!   group with two captures in .NET. The pattern is rewritten with unique
//!   names (`title`, `title__2`, ...) and [`NetCaptures::value`] returns the
//!   last one that took part, as `Group.Value` does.
//! - **Captures of repeated groups.** `(?:E(?<episode>\d+))+` collects every
//!   iteration in .NET (`Group.Captures`); Rust keeps the last. Any quantified
//!   group that contains a named group is unrolled into nested optional
//!   copies (`G(?:G(?:G)?)?`), so each iteration has its own group and
//!   [`NetCaptures::all`] lists them in order. Up to [`MAX_ITERATIONS`]
//!   iterations are kept, which covers any release name seen in the wild.
//!
//! `\uXXXX` escapes are converted and numbered groups follow the .NET rule
//! (unnamed groups first, then named), so `$1` in a replacement means what
//! it meant.

use crate::sync::Recover;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

/// Iterations of a repeated capturing group kept apart, as `Captures` would.
pub const MAX_ITERATIONS: usize = 10;

/// A pattern with its .NET group bookkeeping. The compiled automaton is
/// built when the pattern is first matched and may be let go again once
/// it has not been used for a while ([`release_idle`]): a parser's few
/// hundred patterns weigh tens of megabytes compiled, and are needed for
/// a second every quarter of an hour.
pub struct NetRegex {
    /// The pattern as the engine takes it: groups renamed and unrolled.
    full: String,
    /// Compiled group indices by .NET name, in textual order.
    names: HashMap<String, Vec<usize>>,
    /// Ordered .NET names, for `$N` numbering of named groups.
    numbered: Vec<usize>,
    slot: Arc<Slot>,
}

/// The compiled automaton of one pattern, while it is kept.
struct Slot {
    re: Mutex<Option<Arc<fancy_regex::Regex>>>,
    /// Seconds since the process started when the pattern last matched.
    last_used: AtomicU64,
}

/// Every pattern compiled so far, for [`release_idle`] to walk.
fn registry() -> &'static Mutex<Vec<Weak<Slot>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Weak<Slot>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Seconds since the first pattern was used.
fn uptime() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs()
}

/// Let go of the compiled automata of the patterns nothing matched with
/// for `max_idle`; they are compiled again when next used. Returns how
/// many were released.
pub fn release_idle(max_idle: Duration) -> usize {
    let now = uptime();
    let mut released = 0;
    let mut slots = registry().lock().recover();
    slots.retain(|weak| {
        let Some(slot) = weak.upgrade() else {
            return false;
        };
        if now.saturating_sub(slot.last_used.load(Ordering::Relaxed)) >= max_idle.as_secs() {
            // Dropped from the registry too: compiling again adds it back.
            if slot.re.lock().recover().take().is_some() {
                released += 1;
            }
            return false;
        }
        true
    });
    released
}

/// The engine's seek pre-filter is on: a pattern with lookarounds runs on
/// the backtracking VM, which otherwise tries every position of the text;
/// with seek it first asks a regular approximation of the pattern, which
/// never rejects a position where the pattern matches, where a match can
/// start. Parsing a release name takes about half the time.
fn compile(full: &str) -> Result<fancy_regex::Regex, String> {
    fancy_regex::RegexBuilder::new(full)
        .backtrack_limit(10_000_000)
        .seek(true)
        .build()
        .map_err(|e| e.to_string())
}

impl std::fmt::Debug for NetRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NetRegex({})", self.full)
    }
}

/// A pattern compiled on first use, for `static` tables.
pub struct Lazy {
    pattern: &'static str,
    ignore_case: bool,
    cell: OnceLock<NetRegex>,
}

impl Lazy {
    pub const fn new(pattern: &'static str, ignore_case: bool) -> Lazy {
        Lazy {
            pattern,
            ignore_case,
            cell: OnceLock::new(),
        }
    }

    pub fn get(&self) -> &NetRegex {
        self.cell.get_or_init(|| {
            NetRegex::new(self.pattern, self.ignore_case)
                .unwrap_or_else(|e| panic!("pattern {:?}: {e}", self.pattern))
        })
    }
}

impl std::ops::Deref for Lazy {
    type Target = NetRegex;
    fn deref(&self) -> &NetRegex {
        self.get()
    }
}

/// Case-insensitive pattern table entry.
#[macro_export]
macro_rules! net_regex {
    ($pattern:expr) => {
        $crate::parser::netregex::Lazy::new($pattern, true)
    };
    ($pattern:expr, case_sensitive) => {
        $crate::parser::netregex::Lazy::new($pattern, false)
    };
}

// --- pattern rewriting

/// A flat view of the pattern: literal runs and groups with their quantifier.
#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Group {
        /// `(`, `(?:`, `(?<name>`, `(?=`, `(?<!`, `(?>`, `(?-i:`...
        prefix: String,
        children: Vec<Node>,
        /// `+`, `*?`, `{2,}`... as written, or empty.
        quant: String,
    },
}

struct Scanner<'a> {
    chars: Vec<char>,
    pos: usize,
    _src: &'a str,
}

impl Scanner<'_> {
    fn parse(&mut self, depth: usize) -> Result<Vec<Node>, String> {
        let mut out = Vec::new();
        let mut text = String::new();
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            match c {
                '\\' => {
                    text.push(c);
                    self.pos += 1;
                    if self.pos < self.chars.len() {
                        text.push(self.chars[self.pos]);
                        self.pos += 1;
                    }
                }
                '[' => {
                    let start = self.pos;
                    self.pos += 1;
                    if self.pos < self.chars.len() && self.chars[self.pos] == '^' {
                        self.pos += 1;
                    }
                    if self.pos < self.chars.len() && self.chars[self.pos] == ']' {
                        self.pos += 1;
                    }
                    while self.pos < self.chars.len() && self.chars[self.pos] != ']' {
                        if self.chars[self.pos] == '\\' {
                            self.pos += 1;
                        }
                        self.pos += 1;
                    }
                    self.pos += 1; // the closing bracket
                    text.extend(&self.chars[start..self.pos.min(self.chars.len())]);
                }
                '(' => {
                    // Inline option setting such as `(?i)` is not a group.
                    if self.lookahead_is_option_group() {
                        let start = self.pos;
                        while self.chars.get(self.pos).is_some_and(|c| *c != ')') {
                            self.pos += 1;
                        }
                        self.pos += 1;
                        text.extend(&self.chars[start..self.pos]);
                        continue;
                    }
                    if !text.is_empty() {
                        out.push(Node::Text(std::mem::take(&mut text)));
                    }
                    let prefix = self.group_prefix();
                    let children = self.parse(depth + 1)?;
                    if self.pos >= self.chars.len() || self.chars[self.pos] != ')' {
                        return Err("unbalanced parenthesis".into());
                    }
                    self.pos += 1;
                    let quant = self.quantifier();
                    out.push(Node::Group {
                        prefix,
                        children,
                        quant,
                    });
                }
                ')' => {
                    if depth == 0 {
                        return Err("unexpected )".into());
                    }
                    break;
                }
                _ => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
        if !text.is_empty() {
            out.push(Node::Text(text));
        }
        Ok(out)
    }

    fn lookahead_is_option_group(&self) -> bool {
        // `(?` followed by flag letters and `-`, then `)`.
        if self.chars.get(self.pos + 1) != Some(&'?') {
            return false;
        }
        let mut i = self.pos + 2;
        let mut any = false;
        while let Some(c) = self.chars.get(i) {
            if c.is_ascii_alphabetic() || *c == '-' {
                any = true;
                i += 1;
            } else {
                return any && *c == ')';
            }
        }
        false
    }

    /// Consume the opening of a group and return it as written.
    fn group_prefix(&mut self) -> String {
        let start = self.pos;
        self.pos += 1; // (
        if self.chars.get(self.pos) == Some(&'?') {
            self.pos += 1;
            match self.chars.get(self.pos) {
                Some('<') | Some('P') => {
                    // (?<name>, (?P<name>, (?<=, (?<!
                    if self.chars.get(self.pos) == Some(&'P') {
                        self.pos += 1;
                    }
                    self.pos += 1;
                    match self.chars.get(self.pos) {
                        Some('=') | Some('!') => self.pos += 1,
                        _ => {
                            while self.chars.get(self.pos).is_some_and(|c| *c != '>') {
                                self.pos += 1;
                            }
                            self.pos += 1;
                        }
                    }
                }
                Some(':') | Some('=') | Some('!') | Some('>') => self.pos += 1,
                _ => {
                    // (?i:  (?-i:  flags scoped to the group
                    while self.chars.get(self.pos).is_some_and(|c| *c != ':') {
                        self.pos += 1;
                    }
                    self.pos += 1;
                }
            }
        }
        // An unfinished group leaves `pos` past the end.
        self.chars[start..self.pos.min(self.chars.len())].iter().collect()
    }

    fn quantifier(&mut self) -> String {
        let start = self.pos;
        match self.chars.get(self.pos) {
            Some('*') | Some('+') | Some('?') => self.pos += 1,
            Some('{') => {
                let mut i = self.pos + 1;
                let mut ok = false;
                while let Some(c) = self.chars.get(i) {
                    if *c == '}' {
                        ok = true;
                        break;
                    }
                    if !c.is_ascii_digit() && *c != ',' {
                        break;
                    }
                    i += 1;
                }
                if ok {
                    self.pos = i + 1;
                } else {
                    return String::new();
                }
            }
            _ => return String::new(),
        }
        if matches!(self.chars.get(self.pos), Some('?') | Some('+')) {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }
}

fn has_named_group(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Text(_) => false,
        Node::Group { prefix, children, .. } => is_named(prefix) || has_named_group(children),
    })
}

fn is_named(prefix: &str) -> bool {
    (prefix.starts_with("(?<") && !prefix.starts_with("(?<=") && !prefix.starts_with("(?<!"))
        || prefix.starts_with("(?P<")
}

/// `(min, max, lazy)` of a quantifier; `max` is `None` when unbounded.
fn bounds(quant: &str) -> Option<(usize, Option<usize>, bool)> {
    let lazy = quant.ends_with('?') && quant.len() > 1;
    let core = quant.trim_end_matches(['?', '+']);
    let core = if quant.len() == 1 { quant } else { core };
    match core {
        "" => None,
        "*" => Some((0, None, lazy)),
        "+" => Some((1, None, lazy)),
        "?" => Some((0, Some(1), lazy)),
        _ => {
            let inner = core.trim_start_matches('{').trim_end_matches('}');
            let (lo, hi) = match inner.split_once(',') {
                Some((lo, "")) => (lo.parse().ok()?, None),
                Some((lo, hi)) => (lo.parse().ok()?, Some(hi.parse().ok()?)),
                None => {
                    let n = inner.parse().ok()?;
                    (n, Some(n))
                }
            };
            Some((lo, hi, lazy))
        }
    }
}

fn render(nodes: &[Node], out: &mut String) {
    for n in nodes {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Group {
                prefix,
                children,
                quant,
            } => {
                let repeated = has_named_group(children) || is_named(prefix);
                match bounds(quant) {
                    Some((min, max, lazy)) if repeated && max != Some(1) => {
                        let mut one = String::new();
                        one.push_str(prefix);
                        render(children, &mut one);
                        one.push(')');
                        let cap = max
                            .unwrap_or(MAX_ITERATIONS)
                            .max(min)
                            .min(MAX_ITERATIONS.max(min));
                        for _ in 0..min {
                            out.push_str(&one);
                        }
                        let optional = cap - min;
                        let lazy_mark = if lazy { "??" } else { "?" };
                        for _ in 0..optional {
                            out.push_str("(?:");
                            out.push_str(&one);
                        }
                        for _ in 0..optional {
                            out.push(')');
                            out.push_str(lazy_mark);
                        }
                    }
                    _ => {
                        out.push_str(prefix);
                        render(children, out);
                        out.push(')');
                        out.push_str(quant);
                    }
                }
            }
        }
    }
}

/// `\uXXXX` to `\x{XXXX}`.
fn convert_unicode_escapes(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\'
            && i + 6 <= chars.len()
            && chars[i + 1] == 'u'
            && chars[i + 2..i + 6].iter().all(|c| c.is_ascii_hexdigit())
        {
            out.push_str("\\x{");
            out.extend(&chars[i + 2..i + 6]);
            out.push('}');
            i += 6;
        } else if chars[i] == '\\' && i + 1 < chars.len() {
            out.push(chars[i]);
            out.push(chars[i + 1]);
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Rename duplicate group names and record .NET numbering.
fn rename_groups(pattern: &str) -> (String, HashMap<String, Vec<usize>>, Vec<usize>) {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    let mut names: HashMap<String, Vec<usize>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut unnamed: Vec<usize> = Vec::new();
    let mut named_in_order: Vec<usize> = Vec::new();
    let mut index = 0usize;
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            out.push(c);
            if i + 1 < chars.len() {
                out.push(chars[i + 1]);
            }
            i += 2;
            continue;
        }
        if in_class {
            if c == ']' {
                in_class = false;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '[' {
            in_class = true;
            out.push(c);
            i += 1;
            if chars.get(i) == Some(&'^') {
                out.push('^');
                i += 1;
            }
            if chars.get(i) == Some(&']') {
                out.push(']');
                i += 1;
            }
            continue;
        }
        if c == '(' {
            if chars.get(i + 1) != Some(&'?') {
                index += 1;
                unnamed.push(index);
                out.push(c);
                i += 1;
                continue;
            }
            let named_start =
                if chars.get(i + 2) == Some(&'<') && !matches!(chars.get(i + 3), Some('=') | Some('!')) {
                    Some(i + 3)
                } else if chars.get(i + 2) == Some(&'P') && chars.get(i + 3) == Some(&'<') {
                    Some(i + 4)
                } else {
                    None
                };
            if let Some(start) = named_start {
                let mut j = start;
                while chars.get(j).is_some_and(|c| *c != '>') {
                    j += 1;
                }
                let name: String = chars[start..j].iter().collect();
                index += 1;
                let entry = names.entry(name.clone()).or_default();
                if entry.is_empty() {
                    order.push(name.clone());
                }
                entry.push(index);
                named_in_order.push(index);
                let unique = if entry.len() == 1 {
                    name.clone()
                } else {
                    format!("{name}__{}", entry.len())
                };
                out.push_str("(?<");
                out.push_str(&unique);
                out.push('>');
                i = j + 1;
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    // .NET numbers unnamed groups first, then one number per distinct name.
    let mut numbered = vec![0];
    numbered.extend(unnamed);
    for name in &order {
        numbered.push(*names[name].last().unwrap());
    }
    let _ = named_in_order;
    (out, names, numbered)
}

impl NetRegex {
    pub fn new(pattern: &str, ignore_case: bool) -> Result<NetRegex, String> {
        let converted = convert_unicode_escapes(pattern);
        let mut scanner = Scanner {
            chars: converted.chars().collect(),
            pos: 0,
            _src: &converted,
        };
        let nodes = scanner.parse(0)?;
        let mut unrolled = String::new();
        render(&nodes, &mut unrolled);
        let (renamed, names, numbered) = rename_groups(&unrolled);
        let full = if ignore_case {
            format!("(?i){renamed}")
        } else {
            renamed
        };
        // Compiled once here, so that a pattern the engine refuses is
        // refused when it is built and never later.
        let re = compile(&full)?;
        let slot = Arc::new(Slot {
            re: Mutex::new(Some(Arc::new(re))),
            last_used: AtomicU64::new(uptime()),
        });
        registry().lock().recover().push(Arc::downgrade(&slot));
        Ok(NetRegex {
            full,
            names,
            numbered,
            slot,
        })
    }

    /// The compiled automaton, built again if it was let go.
    fn re(&self) -> Arc<fancy_regex::Regex> {
        self.slot.last_used.store(uptime(), Ordering::Relaxed);
        let mut held = self.slot.re.lock().recover();
        if let Some(re) = held.as_ref() {
            return re.clone();
        }
        let re = Arc::new(compile(&self.full).expect("a pattern that compiled once compiles again"));
        *held = Some(re.clone());
        // The slot's lock is let go before the registry's is taken: the
        // release of idle patterns takes them the other way round.
        drop(held);
        registry().lock().recover().push(Arc::downgrade(&self.slot));
        re
    }

    pub fn as_str(&self) -> &str {
        &self.full
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.re().is_match(text).unwrap_or(false)
    }

    pub fn captures<'t>(&self, text: &'t str) -> Option<NetCaptures<'_, 't>> {
        self.re()
            .captures(text)
            .ok()
            .flatten()
            .map(|caps| NetCaptures { re: self, caps })
    }

    pub fn captures_iter<'t>(&self, text: &'t str) -> Vec<NetCaptures<'_, 't>> {
        self.re()
            .captures_iter(text)
            .filter_map(|c| c.ok())
            .map(|caps| NetCaptures { re: self, caps })
            .collect()
    }

    /// `Regex.Replace` with a .NET replacement string (`$1`, `${name}`, `$$`).
    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        self.replace_all_with(text, |caps| expand(replacement, caps))
    }

    /// `Regex.Replace` with an evaluator; `f` gets each match and returns
    /// the text that stands in for it.
    pub fn replace_all_with(&self, text: &str, f: impl Fn(&NetCaptures<'_, '_>) -> String) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for caps in self.captures_iter(text) {
            let m = caps.caps.get(0).unwrap();
            out.push_str(&text[last..m.start()]);
            out.push_str(&f(&caps));
            last = m.end();
        }
        out.push_str(&text[last..]);
        out
    }

    /// `RegexReplace.TryReplace`: replace, and say whether anything matched.
    pub fn try_replace(&self, text: &mut String, replacement: &str) -> bool {
        if self.is_match(text) {
            *text = self.replace_all(text, replacement);
            true
        } else {
            false
        }
    }
}

fn expand(replacement: &str, caps: &NetCaptures<'_, '_>) -> String {
    let mut out = String::new();
    let chars: Vec<char> = replacement.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            if chars[i + 1] == '$' {
                out.push('$');
                i += 2;
                continue;
            }
            if chars[i + 1] == '{' {
                if let Some(end) = chars[i + 2..].iter().position(|c| *c == '}') {
                    let name: String = chars[i + 2..i + 2 + end].iter().collect();
                    match name.parse::<usize>() {
                        Ok(n) => out.push_str(caps.numbered(n).unwrap_or("")),
                        Err(_) => out.push_str(caps.value(&name).unwrap_or("")),
                    }
                    i += 3 + end;
                    continue;
                }
            }
            if chars[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                let n: usize = chars[i + 1..j].iter().collect::<String>().parse().unwrap_or(0);
                out.push_str(caps.numbered(n).unwrap_or(""));
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// One match, with .NET group semantics.
pub struct NetCaptures<'r, 't> {
    re: &'r NetRegex,
    caps: fancy_regex::Captures<'t, str>,
}

impl<'t> NetCaptures<'_, 't> {
    /// The whole match.
    pub fn whole(&self) -> &'t str {
        self.caps.get(0).map(|m| m.as_str()).unwrap_or("")
    }

    pub fn start(&self) -> usize {
        self.caps.get(0).map(|m| m.start()).unwrap_or(0)
    }

    pub fn end(&self) -> usize {
        self.caps.get(0).map(|m| m.end()).unwrap_or(0)
    }

    /// `Groups[name].Value`: the last capture of the group, if it took part.
    pub fn value(&self, name: &str) -> Option<&'t str> {
        self.re
            .names
            .get(name)?
            .iter()
            .rev()
            .find_map(|i| self.caps.get(*i))
            .map(|m| m.as_str())
    }

    /// `Groups[name].Success`.
    pub fn success(&self, name: &str) -> bool {
        self.value(name).is_some()
    }

    /// `Groups[name].EndIndex()`: byte offset where the last capture ends.
    pub fn end_of(&self, name: &str) -> Option<usize> {
        self.re
            .names
            .get(name)?
            .iter()
            .rev()
            .find_map(|i| self.caps.get(*i))
            .map(|m| m.end())
    }

    /// `Groups[name].Captures`: every capture in order, with its end offset.
    pub fn all(&self, name: &str) -> Vec<(&'t str, usize)> {
        self.re
            .names
            .get(name)
            .map(|idx| {
                idx.iter()
                    .filter_map(|i| self.caps.get(*i))
                    .map(|m| (m.as_str(), m.end()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `Groups[n].Value` by .NET number.
    pub fn numbered(&self, n: usize) -> Option<&'t str> {
        let idx = *self.re.numbered.get(n)?;
        self.caps.get(idx).map(|m| m.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_released_pattern_is_compiled_again_and_matches_the_same() {
        let re = NetRegex::new(r"(?<title>.+?)[. ](?<year>(19|20)\d{2})", true).unwrap();
        let before = re.captures("The.Movie.2019.1080p").map(|c| {
            (
                c.value("title").map(str::to_string),
                c.value("year").map(str::to_string),
            )
        });
        assert_eq!(before, Some((Some("The.Movie".into()), Some("2019".into()))));
        // Other tests compile patterns of their own meanwhile: only this
        // one's state is looked at.
        release_idle(Duration::ZERO);
        assert!(re.slot.re.lock().recover().is_none(), "the automaton was let go");
        let after = re.captures("The.Movie.2019.1080p").map(|c| {
            (
                c.value("title").map(str::to_string),
                c.value("year").map(str::to_string),
            )
        });
        assert_eq!(after, before);
        assert!(re.slot.re.lock().recover().is_some(), "and built again on use");
        // A pattern in use is kept.
        assert!(re.is_match("A.Movie.2001"));
        release_idle(Duration::from_secs(3600));
        assert!(re.slot.re.lock().recover().is_some());
    }

    /// A release profile term can hold any text; an unfinished group must
    /// fail to compile, not stop the daemon.
    #[test]
    fn unfinished_groups_fail_instead_of_panicking() {
        for pattern in ["(?i", "(?<x", "(?<x>", "(?", "(?i:abc", "(abc", "[abc", "abc\\"] {
            let _ = NetRegex::new(pattern, false);
        }
    }

    #[test]
    fn duplicate_names_and_repeated_groups_behave_like_dotnet() {
        let re = NetRegex::new(r"^(?<title>.+?) \((?<title>.+?)\)$", true).unwrap();
        let c = re.captures("Show (Other)").unwrap();
        assert_eq!(c.value("title"), Some("Other"));
        assert_eq!(
            c.all("title").iter().map(|x| x.0).collect::<Vec<_>>(),
            vec!["Show", "Other"]
        );

        let re = NetRegex::new(
            r"^(?<title>.+?)S(?<season>\d{2})(?:E(?<episode>\d{2}))+(?:[-_. ]|$)",
            true,
        )
        .unwrap();
        let c = re.captures("Show.S01E05E06E07.720p").unwrap();
        let eps: Vec<&str> = c.all("episode").iter().map(|x| x.0).collect();
        assert_eq!(eps, vec!["05", "06", "07"]);
        assert_eq!(c.value("episode"), Some("07"));
        assert_eq!(c.end_of("episode"), Some("Show.S01E05E06E07".len()));

        // Nested repetition: S01E05.S01E06 collects both episodes.
        let re = NetRegex::new(
            r"^(?:\W*S(?<season>\d{1,2})(?:e{1,2}(?<episode>\d{1,3}(?!\d+)))+){2,}",
            true,
        )
        .unwrap();
        let c = re.captures("S01E05.S01E06").unwrap();
        let eps: Vec<&str> = c.all("episode").iter().map(|x| x.0).collect();
        assert_eq!(eps, vec!["05", "06"]);
        assert_eq!(c.all("season").len(), 2);
    }

    #[test]
    fn replacement_follows_dotnet_numbering_and_names() {
        let re = NetRegex::new(r"\.E(\d{2,4})\.\d{6}\.(.*-NEXT)$", false).unwrap();
        assert_eq!(
            re.replace_all("Show.E03.240101.1080p-NEXT", ".S01E$1.$2"),
            "Show.S01E03.1080p-NEXT"
        );
        let re = NetRegex::new(r"^\[(?<subgroup>[^\]]+)\]\[(?<title>[^\]]+?)\]", false).unwrap();
        assert_eq!(
            re.replace_all("[Grp][Title] rest", "[${subgroup}] ${title} - "),
            "[Grp] Title -  rest"
        );
        let mut s = "abc".to_string();
        assert!(!re.try_replace(&mut s, "x"));
        assert_eq!(s, "abc");
    }

    #[test]
    fn unicode_escapes_and_classes_survive() {
        let re = NetRegex::new(r"[一-鿌]+|[^\]]+", false).unwrap();
        assert!(re.is_match("第1话"));
        let re = NetRegex::new(r"(?<x>[)(])+", false).unwrap();
        assert_eq!(re.captures("))((").unwrap().all("x").len(), 4);
    }
}
