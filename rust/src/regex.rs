//! A bounded, safe ERE matcher with leftmost-longest matches.
//!
//! Positions and captures are byte offsets. UTF-8 is matched by scalar value;
//! an invalid byte is one opaque character matched by `.`. POSIX character
//! classes use the C locale, independent of the process locale. Like
//! `REG_NEWLINE`, `.` and negated brackets exclude newlines, and anchors match
//! each line. A search range is treated as an independent input slice.
//! An empty pattern is accepted and matches empty input positions; some
//! platform C regex implementations reject empty EREs.
//!
//! The Thompson VM keeps capture-distinct paths rather than backtracking.
//! Pattern size, nesting, expansion, active paths and total work are bounded;
//! exceeding a bound is an error, never a silently truncated result. Collating
//! elements, equivalence classes, backreferences and Perl extensions are rejected.

use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Range;

const MAX_PATTERN: usize = 65_536;
const MAX_DEPTH: usize = 64;
const MAX_REPEAT: usize = 1_000;
const MAX_PROGRAM: usize = 16_384;
const MAX_GROUPS: usize = 64;
const MAX_PATHS: usize = 16_384;
const MAX_WORK: usize = 8_000_000;
const MAX_CAPTURE_HISTORY: usize = 4_096;
const MAX_LIVE_HISTORY: usize = 262_144;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegexError {
    Syntax {
        offset: usize,
        message: &'static str,
    },
    Limit(&'static str),
    InvalidRange,
}

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { offset, message } => write!(f, "regex at byte {offset}: {message}"),
            Self::Limit(message) => write!(f, "regex resource limit: {message}"),
            Self::InvalidRange => f.write_str("invalid regex search range"),
        }
    }
}

impl std::error::Error for RegexError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub range: Range<usize>,
    /// Group zero is the entire match; unmatched groups are `None`.
    pub groups: Vec<Option<Range<usize>>>,
}

#[derive(Clone, Debug)]
enum Ast {
    Empty,
    Atom(Atom),
    Start,
    End,
    Sequence(Vec<Ast>),
    Alternate(Vec<Ast>),
    Repeat {
        node: Box<Ast>,
        min: usize,
        max: Option<usize>,
    },
    Group(usize, Box<Ast>),
}

#[derive(Clone, Debug)]
enum Atom {
    Literal(u32),
    Any,
    Class { negate: bool, members: Vec<Member> },
}

