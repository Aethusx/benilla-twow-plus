//! `luasyntax/Transpile.cpp`: Lua 5.1 syntax rewritten into 1.12's Lua 5.0 before it compiles.
//!
//! - `#operand` -> `__len(operand)`
//! - `a % b` -> `__mod(a, b)`, delimited by precedence
//! - `...` as an expression -> `unpack(arg)` (the parameter-list `...` stays)
//! - `0xHH` integer literals -> decimal
//! - leveled long brackets `[=[ … ]=]` -> `[[ … ]]` or a quoted literal; leveled comments blanked
//!
//! One allocation-free lex decides whether any trigger occurs outside strings and comments; only
//! then is the chunk tokenized and rewritten. No newline is ever added, so line numbers hold.

/// The runtime switches (`_classicapi_SetTranspileOption`), all on by default.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub len: bool,
    pub modulo: bool,
    pub vararg: bool,
    pub hex: bool,
    pub long_brackets: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            len: true,
            modulo: true,
            vararg: true,
            hex: true,
            long_brackets: true,
        }
    }
}

impl Options {
    /// The switch a `_classicapi_*TranspileOption` name selects, case-insensitively.
    pub fn flag(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name.to_ascii_lowercase().as_str() {
            "length" => &mut self.len,
            "modulo" => &mut self.modulo,
            "varargexpansion" => &mut self.vararg,
            "hexliterals" => &mut self.hex,
            "longbrackets" => &mut self.long_brackets,
            _ => return None,
        })
    }
}

/// What a rewrite did: the new source, and whether the vararg and operator passes fired.
#[derive(Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub source: Vec<u8>,
    pub vararg: bool,
    pub ops: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Name,
    Number,
    Str,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}

fn name_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn name_cont(c: u8) -> bool {
    name_start(c) || c.is_ascii_digit()
}

/// `[`, N `=`, `[` at `pos`: N, else none.
fn long_level(src: &[u8], pos: usize) -> Option<usize> {
    if src.get(pos) != Some(&b'[') {
        return None;
    }
    let mut j = pos + 1;
    while src.get(j) == Some(&b'=') {
        j += 1;
    }
    (src.get(j) == Some(&b'[')).then_some(j - pos - 1)
}

