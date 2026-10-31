//! The expression language shared by access rules (`@request.auth.id == record.owner_id`) and query filters (`status == "paid" && amount >= 1000`).
//!
//! It is parsed into a small tree and evaluated against a decoded record; user text is never spliced into anything that executes. Both spellings of the
//! operators work (`&&`/`and`, `||`/`or`, `==`/`=`, `!`/`not`) so the REST form `(status='paid' and amount>1000)` and the SDK form are the same language.
//!
//! Limits (length, nesting, node count) keep a hostile expression from costing more than a small, bounded amount of work to parse and evaluate.

use crate::error::{Error, Result};
use crate::model::Record;
use serde_json::Value;

pub const MAX_LEN: usize = 2048;
const MAX_NEST: usize = 32;
const MAX_NODES: usize = 512;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    /// A dotted path. `at` is true for `@request.auth.id` style paths, false for record fields.
    Path {
        at: bool,
        parts: Vec<String>,
    },
    List(Vec<Value>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cmp(Op, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    NotContains,
    In,
}

// ---- lexer ----

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Num(f64),
    Ident(String),
    At(Vec<String>),
    Op(Op),
    And,
    Or,
    Not,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
}

fn err<T>(pos: usize, msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(format!(
        "expression, position {pos}: {}",
        msg.into()
    )))
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>> {
    if src.len() > MAX_LEN {
        return Err(Error::Invalid(format!(
            "expression is longer than {MAX_LEN} bytes"
        )));
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<(Tok, usize)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push((Tok::LParen, start));
                i += 1
            }
            ')' => {
                out.push((Tok::RParen, start));
                i += 1
            }
            '[' => {
                out.push((Tok::LBracket, start));
                i += 1
            }
            ']' => {
                out.push((Tok::RBracket, start));
                i += 1
            }
            ',' => {
                out.push((Tok::Comma, start));
                i += 1
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                out.push((Tok::And, start));
                i += 2
            }
            '|' if chars.get(i + 1) == Some(&'|') => {
                out.push((Tok::Or, start));
                i += 2
            }
            '=' => {
                i += if chars.get(i + 1) == Some(&'=') { 2 } else { 1 };
                out.push((Tok::Op(Op::Eq), start));
            }
            '!' => match chars.get(i + 1) {
                Some('=') => {
                    out.push((Tok::Op(Op::Ne), start));
                    i += 2
                }
                Some('~') => {
                    out.push((Tok::Op(Op::NotContains), start));
                    i += 2
                }
                _ => {
                    out.push((Tok::Not, start));
                    i += 1
                }
            },
            '<' => {
                if chars.get(i + 1) == Some(&'=') {
                    out.push((Tok::Op(Op::Le), start));
                    i += 2
                } else {
                    out.push((Tok::Op(Op::Lt), start));
                    i += 1
                }
            }
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    out.push((Tok::Op(Op::Ge), start));
                    i += 2
                } else {
                    out.push((Tok::Op(Op::Gt), start));
                    i += 1
                }
            }
            '~' => {
                out.push((Tok::Op(Op::Contains), start));
                i += 1
            }
            '"' | '\'' => {
                let q = c;
                i += 1;
                let mut s = String::new();
                loop {
                    match chars.get(i) {
                        None => return err(start, "unterminated string"),
                        Some(&ch) if ch == q => {
                            i += 1;
                            break;
                        }
                        Some('\\') => {
                            match chars.get(i + 1) {
                                Some('n') => s.push('\n'),
                                Some('t') => s.push('\t'),
                                Some(&e @ ('\\' | '"' | '\'')) => s.push(e),
                                _ => {
                                    return err(
                                        i,
                                        "unknown escape (use \\\\, \\\", \\', \\n or \\t)",
                                    );
                                }
                            }
                            i += 2;
                        }
                        Some(&ch) => {
                            s.push(ch);
                            i += 1
                        }
                    }
                }
                out.push((Tok::Str(s), start));
            }
            '@' => {
                i += 1;
                let mut parts = Vec::new();
                loop {
                    let s = i;
                    if !chars.get(i).is_some_and(|c| is_ident_start(*c)) {
                        return err(i, "expected a name after '@' or '.'");
                    }
                    while chars.get(i).is_some_and(|c| is_ident(*c)) {
                        i += 1
                    }
                    parts.push(chars[s..i].iter().collect());
                    if chars.get(i) == Some(&'.') {
                        i += 1
                    } else {
                        break;
                    }
                }
                out.push((Tok::At(parts), start));
            }
            c if c.is_ascii_digit()
                || (c == '-'
                    && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
                    && !matches!(
                        out.last(),
                        Some((
                            Tok::Ident(_) | Tok::Str(_) | Tok::Num(_) | Tok::RParen | Tok::At(_),
                            _
                        ))
                    )) =>
            {
                let s = i;
                if c == '-' {
                    i += 1
                }
                while chars.get(i).is_some_and(|d| d.is_ascii_digit()) {
                    i += 1
                }
                if chars.get(i) == Some(&'.')
                    && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
                {
                    i += 1;
                    while chars.get(i).is_some_and(|d| d.is_ascii_digit()) {
                        i += 1
                    }
                }
                if matches!(chars.get(i), Some('e' | 'E')) {
                    let mut j = i + 1;
                    if matches!(chars.get(j), Some('+' | '-')) {
                        j += 1
                    }
                    if chars.get(j).is_some_and(|d| d.is_ascii_digit()) {
                        while chars.get(j).is_some_and(|d| d.is_ascii_digit()) {
                            j += 1
                        }
                        i = j;
                    }
                }
                let t: String = chars[s..i].iter().collect();
                match t.parse::<f64>() {
                    Ok(n) if n.is_finite() => out.push((Tok::Num(n), start)),
                    _ => return err(start, format!("bad number {t}")),
                }
            }
            c if is_ident_start(c) => {
                let s = i;
                while chars.get(i).is_some_and(|c| is_ident(*c)) {
                    i += 1
                }
                let mut parts: Vec<String> = vec![chars[s..i].iter().collect()];
                while chars.get(i) == Some(&'.')
                    && chars.get(i + 1).is_some_and(|c| is_ident_start(*c))
                {
                    i += 1;
                    let s2 = i;
                    while chars.get(i).is_some_and(|c| is_ident(*c)) {
                        i += 1
                    }
                    parts.push(chars[s2..i].iter().collect());
                }
                let word = parts[0].to_ascii_lowercase();
                if parts.len() == 1 {
                    match word.as_str() {
                        "and" => out.push((Tok::And, start)),
                        "or" => out.push((Tok::Or, start)),
                        "not" => out.push((Tok::Not, start)),
                        "in" => out.push((Tok::Op(Op::In), start)),
                        _ => out.push((Tok::Ident(parts.remove(0)), start)),
                    }
                } else {
                    out.push((Tok::Ident(parts.join(".")), start));
                }
            }
            other => return err(i, format!("unexpected character {other:?}")),
        }
    }
    Ok(out)
}