#[derive(Clone, Debug)]
enum Member {
    Range(u32, u32),
    Named(&'static str),
}

/// Native `REG_ICASE` in the C locale folds the ASCII letters only.
fn fold(value: u32) -> u32 {
    if (u32::from(b'a')..=u32::from(b'z')).contains(&value) {
        value - 32
    } else {
        value
    }
}

impl Atom {
    fn matches(&self, value: u32, ignorecase: bool) -> bool {
        // Native `REG_ICASE` folds the ASCII letters of the pattern and the
        // input; the named classes keep the case of the input.
        let folded = if ignorecase { fold(value) } else { value };
        match self {
            Self::Literal(expected) => {
                if ignorecase {
                    fold(*expected) == folded
                } else {
                    *expected == value
                }
            }
            Self::Any => value != u32::from(b'\n'),
            Self::Class { negate, members } => {
                let member = members.iter().any(|member| match member {
                    Member::Range(start, end) => {
                        (*start..=*end).contains(&value)
                            || (ignorecase && (fold(*start)..=fold(*end)).contains(&folded))
                    }
                    // The named classes keep the case of the input.
                    Member::Named(name) => named_class(name, value),
                });
                if *negate {
                    !member && value != u32::from(b'\n')
                } else {
                    member
                }
            }
        }
    }
}

fn named_class(name: &str, value: u32) -> bool {
    let Ok(byte) = u8::try_from(value) else {
        return false;
    };
    match name {
        "alnum" => byte.is_ascii_alphanumeric(),
        "alpha" => byte.is_ascii_alphabetic(),
        "blank" => matches!(byte, b' ' | b'\t'),
        "cntrl" => byte.is_ascii_control(),
        "digit" => byte.is_ascii_digit(),
        "graph" => byte.is_ascii_graphic(),
        "lower" => byte.is_ascii_lowercase(),
        "print" => byte.is_ascii_graphic() || byte == b' ',
        "punct" => byte.is_ascii_punctuation(),
        "space" => matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
        "upper" => byte.is_ascii_uppercase(),
        "xdigit" => byte.is_ascii_hexdigit(),
        _ => false,
    }
}

struct Parser<'a> {
    pattern: &'a str,
    pos: usize,
    groups: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.pattern[self.pos..].chars().next()
    }

    fn take(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.pos += value.len_utf8();
        Some(value)
    }

    fn error(&self, message: &'static str) -> RegexError {
        RegexError::Syntax {
            offset: self.pos,
            message,
        }
    }

    fn alternate(&mut self, depth: usize) -> Result<Ast, RegexError> {
        if depth > MAX_DEPTH {
            return Err(RegexError::Limit("nesting"));
        }
        let mut nodes = vec![self.sequence(depth)?];
        while self.peek() == Some('|') {
            self.take();
            nodes.push(self.sequence(depth)?);
        }
        Ok(if nodes.len() == 1 {
            nodes.pop().unwrap()
        } else {
            Ast::Alternate(nodes)
        })
    }

    fn sequence(&mut self, depth: usize) -> Result<Ast, RegexError> {
        let mut nodes = Vec::new();
        while !matches!(self.peek(), None | Some('|' | ')')) {
            nodes.push(self.repeated(depth)?);
        }
        Ok(match nodes.len() {
            0 => Ast::Empty,
            1 => nodes.pop().unwrap(),
            _ => Ast::Sequence(nodes),
        })
    }

    fn repeated(&mut self, depth: usize) -> Result<Ast, RegexError> {
        let mut node = self.atom(depth)?;
        let bounds = match self.peek() {
            Some('*') => {
                self.take();
                Some((0, None))
            }
            Some('+') => {
                self.take();
                Some((1, None))
            }
            Some('?') => {
                self.take();
                Some((0, Some(1)))
            }
            Some('{') => Some(self.bounds()?),
            _ => None,
        };
        if let Some((min, max)) = bounds {
            if matches!(node, Ast::Start | Ast::End) {
                return Err(self.error("quantified anchor"));
            }
            node = Ast::Repeat {
                node: Box::new(node),
                min,
                max,
            };
            if matches!(self.peek(), Some('*' | '+' | '?' | '{')) {
                return Err(self.error("repeated quantifier"));
            }
        }
        Ok(node)
    }

    fn bounds(&mut self) -> Result<(usize, Option<usize>), RegexError> {
        self.take();
        let min = self.number()?;
        let max = match self.take() {
            Some('}') => Some(min),
            Some(',') if self.peek() == Some('}') => {
                self.take();
                None
            }
            Some(',') => {
                let max = self.number()?;
                if self.take() != Some('}') {
                    return Err(self.error("unclosed repetition"));
                }
                if max < min {
                    return Err(self.error("reversed repetition bounds"));
                }
                Some(max)
            }
            _ => return Err(self.error("invalid repetition")),
        };
        Ok((min, max))
    }

    fn number(&mut self) -> Result<usize, RegexError> {
        let mut value = 0;
        let begin = self.pos;
        while let Some(digit) = self
            .peek()
            .and_then(|c| c.to_digit(10).filter(|_| c.is_ascii()))
        {
            self.take();
            value = value * 10 + digit as usize;
            if value > MAX_REPEAT {
                return Err(RegexError::Limit("repetition count"));
            }
        }
        if begin == self.pos {
            return Err(self.error("expected repetition count"));
        }
        Ok(value)
    }

    fn atom(&mut self, depth: usize) -> Result<Ast, RegexError> {
        Ok(
            match self
                .take()
                .ok_or_else(|| self.error("expected expression"))?
            {
                '(' => {
                    if self.peek() == Some('?') {
                        return Err(self.error("Perl groups are unsupported"));
                    }
                    self.groups += 1;
                    if self.groups > MAX_GROUPS {
                        return Err(RegexError::Limit("capture count"));
                    }
                    let group = self.groups;
                    let node = self.alternate(depth + 1)?;
                    if self.take() != Some(')') {
                        return Err(self.error("unclosed group"));
                    }
                    Ast::Group(group, Box::new(node))
                }
                '[' => Ast::Atom(self.class()?),
                '.' => Ast::Atom(Atom::Any),
                '^' => Ast::Start,
                '$' => Ast::End,
                '\\' => Ast::Atom(Atom::Literal(self.escaped()?)),
                '*' | '+' | '?' | '{' => return Err(self.error("quantifier without expression")),
                value => Ast::Atom(Atom::Literal(value as u32)),
            },
        )
    }

    fn escaped(&mut self) -> Result<u32, RegexError> {
        let value = self.take().ok_or_else(|| self.error("trailing escape"))?;
        Ok(match value {
            'n' => u32::from(b'\n'),
            't' => u32::from(b'\t'),
            'r' => u32::from(b'\r'),
            '0'..='9' => return Err(self.error("backreferences are unsupported")),
            // Escaped ordinary letters are undefined in POSIX; reject common
            // Perl shorthand rather than interpreting it as a literal letter.
            'a'..='z' | 'A'..='Z' => return Err(self.error("unsupported escape")),
            other => other as u32,
        })
    }

    fn class(&mut self) -> Result<Atom, RegexError> {
        let negate = self.peek() == Some('^');
        if negate {
            self.take();
        }
        let mut members = Vec::new();
        let mut first = true;
        loop {
            if self.peek().is_none() {
                return Err(self.error("unclosed bracket"));
            }
            if self.peek() == Some(']') && !first {
                self.take();
                break;
            }
            first = false;
            if self.pattern[self.pos..].starts_with("[:") {
                self.pos += 2;
                let start = self.pos;
                let Some(end) = self.pattern[start..].find(":]") else {
                    return Err(self.error("unclosed POSIX character class"));
                };
                let name = match &self.pattern[start..start + end] {
                    "alnum" => "alnum",
                    "alpha" => "alpha",
                    "blank" => "blank",
                    "cntrl" => "cntrl",
                    "digit" => "digit",
                    "graph" => "graph",
                    "lower" => "lower",
                    "print" => "print",
                    "punct" => "punct",
                    "space" => "space",
                    "upper" => "upper",
                    "xdigit" => "xdigit",
                    _ => return Err(self.error("unknown POSIX character class")),
                };
                self.pos = start + end + 2;
                members.push(Member::Named(name));
                continue;
            }
            if self.pattern[self.pos..].starts_with("[.")
                || self.pattern[self.pos..].starts_with("[=")
            {
                return Err(self.error("collating and equivalence classes are unsupported"));
            }
            let start = self.class_char()?;
            if self.peek() == Some('-') && !self.pattern[self.pos..].starts_with("-]") {
                self.take();
                let end = self.class_char()?;
                if end < start {
                    return Err(self.error("reversed bracket range"));
                }
                members.push(Member::Range(start, end));
            } else {
                members.push(Member::Range(start, start));
            }
        }
        Ok(Atom::Class { negate, members })
    }

    fn class_char(&mut self) -> Result<u32, RegexError> {
        match self.take() {
            Some('\\') => self.escaped(),
            Some(c) => Ok(c as u32),
            None => Err(self.error("unclosed bracket")),
        }
    }
}