/// Past a long bracket opening at `pos`; level 0 nests as the 5.0 reader does (`0x700010`).
fn skip_long(src: &[u8], pos: usize, level: usize) -> usize {
    let len = src.len();
    let mut i = pos + 2 + level;
    let mut depth = 0;
    while i < len {
        if level == 0 && src[i] == b'[' && src.get(i + 1) == Some(&b'[') {
            depth += 1;
            i += 2;
            continue;
        }
        if src[i] == b']' {
            let mut j = i + 1;
            let mut eq = 0;
            while src.get(j) == Some(&b'=') {
                j += 1;
                eq += 1;
            }
            if eq == level && src.get(j) == Some(&b']') {
                if depth == 0 {
                    return j + 1;
                }
                depth -= 1;
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    len
}

fn skip_short(src: &[u8], pos: usize) -> usize {
    let q = src[pos];
    let mut i = pos + 1;
    while i < src.len() {
        let c = src[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if c == q {
            return i + 1;
        }
        if c == b'\n' {
            return i;
        }
        i += 1;
    }
    src.len()
}

fn skip_number(src: &[u8], pos: usize) -> usize {
    let len = src.len();
    let hex = src.get(pos) == Some(&b'0') && matches!(src.get(pos + 1), Some(b'x' | b'X'));
    let mut seen_dot = false;
    let mut i = pos;
    while i < len {
        let c = src[i];
        if c == b'.' {
            if seen_dot || src.get(i + 1) == Some(&b'.') {
                break;
            }
            seen_dot = true;
            i += 1;
            continue;
        }
        if name_cont(c) {
            i += 1;
            continue;
        }
        if (c == b'+' || c == b'-') && i > pos {
            let p = src[i - 1];
            let marker = if hex {
                matches!(p, b'p' | b'P')
            } else {
                matches!(p, b'e' | b'E')
            };
            if marker {
                i += 1;
                continue;
            }
        }
        break;
    }
    i
}

trait Sink {
    fn leveled(&mut self);
    fn token(&mut self, kind: Kind, start: usize, end: usize);
}

/// The one lexer body, so the trigger scan and the tokenizer cannot disagree on string ends.
fn lex(src: &[u8], sink: &mut impl Sink) {
    let len = src.len();
    let mut i = 0;
    while i < len {
        let c = src[i];
        if matches!(c, b' ' | b'\t' | b'\r' | b'\n' | 0x0c | 0x0b) {
            i += 1;
            continue;
        }
        if c == b'-' && src.get(i + 1) == Some(&b'-') {
            let j = i + 2;
            if let Some(lvl) = long_level(src, j) {
                if lvl > 0 {
                    sink.leveled();
                }
                i = skip_long(src, j, lvl);
                continue;
            }
            while i < len && src[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'[' {
            if let Some(lvl) = long_level(src, i) {
                if lvl > 0 {
                    sink.leveled();
                }
                let s = i;
                i = skip_long(src, i, lvl);
                sink.token(Kind::Str, s, i);
                continue;
            }
        }
        if c == b'"' || c == b'\'' {
            let s = i;
            i = skip_short(src, i);
            sink.token(Kind::Str, s, i);
            continue;
        }
        if name_start(c) {
            let s = i;
            i += 1;
            while i < len && name_cont(src[i]) {
                i += 1;
            }
            sink.token(Kind::Name, s, i);
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && src.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let s = i;
            i = skip_number(src, i);
            sink.token(Kind::Number, s, i);
            continue;
        }
        if c == b'.' {
            let mut d = 1;
            while d < 3 && src.get(i + d) == Some(&b'.') {
                d += 1;
            }
            sink.token(Kind::Punct, i, i + d);
            i += d;
            continue;
        }
        sink.token(Kind::Punct, i, i + 1);
        i += 1;
    }
}

struct Tokens(Vec<Token>);

impl Sink for Tokens {
    fn leveled(&mut self) {}
    fn token(&mut self, kind: Kind, start: usize, end: usize) {
        self.0.push(Token { kind, start, end });
    }
}

/// The trigger scan: which passes have work, counting only tokens.
#[derive(Default)]
struct Flags<'a> {
    src: &'a [u8],
    hash: bool,
    modulo: bool,
    vararg: bool,
    hex: bool,
    leveled: bool,
}

impl Sink for Flags<'_> {
    fn leveled(&mut self) {
        self.leveled = true;
    }
    fn token(&mut self, kind: Kind, start: usize, end: usize) {
        let n = end - start;
        match kind {
            Kind::Punct if n == 3 => self.vararg = true,
            Kind::Punct if n == 1 => match self.src[start] {
                b'#' => self.hash = true,
                b'%' => self.modulo = true,
                _ => {}
            },
            Kind::Number
                if n >= 3
                    && self.src[start] == b'0'
                    && matches!(self.src[start + 1], b'x' | b'X') =>
            {
                self.hex = true;
            }
            _ => {}
        }
    }
}

fn tokenize(src: &[u8]) -> Vec<Token> {
    let mut t = Tokens(Vec::new());
    lex(src, &mut t);
    t.0
}

// ---- The # / % precedence parser ----

const MAX_DEPTH: i32 = 200;

struct Ctx<'a> {
    src: &'a [u8],
    toks: &'a [Token],
    opens: Vec<(usize, &'static str, usize)>,
    closes: Vec<(usize, usize)>,
    char_ops: Vec<(usize, Option<u8>)>,
    len_on: bool,
    mod_on: bool,
    depth: i32,
}

const KEYWORDS: [&str; 21] = [
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

impl Ctx<'_> {
    fn n(&self) -> usize {
        self.toks.len()
    }
    fn text(&self, k: usize) -> &[u8] {
        let t = self.toks[k];
        &self.src[t.start..t.end]
    }
    fn punct(&self, k: usize, ch: u8) -> bool {
        k < self.n() && self.toks[k].kind == Kind::Punct && self.src[self.toks[k].start] == ch
    }
    fn name_is(&self, k: usize, w: &str) -> bool {
        k < self.n() && self.toks[k].kind == Kind::Name && self.text(k) == w.as_bytes()
    }
    fn keyword(&self, k: usize) -> bool {
        KEYWORDS.iter().any(|w| self.text(k) == w.as_bytes())
    }
    fn name_non_kw(&self, k: usize) -> bool {
        k < self.n() && self.toks[k].kind == Kind::Name && !self.keyword(k)
    }

    fn match_bracket(&self, k: usize, open: u8, close: u8) -> Option<usize> {
        let mut depth = 0;
        for j in k..self.n() {
            if self.toks[j].kind != Kind::Punct {
                continue;
            }
            let ch = self.src[self.toks[j].start];
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Some(j + 1);
                }
            }
        }
        None
    }

    fn scan_bracket(&mut self, k: usize, open: u8, close: u8) -> Option<usize> {
        let e = self.match_bracket(k, open, close)?;
        self.scan_range(k + 1, e - 1);
        Some(e)
    }

    fn primary(&mut self, k: usize) -> Option<usize> {
        if k >= self.n() {
            return None;
        }
        match self.toks[k].kind {
            Kind::Punct => match self.src[self.toks[k].start] {
                b'(' => self.scan_bracket(k, b'(', b')'),
                b'{' => self.scan_bracket(k, b'{', b'}'),
                _ => None,
            },
            Kind::Str | Kind::Number => Some(k + 1),
            Kind::Name => {
                if self.keyword(k) {
                    matches!(self.text(k), b"nil" | b"true" | b"false").then_some(k + 1)
                } else {
                    Some(k + 1)
                }
            }
        }
    }

    fn suffixes(&mut self, mut k: usize) -> usize {
        while k < self.n() {
            let t = self.toks[k];
            if t.kind == Kind::Str {
                k += 1;
                continue;
            }
            if t.kind != Kind::Punct {
                break;
            }
            let ch = self.src[t.start];
            let next = match ch {
                b'.' if t.end - t.start == 1 => {
                    if self.name_non_kw(k + 1) {
                        Some(k + 2)
                    } else {
                        None
                    }
                }
                b'[' => self.scan_bracket(k, b'[', b']'),
                b'(' => self.scan_bracket(k, b'(', b')'),
                b'{' => self.scan_bracket(k, b'{', b'}'),
                b':' => {
                    if !self.name_non_kw(k + 1) {
                        None
                    } else {
                        let m = k + 2;
                        if self.punct(m, b'(') {
                            self.scan_bracket(m, b'(', b')')
                        } else if m < self.n() && self.toks[m].kind == Kind::Str {
                            Some(m + 1)
                        } else if self.punct(m, b'{') {
                            self.scan_bracket(m, b'{', b'}')
                        } else {
                            None
                        }
                    }
                }
                _ => None,
            };
            match next {
                Some(e) => k = e,
                None => break,
            }
        }
        k
    }

    /// Binary precedence and right-associativity; 0 for anything not climbed.
    fn bin_prec(&self, k: usize) -> (i32, bool) {
        let t = self.toks[k];
        match t.kind {
            Kind::Name => match self.text(k) {
                b"or" => (1, false),
                b"and" => (2, false),
                _ => (0, false),
            },
            Kind::Punct => match self.src[t.start] {
                b'<' | b'>' => (3, false),
                b'+' | b'-' => (5, false),
                b'*' | b'/' | b'%' => (6, false),
                b'^' => (8, true),
                _ => (0, false),
            },
            _ => (0, false),
        }
    }

    fn expr(&mut self, k: usize, min_prec: i32) -> usize {
        let start = k;
        self.depth += 1;
        let out = self.expr_inner(start, min_prec);
        self.depth -= 1;
        out
    }

    fn expr_inner(&mut self, k: usize, min_prec: i32) -> usize {
        let start = k;
        if self.depth > MAX_DEPTH {
            return start;
        }
        let mut cur;
        if k < self.n() && (self.punct(k, b'-') || self.punct(k, b'#') || self.name_is(k, "not")) {
            let is_hash = self.punct(k, b'#');
            let operand_end = self.expr(k + 1, 7);
            if operand_end == k + 1 {
                return start;
            }
            if is_hash && self.len_on {
                let open = self.toks[k].start;
                let close = self.toks[operand_end - 1].end;
                self.opens.push((open, "__len(", close - open));
                self.closes.push((close, close - open));
                self.char_ops.push((open, None));
            }
            cur = operand_end;
        } else {
            let Some(p) = self.primary(k) else {
                return start;
            };
            cur = self.suffixes(p);
        }
        while cur < self.n() {
            let (prec, ra) = self.bin_prec(cur);
            if prec == 0 || prec < min_prec {
                break;
            }
            let op = cur;
            let rstart = cur + 1;
            let rend = self.expr(rstart, prec + if ra { 0 } else { 1 });
            if rend == rstart {
                break;
            }
            if self.src[self.toks[op].start] == b'%'
                && self.toks[op].kind == Kind::Punct
                && self.mod_on
            {
                let open = self.toks[start].start;
                let close = self.toks[rend - 1].end;
                self.opens.push((open, "__mod(", close - open));
                self.closes.push((close, close - open));
                self.char_ops.push((self.toks[op].start, Some(b',')));
            }
            cur = rend;
        }
        cur
    }

    fn scan_range(&mut self, lo: usize, hi: usize) {
        let mut k = lo;
        while k < hi {
            let e = self.expr(k, 0);
            k = if e > k { e } else { k + 1 };
        }
    }
}

fn rewrite_ops(src: &[u8], toks: &[Token], len_on: bool, mod_on: bool) -> Option<Vec<u8>> {
    let mut c = Ctx {
        src,
        toks,
        opens: Vec::new(),
        closes: Vec::new(),
        char_ops: Vec::new(),
        len_on,
        mod_on,
        depth: 0,
    };
    c.scan_range(0, toks.len());
    if c.opens.is_empty() {
        return None;
    }
    let mut opens = c.opens;
    let mut closes = c.closes;
    let mut ops = c.char_ops;
    opens.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)));
    closes.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    ops.sort_by_key(|o| o.0);
    let mut out = Vec::with_capacity(src.len() + opens.len() * 7);
    let (mut oi, mut ci, mut hi) = (0, 0, 0);
    for (p, &byte) in src.iter().enumerate() {
        while ci < closes.len() && closes[ci].0 == p {
            out.push(b')');
            ci += 1;
        }
        while oi < opens.len() && opens[oi].0 == p {
            out.extend_from_slice(opens[oi].1.as_bytes());
            oi += 1;
        }
        if hi < ops.len() && ops[hi].0 == p {
            if let Some(r) = ops[hi].1 {
                out.push(r);
            }
            hi += 1;
        } else {
            out.push(byte);
        }
    }
    while ci < closes.len() && closes[ci].0 == src.len() {
        out.push(b')');
        ci += 1;
    }
    Some(out)
}