// ---- parser (recursive descent, depth- and size-limited) ----

struct P {
    toks: Vec<(Tok, usize)>,
    i: usize,
    nodes: usize,
    /// How many parentheses and negations are open right now. Bounded, so the recursion below cannot be driven deep.
    nest: usize,
    end: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i).map(|(t, _)| t)
    }
    fn pos(&self) -> usize {
        self.toks.get(self.i).map(|(_, p)| *p).unwrap_or(self.end)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.i).map(|(t, _)| t.clone());
        self.i += 1;
        t
    }
    fn node(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            err(self.pos(), "expression is too large")
        } else {
            Ok(())
        }
    }

    fn enter(&mut self) -> Result<()> {
        self.nest += 1;
        if self.nest > MAX_NEST {
            err(self.pos(), "expression is nested too deeply")
        } else {
            Ok(())
        }
    }

    fn or(&mut self, d: usize) -> Result<Expr> {
        let mut l = self.and(d + 1)?;
        while self.peek() == Some(&Tok::Or) {
            self.bump();
            self.node()?;
            l = Expr::Or(Box::new(l), Box::new(self.and(d + 1)?));
        }
        Ok(l)
    }
    fn and(&mut self, d: usize) -> Result<Expr> {
        let mut l = self.not(d + 1)?;
        while self.peek() == Some(&Tok::And) {
            self.bump();
            self.node()?;
            l = Expr::And(Box::new(l), Box::new(self.not(d + 1)?));
        }
        Ok(l)
    }
    fn not(&mut self, d: usize) -> Result<Expr> {
        if self.peek() == Some(&Tok::Not) {
            self.bump();
            self.node()?;
            self.enter()?;
            let inner = self.not(d + 1)?;
            self.nest -= 1;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.cmp(d + 1)
    }
    fn cmp(&mut self, d: usize) -> Result<Expr> {
        let l = self.term(d + 1)?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            self.bump();
            self.node()?;
            let r = self.term(d + 1)?;
            return Ok(Expr::Cmp(op, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }
    fn lit_of(t: &Tok) -> Option<Value> {
        match t {
            Tok::Str(s) => Some(Value::String(s.clone())),
            Tok::Num(n) => Some(
                serde_json::Number::from_f64(*n)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            ),
            Tok::Ident(w) if w.eq_ignore_ascii_case("true") => Some(Value::Bool(true)),
            Tok::Ident(w) if w.eq_ignore_ascii_case("false") => Some(Value::Bool(false)),
            Tok::Ident(w) if w.eq_ignore_ascii_case("null") => Some(Value::Null),
            _ => None,
        }
    }
    fn term(&mut self, d: usize) -> Result<Expr> {
        let pos = self.pos();
        self.node()?;
        match self.bump() {
            Some(Tok::LParen) => {
                self.enter()?;
                let e = self.or(d + 1)?;
                if self.bump() != Some(Tok::RParen) {
                    return err(self.pos(), "expected ')'");
                }
                self.nest -= 1;
                Ok(e)
            }
            Some(Tok::LBracket) => {
                let mut items = Vec::new();
                if self.peek() == Some(&Tok::RBracket) {
                    self.bump();
                    return Ok(Expr::List(items));
                }
                loop {
                    let t = self
                        .bump()
                        .ok_or_else(|| Error::Invalid("expression: unterminated list".into()))?;
                    items.push(Self::lit_of(&t).ok_or_else(|| {
                        Error::Invalid(format!(
                            "expression, position {pos}: lists hold only literals"
                        ))
                    })?);
                    if items.len() > 128 {
                        return err(pos, "lists are limited to 128 items");
                    }
                    match self.bump() {
                        Some(Tok::Comma) => continue,
                        Some(Tok::RBracket) => break,
                        _ => return err(self.pos(), "expected ',' or ']'"),
                    }
                }
                Ok(Expr::List(items))
            }
            Some(Tok::At(parts)) => Ok(Expr::Path { at: true, parts }),
            Some(t @ Tok::Ident(_)) => {
                if let Some(v) = Self::lit_of(&t) {
                    return Ok(Expr::Lit(v));
                }
                let Tok::Ident(p) = t else { unreachable!() };
                Ok(Expr::Path {
                    at: false,
                    parts: p.split('.').map(str::to_string).collect(),
                })
            }
            Some(t @ (Tok::Str(_) | Tok::Num(_))) => {
                Ok(Expr::Lit(Self::lit_of(&t).unwrap_or(Value::Null)))
            }
            Some(other) => err(pos, format!("unexpected {other:?}")),
            None => err(pos, "the expression ends too soon"),
        }
    }
}

pub fn parse(src: &str) -> Result<Expr> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return Err(Error::Invalid("expression is empty".into()));
    }
    let mut p = P {
        toks,
        i: 0,
        nodes: 0,
        nest: 0,
        end: src.len(),
    };
    let e = p.or(0)?;
    if p.i < p.toks.len() {
        return err(p.pos(), "unexpected text after the expression");
    }
    Ok(e)
}