#[derive(Clone, Debug)]
enum Instruction {
    Consume(Atom, usize),
    Split(usize, usize),
    Save(usize, usize),
    Start(usize),
    End(usize),
    Clear(Range<usize>, usize),
    Accept,
}

#[derive(Clone, Debug)]
pub struct Regex {
    program: Vec<Instruction>,
    entry: usize,
    groups: usize,
    /// Native `REG_ICASE`: the ASCII letters match regardless of case.
    ignorecase: bool,
}

impl Regex {
    pub fn new(pattern: &str) -> Result<Self, RegexError> {
        Self::with_options(pattern, false)
    }

    /// A pattern that matches the ASCII letters regardless of case, like the
    /// native `ignorecase` option.
    pub fn new_ignorecase(pattern: &str) -> Result<Self, RegexError> {
        Self::with_options(pattern, true)
    }

    fn with_options(pattern: &str, ignorecase: bool) -> Result<Self, RegexError> {
        if pattern.len() > MAX_PATTERN {
            return Err(RegexError::Limit("pattern bytes"));
        }
        let mut parser = Parser {
            pattern,
            pos: 0,
            groups: 0,
        };
        let ast = parser.alternate(0)?;
        if parser.pos != pattern.len() {
            return Err(parser.error("unexpected closing group"));
        }
        let mut regex = Self {
            program: vec![Instruction::Accept],
            entry: 0,
            groups: parser.groups,
            ignorecase,
        };
        let end = regex.emit(Instruction::Save(1, 0))?;
        let body = regex.compile(&ast, end)?;
        regex.entry = regex.emit(Instruction::Save(0, body))?;
        Ok(regex)
    }