// ---- The vararg pass ----

fn rewrite_vararg(src: &[u8], toks: &[Token]) -> Option<Vec<u8>> {
    let n = toks.len();
    let is_vararg =
        |i: usize| i < n && toks[i].kind == Kind::Punct && toks[i].end - toks[i].start == 3;
    let is_punct = |i: usize, ch: u8| {
        i < n
            && toks[i].kind == Kind::Punct
            && toks[i].end - toks[i].start == 1
            && src[toks[i].start] == ch
    };
    let mut skip = vec![false; n];
    let mut i = 0;
    while i < n {
        if toks[i].kind != Kind::Name || &src[toks[i].start..toks[i].end] != b"function" {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < n
            && !is_punct(j, b'(')
            && (toks[j].kind == Kind::Name || is_punct(j, b'.') || is_punct(j, b':'))
        {
            j += 1;
        }
        if !is_punct(j, b'(') {
            i += 1;
            continue;
        }
        // The matching `)`.
        let mut depth = 0;
        let mut e = n;
        for (p, t) in toks.iter().enumerate().skip(j) {
            if t.kind != Kind::Punct || t.end - t.start != 1 {
                continue;
            }
            match src[t.start] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        e = p + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(p) = (j..e).find(|p| is_vararg(*p)) {
            skip[p] = true;
        }
        i = e.max(i + 1);
    }
    let mut out = Vec::with_capacity(src.len() + 8);
    let mut prev = 0;
    let mut any = false;
    for (i, t) in toks.iter().enumerate() {
        if is_vararg(i) && !skip[i] {
            out.extend_from_slice(&src[prev..t.start]);
            out.extend_from_slice(b"unpack(arg)");
            prev = t.end;
            any = true;
        }
    }
    if !any {
        return None;
    }
    out.extend_from_slice(&src[prev..]);
    Some(out)
}

// ---- The hex pass ----

fn rewrite_hex(src: &[u8], toks: &[Token]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(src.len());
    let mut p = 0;
    let mut any = false;
    for t in toks {
        if t.kind != Kind::Number
            || t.end - t.start < 3
            || src[t.start] != b'0'
            || !matches!(src[t.start + 1], b'x' | b'X')
        {
            continue;
        }
        let digits = &src[t.start + 2..t.end];
        if digits.is_empty() || digits.len() > 16 || !digits.iter().all(u8::is_ascii_hexdigit) {
            continue;
        }
        let Ok(v) = u64::from_str_radix(std::str::from_utf8(digits).unwrap_or("x"), 16) else {
            continue;
        };
        out.extend_from_slice(&src[p..t.start]);
        out.extend_from_slice(v.to_string().as_bytes());
        p = t.end;
        any = true;
    }
    if !any {
        return None;
    }
    out.extend_from_slice(&src[p..]);
    Some(out)
}

// ---- The leveled long-bracket pass ----

fn long_closed(src: &[u8], start: usize, end: usize, level: usize) -> bool {
    let delim = 2 + level;
    if end < start + 2 * delim {
        return false;
    }
    let p = end - delim;
    src[p] == b']' && src[end - 1] == b']' && (0..level).all(|k| src[p + 1 + k] == b'=')
}

fn emit_leveled(src: &[u8], start: usize, end: usize, level: usize, out: &mut Vec<u8>) {
    let delim = 2 + level;
    let body = &src[start + delim..end - delim];
    let double = body.windows(2).any(|w| w == b"[[" || w == b"]]");
    if !double && body.last() != Some(&b']') {
        out.extend_from_slice(b"[[");
        out.extend_from_slice(body);
        out.extend_from_slice(b"]]");
        return;
    }
    out.push(b'"');
    for &b in body {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0 => out.extend_from_slice(b"\\0"),
            _ => out.push(b),
        }
    }
    out.push(b'"');
}

fn rewrite_long_brackets(src: &[u8]) -> Option<Vec<u8>> {
    let len = src.len();
    let mut out = Vec::with_capacity(len + 16);
    let mut i = 0;
    let mut any = false;
    while i < len {
        let c = src[i];
        if c == b'"' || c == b'\'' {
            let e = skip_short(src, i);
            out.extend_from_slice(&src[i..e]);
            i = e;
            continue;
        }
        if c == b'-' && src.get(i + 1) == Some(&b'-') {
            let j = i + 2;
            match long_level(src, j) {
                Some(lvl) if lvl > 0 => {
                    let e = skip_long(src, j, lvl);
                    if long_closed(src, j, e, lvl) {
                        out.extend(src[i..e].iter().map(|&b| {
                            if b == b'\n' || b == b'\r' {
                                b
                            } else {
                                b' '
                            }
                        }));
                        any = true;
                    } else {
                        out.extend_from_slice(&src[i..e]);
                    }
                    i = e;
                }
                Some(_) => {
                    let e = skip_long(src, j, 0);
                    out.extend_from_slice(&src[i..e]);
                    i = e;
                }
                None => {
                    let mut e = j;
                    while e < len && src[e] != b'\n' {
                        e += 1;
                    }
                    out.extend_from_slice(&src[i..e]);
                    i = e;
                }
            }
            continue;
        }
        if c == b'[' {
            match long_level(src, i) {
                Some(lvl) if lvl > 0 => {
                    let e = skip_long(src, i, lvl);
                    if long_closed(src, i, e, lvl) {
                        emit_leveled(src, i, e, lvl, &mut out);
                        any = true;
                    } else {
                        out.extend_from_slice(&src[i..e]);
                    }
                    i = e;
                    continue;
                }
                Some(_) => {
                    let e = skip_long(src, i, 0);
                    out.extend_from_slice(&src[i..e]);
                    i = e;
                    continue;
                }
                None => {}
            }
        }
        out.push(c);
        i += 1;
    }
    any.then_some(out)
}

/// `RunPasses`: every rewrite the chunk needs, or `None` when it needs none.
pub fn run(src: &[u8], o: Options) -> Option<Rewrite> {
    if src.is_empty() || src[0] == 0x1b {
        return None;
    }
    let mut flags = Flags {
        src,
        ..Flags::default()
    };
    lex(src, &mut flags);
    let want_long = o.long_brackets && flags.leveled;
    let want_hex = o.hex && flags.hex;
    let want_vararg = o.vararg && flags.vararg;
    let want_len = o.len && flags.hash;
    let want_mod = o.modulo && flags.modulo;
    let want_tokens = want_hex || want_vararg || want_len || want_mod;
    if !want_long && !want_tokens {
        return None;
    }
    let mut cur: Option<Vec<u8>> = None;
    if want_long {
        cur = rewrite_long_brackets(src);
    }
    let mut out = Rewrite {
        source: Vec::new(),
        vararg: false,
        ops: false,
    };
    if want_tokens {
        let text = |c: &Option<Vec<u8>>| c.clone().unwrap_or_else(|| src.to_vec());
        let mut buf = text(&cur);
        let mut toks = tokenize(&buf);
        if want_hex {
            if let Some(b) = rewrite_hex(&buf, &toks) {
                buf = b;
                toks = tokenize(&buf);
                cur = Some(buf.clone());
            }
        }
        if want_vararg {
            if let Some(b) = rewrite_vararg(&buf, &toks) {
                buf = b;
                toks = tokenize(&buf);
                cur = Some(buf.clone());
                out.vararg = true;
            }
        }
        if want_len || want_mod {
            if let Some(b) = rewrite_ops(&buf, &toks, want_len, want_mod) {
                cur = Some(b);
                out.ops = true;
            }
        }
    }
    out.source = cur?;
    Some(out)
}

/// `AddonNameFromChunk`: the folder of an `…\AddOns\<Name>\…` chunk name, when it is safe to
/// embed in a string literal.
pub fn addon_from_chunk(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find("addons") {
        let at = from + p;
        let after = at + 6;
        if matches!(name.as_bytes().get(after), Some(b'\\' | b'/')) {
            let rest = &name[after + 1..];
            let folder: &str = rest.split(['\\', '/']).next().unwrap_or("");
            if folder.is_empty() || folder.len() > 127 || folder.contains(['"', '\\', '\n', '\r']) {
                return None;
            }
            return Some(folder.to_string());
        }
        from = after;
    }
    None
}

/// `LoadBuffer_h`'s whole rewrite: the passes, then the newline-free preamble. A file chunk of
/// an addon that uses `...` gets `(addonName, addonTable)` as its `arg`; any other vararg chunk
/// is wrapped `return function(...) … end`, which the funnel calls once (`wrapped`). The
/// `Option<String>` is the addon whose namespace grant the preamble consumes.
pub fn load_rewrite(
    src: &[u8],
    name: &str,
    file: bool,
    o: Options,
) -> Option<(Vec<u8>, bool, Option<String>)> {
    let r = run(src, o)?;
    let mut pre = String::new();
    if r.ops {
        pre += "local __len,__mod=__len,__mod;";
    }
    if r.vararg {
        pre += "local unpack=unpack;";
    }
    let mut wrapped = false;
    let mut grant = None;
    if r.vararg {
        match addon_from_chunk(name).filter(|_| file) {
            Some(addon) => {
                pre += &format!("local arg={{\"{addon}\",__addonns(\"{addon}\"),n=2}};");
                grant = Some(addon);
            }
            None => {
                pre += "return function(...) ";
                wrapped = true;
            }
        }
    }
    if pre.is_empty() {
        return Some((r.source, false, None));
    }
    let body = r.source;
    let bom = if body.starts_with(&[0xef, 0xbb, 0xbf]) {
        3
    } else {
        0
    };
    let mut full = Vec::with_capacity(body.len() + pre.len() + 8);
    full.extend_from_slice(&body[..bom]);
    full.extend_from_slice(pre.as_bytes());
    full.extend_from_slice(&body[bom..]);
    if wrapped {
        full.extend_from_slice(b"\nend");
    }
    Some((full, wrapped, grant))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(src: &str) -> String {
        run(src.as_bytes(), Options::default())
            .map(|r| String::from_utf8(r.source).unwrap())
            .unwrap_or_else(|| src.to_string())
    }

    #[test]
    fn operators_rewrite_by_precedence() {
        assert_eq!(t("x = #t"), "x = __len(t)");
        assert_eq!(t("x = a * b % c"), "x = __mod(a * b , c)");
        assert_eq!(t("x = a + b % c"), "x = a + __mod(b , c)");
        assert_eq!(t("x = a % b % c"), "x = __mod(__mod(a , b) , c)");
        assert_eq!(t("x = #a % b"), "x = __mod(__len(a) , b)");
        assert_eq!(t("x = #self.list[1]"), "x = __len(self.list[1])");
    }

    #[test]
    fn strings_comments_and_params_are_left_alone() {
        assert_eq!(t("s = \"%d #\" -- a % b"), "s = \"%d #\" -- a % b");
        assert_eq!(
            t("f = function(...) return ... end"),
            "f = function(...) return unpack(arg) end"
        );
        assert_eq!(t("x = 0xFF"), "x = 255");
        assert_eq!(t("s = [=[a]]b]=]"), "s = \"a]]b\"");
        assert_eq!(t("--[=[ c\n]=] x=1"), "       \n    x=1");
    }

    #[test]
    fn file_scope_varargs_name_the_addon() {
        let (src, wrapped, grant) = load_rewrite(
            b"local name, ns = ...",
            "@Interface\\AddOns\\Foo\\a.lua",
            true,
            Options::default(),
        )
        .unwrap();
        assert!(!wrapped);
        assert_eq!(grant.as_deref(), Some("Foo"));
        assert_eq!(
            String::from_utf8(src).unwrap(),
            "local unpack=unpack;local arg={\"Foo\",__addonns(\"Foo\"),n=2};local name, ns = unpack(arg)"
        );
        let (src, wrapped, _) =
            load_rewrite(b"return ...", "=x", false, Options::default()).unwrap();
        assert!(wrapped);
        assert_eq!(
            String::from_utf8(src).unwrap(),
            "local unpack=unpack;return function(...) return unpack(arg)\nend"
        );
    }
}