// ---- evaluation ----

/// What an expression can see.
#[derive(Default)]
pub struct Ctx<'a> {
    /// The record being filtered or protected. Bare names and `record.name` read its fields, plus `id`, `created_at`, `updated_at`, `collection`.
    pub record: Option<&'a Record>,
    /// `@request.auth.*` / `@auth.*`: the signed-in user, if any.
    pub auth: Option<&'a Value>,
    /// `@request.body.*`: the incoming data of a create or update.
    pub body: Option<&'a Value>,
    /// `@request.method`.
    pub method: &'a str,
}

fn walk<'v>(mut v: &'v Value, parts: &[String]) -> Option<&'v Value> {
    for p in parts {
        v = match v {
            Value::Object(m) => m.get(p)?,
            Value::Array(a) => a.get(p.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(v)
}

fn resolve(at: bool, parts: &[String], c: &Ctx<'_>) -> Value {
    if at {
        let (head, rest) = match parts.first().map(String::as_str) {
            Some("request") => (
                parts.get(1).map(String::as_str),
                parts.get(2..).unwrap_or(&[]),
            ),
            Some("auth") => (Some("auth"), parts.get(1..).unwrap_or(&[])),
            _ => return Value::Null,
        };
        return match head {
            Some("auth") => c
                .auth
                .and_then(|a| walk(a, rest))
                .cloned()
                .unwrap_or(Value::Null),
            Some("body") => c
                .body
                .and_then(|b| walk(b, rest))
                .cloned()
                .unwrap_or(Value::Null),
            Some("method") => Value::String(c.method.to_string()),
            _ => Value::Null,
        };
    }
    let Some(r) = c.record else {
        return Value::Null;
    };
    let parts = if parts.first().map(String::as_str) == Some("record") {
        &parts[1..]
    } else {
        parts
    };
    let Some(head) = parts.first() else {
        return Value::Null;
    };
    let first = match head.as_str() {
        "id" => Value::String(r.id.clone()),
        "collection" => Value::String(r.collection.clone()),
        "created_at" => Value::from(r.created_at),
        "updated_at" => Value::from(r.updated_at),
        name => match r.fields.get(name) {
            Some(v) => v.clone(),
            None => match r.files.get(name) {
                Some(f) => serde_json::to_value(f).unwrap_or(Value::Null),
                None => return Value::Null,
            },
        },
    };
    walk(&first, &parts[1..]).cloned().unwrap_or(Value::Null)
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Equality as rules need it: numbers compare by value, and a missing value (null) equals the empty string, so `field == ""` and `field == null` both mean
/// "not set" and `@request.auth.id != ""` means "signed in".
fn eq(a: &Value, b: &Value) -> bool {
    let empty = |v: &Value| matches!(v, Value::Null) || v.as_str() == Some("");
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ if empty(a) && empty(b) => true,
        _ => a == b,
    }
}

fn is_empty_value(v: &Value) -> bool {
    matches!(v, Value::Null) || v.as_str() == Some("")
}

fn is_auth_path(e: &Expr) -> bool {
    matches!(e, Expr::Path { at: true, parts } if matches!(parts.first().map(String::as_str), Some("auth")) || (parts.first().map(String::as_str) == Some("request") && parts.get(1).map(String::as_str) == Some("auth")))
}

fn is_path(e: &Expr) -> bool {
    matches!(e, Expr::Path { .. })
}

fn order(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64()?.partial_cmp(&y.as_f64()?),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

impl Expr {
    /// Whether the expression reads the record. A rule that does not (`@request.auth.id != ""`) has one answer for the whole request and can be decided before looking at any record.
    pub fn uses_record(&self) -> bool {
        match self {
            Expr::Path { at, .. } => !*at,
            Expr::Lit(_) | Expr::List(_) => false,
            Expr::Not(a) => a.uses_record(),
            Expr::And(a, b) | Expr::Or(a, b) | Expr::Cmp(_, a, b) => {
                a.uses_record() || b.uses_record()
            }
        }
    }

    pub fn eval(&self, c: &Ctx<'_>) -> Value {
        match self {
            Expr::Lit(v) => v.clone(),
            Expr::Path { at, parts } => resolve(*at, parts, c),
            Expr::List(items) => Value::Array(items.clone()),
            Expr::Not(e) => Value::Bool(!truthy(&e.eval(c))),
            Expr::And(a, b) => Value::Bool(truthy(&a.eval(c)) && truthy(&b.eval(c))),
            Expr::Or(a, b) => Value::Bool(truthy(&a.eval(c)) || truthy(&b.eval(c))),
            Expr::Cmp(op, a, b) => {
                let (l, r) = (a.eval(c), b.eval(c));
                // An anonymous caller has no identity. Comparing that absence with another field must never count as a match, or a rule like
                // `@request.auth.id == record.owner_id` would let anyone in on every record that has no owner. Comparing with a literal ("" or null) is
                // still how a rule asks "is anyone signed in?".
                let anon_vs_field = (is_auth_path(a) && is_empty_value(&l) && is_path(b))
                    || (is_auth_path(b) && is_empty_value(&r) && is_path(a));
                Value::Bool(match op {
                    Op::Eq if anon_vs_field => false,
                    Op::Ne if anon_vs_field => true,
                    Op::Eq => eq(&l, &r),
                    Op::Ne => !eq(&l, &r),
                    Op::Lt => order(&l, &r).is_some_and(|o| o.is_lt()),
                    Op::Le => order(&l, &r).is_some_and(|o| o.is_le()),
                    Op::Gt => order(&l, &r).is_some_and(|o| o.is_gt()),
                    Op::Ge => order(&l, &r).is_some_and(|o| o.is_ge()),
                    Op::Contains | Op::NotContains => {
                        let has = match (&l, &r) {
                            (Value::String(x), Value::String(y)) => {
                                x.to_lowercase().contains(&y.to_lowercase())
                            }
                            (Value::Array(x), y) => x.iter().any(|i| eq(i, y)),
                            _ => false,
                        };
                        has == (*op == Op::Contains)
                    }
                    Op::In => match &r {
                        Value::Array(items) => items.iter().any(|i| eq(&l, i)),
                        _ => false,
                    },
                })
            }
        }
    }
    pub fn matches(&self, c: &Ctx<'_>) -> bool {
        truthy(&self.eval(c))
    }
}