    pub fn capture_count(&self) -> usize {
        self.groups
    }

    fn emit(&mut self, instruction: Instruction) -> Result<usize, RegexError> {
        if self.program.len() >= MAX_PROGRAM {
            return Err(RegexError::Limit("program size"));
        }
        self.program.push(instruction);
        Ok(self.program.len() - 1)
    }

    fn compile(&mut self, node: &Ast, next: usize) -> Result<usize, RegexError> {
        match node {
            Ast::Empty => Ok(next),
            Ast::Atom(atom) => self.emit(Instruction::Consume(atom.clone(), next)),
            Ast::Start => self.emit(Instruction::Start(next)),
            Ast::End => self.emit(Instruction::End(next)),
            Ast::Sequence(nodes) => {
                let mut entry = next;
                for node in nodes.iter().rev() {
                    entry = self.compile(node, entry)?;
                }
                Ok(entry)
            }
            Ast::Alternate(nodes) => {
                let mut entry = self.compile(nodes.last().unwrap(), next)?;
                for node in nodes[..nodes.len() - 1].iter().rev() {
                    let branch = self.compile(node, next)?;
                    entry = self.emit(Instruction::Split(branch, entry))?;
                }
                Ok(entry)
            }
            Ast::Group(group, node) => {
                let end = self.emit(Instruction::Save(group * 2 + 1, next))?;
                let entry = self.compile(node, end)?;
                let entry = self.emit(Instruction::Save(group * 2, entry))?;
                let last = last_group(node).max(*group);
                self.emit(Instruction::Clear((group + 1) * 2..(last + 1) * 2, entry))
            }
            Ast::Repeat { node, min, max } => {
                let mut entry = match max {
                    None => {
                        let split = self.emit(Instruction::Accept)?;
                        let body = self.compile(node, split)?;
                        self.program[split] = Instruction::Split(body, next);
                        split
                    }
                    Some(max) => {
                        let mut entry = next;
                        for _ in *min..*max {
                            let body = self.compile(node, entry)?;
                            entry = self.emit(Instruction::Split(body, entry))?;
                        }
                        entry
                    }
                };
                for _ in 0..*min {
                    entry = self.compile(node, entry)?;
                }
                Ok(entry)
            }
        }
    }

    /// Find the earliest match, preferring the longest match at that position.
    /// The range must lie inside `data`; offsets in the result are absolute.
    pub fn find(&self, data: &[u8], range: Range<usize>) -> Result<Option<Match>, RegexError> {
        let mut budget = MAX_WORK;
        self.find_with_budget(data, range, false, false, &mut budget)
    }

    /// Search with POSIX REG_NOTBOL / REG_NOTEOL equivalents. These suppress
    /// artificial anchors at range boundaries, while actual newlines still anchor.
    pub fn find_with_flags(
        &self,
        data: &[u8],
        range: Range<usize>,
        not_bol: bool,
        not_eol: bool,
    ) -> Result<Option<Match>, RegexError> {
        let mut budget = MAX_WORK;
        self.find_with_budget(data, range, not_bol, not_eol, &mut budget)
    }

    fn find_with_budget(
        &self,
        data: &[u8],
        range: Range<usize>,
        not_bol: bool,
        not_eol: bool,
        budget: &mut usize,
    ) -> Result<Option<Match>, RegexError> {
        if range.start > range.end || range.end > data.len() {
            return Err(RegexError::InvalidRange);
        }
        let mut start = range.start;
        loop {
            if let Some(found) = self.at(data, &range, start, not_bol, not_eol, budget)? {
                return Ok(Some(found));
            }
            if start == range.end {
                return Ok(None);
            }
            start = decode(data, start, range.end).1;
        }
    }

    /// Collect non-overlapping matches. An empty match advances one scalar,
    /// including invalid bytes, and an EOF empty match is returned only once.
    pub fn find_all(&self, data: &[u8], range: Range<usize>) -> Result<Vec<Match>, RegexError> {
        if range.start > range.end || range.end > data.len() {
            return Err(RegexError::InvalidRange);
        }
        let mut budget = MAX_WORK;
        let mut position = range.start;
        let mut matches = Vec::new();
        // Keep the original anchor boundaries while advancing the search.
        while position <= range.end {
            let mut start = position;
            let found = loop {
                if let Some(found) = self.at(data, &range, start, false, false, &mut budget)? {
                    break Some(found);
                }
                if start == range.end {
                    break None;
                }
                start = decode(data, start, range.end).1;
            };
            let Some(found) = found else { break };
            let empty = found.range.is_empty();
            position = found.range.end;
            matches.push(found);
            if empty {
                if position == range.end {
                    break;
                }
                position = decode(data, position, range.end).1;
            }
        }
        Ok(matches)
    }

    fn at(
        &self,
        data: &[u8],
        range: &Range<usize>,
        start: usize,
        not_bol: bool,
        not_eol: bool,
        budget: &mut usize,
    ) -> Result<Option<Match>, RegexError> {
        let mut seeds = vec![Thread {
            pc: self.entry,
            captures: vec![None; (self.groups + 1) * 2],
            history: vec![Vec::new(); self.groups],
        }];
        let mut position = start;
        let mut best: Option<Match> = None;
        let mut best_history = Vec::new();
        loop {
            let mut seen: HashSet<Thread> = HashSet::new();
            let mut live_history = 0;
            let mut consuming = Vec::new();
            while let Some(mut thread) = seeds.pop() {
                spend(budget)?;
                if let Some(old) = seen.get(&thread) {
                    if !history_preferred(&thread.history, &old.history) {
                        continue;
                    }
                }
                live_history += thread.history.iter().map(Vec::len).sum::<usize>();
                if live_history > MAX_LIVE_HISTORY {
                    return Err(RegexError::Limit("live capture history"));
                }
                seen.replace(thread.clone());
                if seen.len() > MAX_PATHS {
                    return Err(RegexError::Limit("active capture paths"));
                }
                match &self.program[thread.pc] {
                    Instruction::Accept => {
                        let groups: Vec<_> = thread
                            .captures
                            .chunks_exact(2)
                            .map(|pair| match (pair[0], pair[1]) {
                                (Some(begin), Some(end)) if begin <= end => Some(begin..end),
                                _ => None,
                            })
                            .collect();
                        let found = Match {
                            range: start..position,
                            groups,
                        };
                        if best.as_ref().is_none_or(|old| {
                            found.range.end > old.range.end
                                || (found.range.end == old.range.end
                                    && history_preferred(&thread.history, &best_history))
                        }) {
                            best = Some(found);
                            best_history = thread.history;
                        }
                    }
                    Instruction::Consume(_, _) => consuming.push(thread),
                    Instruction::Split(left, right) => {
                        let mut other = thread.clone();
                        other.pc = *right;
                        thread.pc = *left;
                        seeds.push(other);
                        seeds.push(thread);
                    }
                    Instruction::Save(slot, next) => {
                        thread.captures[*slot] = Some(position);
                        if *slot > 1 && slot % 2 == 1 {
                            if let Some(begin) = thread.captures[slot - 1] {
                                let history = &mut thread.history[slot / 2 - 1];
                                if history.len() >= MAX_CAPTURE_HISTORY {
                                    return Err(RegexError::Limit("capture history"));
                                }
                                history.push(begin..position);
                            }
                        }
                        thread.pc = *next;
                        seeds.push(thread);
                    }
                    Instruction::Clear(slots, next) => {
                        thread.captures[slots.clone()].fill(None);
                        thread.pc = *next;
                        seeds.push(thread);
                    }
                    Instruction::Start(next) => {
                        if (position == range.start && !not_bol)
                            || data.get(position.wrapping_sub(1)) == Some(&b'\n')
                        {
                            thread.pc = *next;
                            seeds.push(thread);
                        }
                    }
                    Instruction::End(next) => {
                        if (position == range.end && !not_eol) || data.get(position) == Some(&b'\n')
                        {
                            thread.pc = *next;
                            seeds.push(thread);
                        }
                    }
                }
            }
            if position == range.end || consuming.is_empty() {
                break;
            }
            let (value, next_position) = decode(data, position, range.end);
            for mut thread in consuming {
                spend(budget)?;
                let Instruction::Consume(atom, next) = &self.program[thread.pc] else {
                    unreachable!()
                };
                if atom.matches(value, self.ignorecase) {
                    thread.pc = *next;
                    seeds.push(thread);
                }
            }
            if seeds.is_empty() {
                break;
            }
            position = next_position;
        }
        Ok(best)
    }
}

#[derive(Clone, Debug)]
struct Thread {
    pc: usize,
    captures: Vec<Option<usize>>,
    history: Vec<Vec<Range<usize>>>,
}

// At a given instruction/position, identical live captures have identical
// possible futures. Retain only the preferred history for that configuration.
impl PartialEq for Thread {
    fn eq(&self, other: &Self) -> bool {
        self.pc == other.pc && self.captures == other.captures
    }
}
impl Eq for Thread {}
impl Hash for Thread {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pc.hash(state);
        self.captures.hash(state);
    }
}

fn spend(budget: &mut usize) -> Result<(), RegexError> {
    *budget = budget
        .checked_sub(1)
        .ok_or(RegexError::Limit("matching work"))?;
    Ok(())
}

fn history_preferred(new: &[Vec<Range<usize>>], old: &[Vec<Range<usize>>]) -> bool {
    // Opening-parenthesis order gives outer/earlier groups priority. For a
    // repeated group compare iterations in input order, not merely the final
    // capture: `(a|aa)*` on `aaa` prefers `aa,a` over `a,aa`.
    for (new, old) in new.iter().zip(old) {
        for (new, old) in new.iter().zip(old) {
            if new.len() != old.len() {
                return new.len() > old.len();
            }
            if new.start != old.start {
                return new.start < old.start;
            }
        }
        if new.len() != old.len() {
            if old.is_empty() {
                return true;
            }
            if new.is_empty() {
                return false;
            }
            // Nonempty extra iterations extend the repeated subexpression;
            // empty iterations cannot improve it and must not cycle forever.
            if new.len() > old.len() {
                return new[old.len()..].iter().any(|range| !range.is_empty());
            }
            return old[new.len()..].iter().all(Range::is_empty);
        }
    }
    false
}

fn last_group(node: &Ast) -> usize {
    match node {
        Ast::Group(group, inner) => (*group).max(last_group(inner)),
        Ast::Sequence(nodes) | Ast::Alternate(nodes) => {
            nodes.iter().map(last_group).max().unwrap_or(0)
        }
        Ast::Repeat { node, .. } => last_group(node),
        _ => 0,
    }
}

fn decode(data: &[u8], pos: usize, end: usize) -> (u32, usize) {
    let byte = data[pos];
    let width = match byte {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    };
    if width > 0 && pos + width <= end {
        if let Ok(text) = std::str::from_utf8(&data[pos..pos + width]) {
            return (text.chars().next().unwrap() as u32, pos + width);
        }
    }
    (0x11_0000 + u32::from(byte), pos + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_case_insensitive_mode_folds_the_ascii_letters() {
        let data = b"Hello World\n";
        let sensitive = Regex::new("world").unwrap();
        assert!(sensitive.find(data, 0..data.len()).unwrap().is_none());
        let insensitive = Regex::new_ignorecase("world").unwrap();
        let found = insensitive.find(data, 0..data.len()).unwrap().unwrap();
        assert_eq!(found.range, 6..11);
        // The named classes keep their case, like POSIX: the folded input
        // never matches a lowercase class.
        let lower = Regex::new_ignorecase("[[:lower:]]+").unwrap();
        let found = lower.find(data, 0..data.len()).unwrap().unwrap();
        assert_eq!(found.range, 1..5);
    }

    fn find(pattern: &str, data: &[u8]) -> Option<Match> {
        Regex::new(pattern)
            .unwrap()
            .find(data, 0..data.len())
            .unwrap()
    }

    #[test]
    fn leftmost_longest_and_capture_precedence() {
        assert_eq!(find("a|aaaa", b"zaaaa").unwrap().range, 1..5);
        let found = find("(a|aa)(a?)", b"aa").unwrap();
        assert_eq!(found.groups, vec![Some(0..2), Some(0..2), Some(2..2)]);
        assert_eq!(find("(a)?b", b"b").unwrap().groups, vec![Some(0..1), None]);
        assert_eq!(
            find("((ab)+)", b"abab").unwrap().groups,
            vec![Some(0..4), Some(0..4), Some(2..4)]
        );
    }

    #[test]
    fn newline_anchors_and_range_boundaries() {
        let data = b"one\ntwo\n\nthree";
        assert_eq!(find("^two$", data).unwrap().range, 4..7);
        assert_eq!(find("[^x]*", b"ab\ncd").unwrap().range, 0..2);
        assert_eq!(find(".*", b"a\nb").unwrap().range, 0..1);
        assert_eq!(
            Regex::new("^two$")
                .unwrap()
                .find(data, 4..7)
                .unwrap()
                .unwrap()
                .range,
            4..7
        );
        assert_eq!(find("^$", data).unwrap().range, 8..8);
        assert!(find("^three$", data).is_some());
    }

    #[test]
    fn nested_groups_reset_on_each_repetition() {
        assert_eq!(
            find("(a(b)?)+", b"aba").unwrap().groups,
            vec![Some(0..3), Some(2..3), None]
        );
        assert_eq!(
            find("(a|aa)*a?", b"aaa").unwrap().groups,
            vec![Some(0..3), Some(2..3)]
        );
    }

    #[test]
    fn search_flags_suppress_artificial_boundary_anchors() {
        let regex = Regex::new("^").unwrap();
        assert_eq!(
            regex
                .find_with_flags(b"a\nb\n", 1..4, true, false)
                .unwrap()
                .unwrap()
                .range,
            2..2
        );
        assert_eq!(
            regex
                .find_with_flags(b"a\nb\n", 2..4, true, false)
                .unwrap()
                .unwrap()
                .range,
            2..2
        );
        assert!(
            Regex::new("$")
                .unwrap()
                .find_with_flags(b"abcd", 0..2, false, true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unicode_and_binary_data() {
        assert_eq!(find("(.)+", "♪♫".as_bytes()).unwrap().range, 0..6);
        assert_eq!(find("[α-ω]+", "AαβωZ".as_bytes()).unwrap().range, 1..7);
        assert_eq!(find("a.b", b"a\0b").unwrap().range, 0..3);
        assert_eq!(find("...", &[0xff, 0x80, b'x']).unwrap().range, 0..3);
        assert_eq!(find("♪", "♫♪".as_bytes()).unwrap().range, 3..6);
    }

    #[test]
    fn repetitions_and_classes() {
        assert_eq!(find("a{2,4}", b"aaaaaa").unwrap().range, 0..4);
        assert_eq!(find("a{2,}", b"aaaaaa").unwrap().range, 0..6);
        assert_eq!(find("ab?c+", b"accc").unwrap().range, 0..4);
        assert_eq!(
            find("[[:alpha:]_][[:alnum:]_]*", b"9 foo_12!")
                .unwrap()
                .range,
            2..8
        );
        assert_eq!(find("[]-]+", b"]--x").unwrap().range, 0..3);
        assert_eq!(find("[[:space:]]", b"\x0b").unwrap().range, 0..1);
        assert!(find("[[:alpha:]]", &[0xe9]).is_none());
    }

    #[test]
    fn empty_matches_progress_and_keep_anchor_context() {
        let regex = Regex::new("").unwrap();
        assert_eq!(
            regex
                .find_all("♪x".as_bytes(), 0..4)
                .unwrap()
                .iter()
                .map(|m| m.range.clone())
                .collect::<Vec<_>>(),
            vec![0..0, 3..3, 4..4]
        );
        assert_eq!(
            Regex::new("^")
                .unwrap()
                .find_all(b"ab\ncd", 0..5)
                .unwrap()
                .iter()
                .map(|m| m.range.clone())
                .collect::<Vec<_>>(),
            vec![0..0, 3..3]
        );
        assert_eq!(
            find("(a*)*", b"aaa").unwrap().groups,
            vec![Some(0..3), Some(0..3)]
        );
        assert_eq!(
            find("(a|aa)*", b"aaa").unwrap().groups,
            vec![Some(0..3), Some(2..3)]
        );
        assert_eq!(find("", b"").unwrap().range, 0..0);
    }

    #[test]
    fn malformed_or_unsupported_patterns_are_errors() {
        for pattern in [
            "(",
            ")",
            "[",
            "[z-a]",
            "*x",
            "a**",
            "a{3,2}",
            "a{,2}",
            "a{2",
            "\\",
            "\\1",
            "\\d",
            "(?=a)",
            "[[:unknown:]]",
            "[[.a.]]",
            "[[=a=]]",
        ] {
            assert!(Regex::new(pattern).is_err(), "{pattern}");
        }
        assert!(matches!(Regex::new("a{1001}"), Err(RegexError::Limit(_))));
        assert!(matches!(
            Regex::new(&format!("{}a{}", "(".repeat(65), ")".repeat(65))),
            Err(RegexError::Limit(_))
        ));
        assert!(matches!(
            Regex::new("a").unwrap().find(b"a", 0..2),
            Err(RegexError::InvalidRange)
        ));
    }

    #[test]
    fn work_is_bounded() {
        let regex = Regex::new("(a|aa)*b").unwrap();
        let mut budget = 100;
        assert!(matches!(
            regex.find_with_budget(&[b'a'; 100], 0..100, false, false, &mut budget),
            Err(RegexError::Limit(_))
        ));
    }
}
