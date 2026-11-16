// SPDX-License-Identifier: CC0-1.0

//! Evaluating a resolver's MAPPING TEMPLATE: the other way a resolver's logic is written.
//!
//! "Mapping templates are written in the Apache Velocity Template Language (VTL)"
//! (`EvaluateMappingTemplate`), and a resolver carries one per direction —
//! `Resolver$requestMappingTemplate` "converts … into a request configuration for the
//! selected data source operation" and `Resolver$responseMappingTemplate` "interpret[s]
//! responses from the data source and map[s] it to the shape of the GraphQL field output
//! type". A resolver defined that way carries no `code`, so running it means EVALUATING
//! those two templates ([`super::execution`] is what dispatches the rendered request to the
//! field's data source and reads the rendered response back).
//!
//! What a template is, and what this evaluates:
//!
//! * TEXT, written out as it stands — which for a request template is the JSON request
//!   object the data source is handed, and for a response template the JSON value the field
//!   resolves to;
//! * REFERENCES (`$context.identity`, `${ctx.args.id}`, `$!{ctx.result}`), which name a
//!   value of the template's own context, walk into it by member and index, and call the
//!   `Map`, `List` and `String` methods of the language on it ([`values`]);
//! * the `$util` HELPERS the resolver reference publishes ([`utilities`]), `$util.toJson`
//!   and the `$util.error` that refuses a field among them;
//! * the DIRECTIVES that make it a template rather than a document: `#set`, `#if` /
//!   `#elseif` / `#else` / `#end`, `#foreach` / `#end` with `$foreach` and `$velocityCount`,
//!   `#break`, `#stop`, the `##` and `#* … *#` comments and the `#[[ … ]]#` block that is
//!   written out unparsed.
//!
//! Every run is bounded in host code the template cannot reach — how long it may run (the
//! budget its caller hands it, [`MAX_RUN`] at most), how many steps it may take
//! ([`MAX_STEPS`]), how large a value it may hold ([`MAX_VALUE_BYTES`]) and how deeply that
//! value may nest ([`MAX_VALUE_DEPTH`]), and how much text it may produce
//! ([`MAX_OUTPUT_BYTES`]) — and so is the PARSE of it, in how long the text may be
//! ([`MAX_TEMPLATE_BYTES`]), how deeply it may nest ([`MAX_NESTING`]) and how far one
//! reference may walk ([`MAX_ACCESSES`]), because the template is caller-supplied and a
//! `#foreach` is a loop. Copying is charged against the step budget by the BYTE
//! ([`BYTES_PER_STEP`]), so a construct that doubles a value runs the budget down in
//! proportion to what it allocates. The CONTEXT is a value, not a handle: a template reads
//! and writes only the JSON it was handed, so nothing of this process is reachable from one,
//! and what it leaves behind (`$ctx.stash`) travels back to the caller as a value too.
//!
//! A template that asks for something this does not implement — a method, a helper — is a
//! FAULT naming it, so the field is reported as one that did not resolve rather than
//! answered with a value nothing computed.

pub mod utilities;
pub mod values;

#[cfg(test)]
mod tests;

use ::serde_json::{Map as JsonMap, Value as Json};

pub use utilities::Raised;

/// How much TEMPLATE this evaluates: the longest a stored one can be — `MappingTemplate` is
/// `@length 1..65536` (`smithy_query(query="shape",shape="MappingTemplate")`), which is what
/// the write path refuses a longer one against. Stated here too, because what parses
/// caller-supplied text states its own ceiling rather than inheriting one.
const MAX_TEMPLATE_BYTES: usize = 65_536;

/// How many steps one evaluation may take: a reference resolved, an expression evaluated or
/// one pass of a loop body. An emulator resource guard, not an AWS quota.
const MAX_STEPS: usize = 100_000;

/// How long one evaluation may run, whatever it spends its steps on, and the longest budget
/// a caller may give one. The evaluation is synchronous host work, so nothing outside it can
/// stop it: the clock is read as the steps are charged ([`Run::charge`]). The same bound the
/// APPSYNC_JS runs of this service are held to
/// (`crate::ops::evaluate_code::appsync_js`), for the same reason.
const MAX_RUN: ::std::time::Duration = ::std::time::Duration::from_secs(2);

/// How often the clock is read: once every so many steps, so the reading itself costs
/// nothing measurable while still stopping a run whose individual steps are expensive — a
/// copy is charged by the byte ([`BYTES_PER_STEP`]), so a large one spends many steps and
/// meets the clock on the way.
const STEPS_PER_CLOCK_READING: usize = 64;

/// How large a value one evaluation may hold: the ceiling on anything a template builds,
/// stores or grows in place. Memory, not steps: a construct that doubles a value is refused
/// here rather than allowed to outrun the step budget.
const MAX_VALUE_BYTES: usize = 256 * 1024;

/// How deeply a value may nest, for the recursion that walking, weighing, rendering and
/// dropping one takes.
const MAX_VALUE_DEPTH: usize = 32;

/// How many bytes of a value one step pays for: copying is charged against the step budget in
/// proportion to its size, so the work a run can do is bounded in bytes as well as in
/// operations.
const BYTES_PER_STEP: usize = 64;

/// How deeply a template may nest — a block within a block, an expression within an
/// expression, a string that is itself a template. A parse guard, so a caller-supplied
/// template cannot drive the parser's own recursion past the host's stack.
const MAX_NESTING: usize = 64;

/// How far one reference may walk: how many members, indexes and calls it may chain. The
/// same guard, for the recursion a walk into a value takes.
const MAX_ACCESSES: usize = 64;

/// How much text one evaluation may produce, for the same reason as the step budget.
const MAX_OUTPUT_BYTES: usize = 1 << 20;

/// What evaluating one template came to.
#[derive(Debug, Clone)]
pub struct Evaluation {
    /// The rendered text.
    pub text: String,
    /// The context as the template left it, which is what carries the `stash` on to the
    /// next step of the resolution.
    pub context: Json,
    /// The errors `$util.appendError` appended, which do not stop an evaluation.
    pub appended: Vec<Raised>,
}

/// Why an evaluation produced no text.
#[derive(Debug, Clone)]
pub enum Fault {
    /// The template RAISED: `$util.error` or `$util.unauthorized`, which is how a template
    /// refuses the field it is resolving. Boxed, because what a raise carries is far larger
    /// than the rest of this error channel.
    Raised(Box<Raised>),
    /// The template could not be evaluated at all: it does not parse, it asks for something
    /// this does not implement, or it ran past one of the bounds above.
    Fault(String),
}

impl Fault {
    /// What this fault states, as the error of the field that did not resolve.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Raised(raised) => raised.message.clone(),
            Self::Fault(reason) => reason.clone(),
        }
    }
}

/// Evaluate `template` over `context` — the `$context` (`$ctx`) a resolver reads — and
/// answer the text it rendered.
///
/// `budget` is how long this run may take, capped at [`MAX_RUN`]: the caller holds the
/// request's own remaining time, so the templates of one request cannot add up past it.
pub fn evaluate(template: &str, context: &Json, budget: ::std::time::Duration) -> Result<Evaluation, Fault> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(Fault::Fault(format!(
            "{TOO_LONG_A_TEMPLATE} {MAX_TEMPLATE_BYTES} bytes."
        )));
    }
    let nodes = parse(template).map_err(Fault::Fault)?;
    let mut env = JsonMap::new();
    env.insert(CONTEXT_KEY.to_string(), context.clone());
    let mut run = Run {
        env,
        out: String::new(),
        steps: 0,
        started: ::std::time::Instant::now(),
        budget: budget.min(MAX_RUN),
        appended: Vec::new(),
    };
    run.block(&nodes)?;
    Ok(Evaluation {
        context: run
            .env
            .get(CONTEXT_KEY)
            .cloned()
            .unwrap_or(Json::Object(JsonMap::new())),
        text: run.out,
        appended: run.appended,
    })
}

/// The one key the context is held under, which both of its names resolve to: a template
/// reads it as `$context` or as `$ctx` ("`$ctx` … an alias of `$context`"), so a `#set` made
/// through one name is read back through the other.
const CONTEXT_KEY: &str = "ctx";

/// The other name the context is read under.
const CONTEXT_ALIAS: &str = "context";

/// The root a `$util` / `$utils` reference names, which is not a value of the context.
const UTILITIES_ROOTS: &[&str] = &["util", "utils"];

/// Which variable a root name reads.
fn root_key(root: &str) -> &str {
    if root == CONTEXT_ALIAS {
        CONTEXT_KEY
    } else {
        root
    }
}

// ---------------------------------------------------------------------------------------
// The template, parsed.
// ---------------------------------------------------------------------------------------

/// One piece of a parsed template.
#[derive(Debug, Clone)]
enum Node {
    /// Text written out as it stands.
    Text(String),
    /// A reference written out: the value, or — for one that resolves to nothing — the
    /// reference's own text, unless it was written quietly (`$!`).
    Write {
        reference: Reference,
        quiet: bool,
        literal: String,
    },
    /// `#set($target = value)`.
    Set { target: Reference, value: Expr },
    /// `#if` with its `#elseif` branches and its `#else` body.
    If {
        branches: Vec<(Expr, Vec<Node>)>,
        otherwise: Vec<Node>,
    },
    /// `#foreach($name in over)`.
    ForEach {
        name: String,
        over: Expr,
        body: Vec<Node>,
    },
    /// `#break`, which ends the innermost loop.
    Break,
    /// `#stop`, which ends the whole evaluation.
    Stop,
}

/// A reference: the variable it starts at, and how it walks into the value.
#[derive(Debug, Clone)]
struct Reference {
    root: String,
    accesses: Vec<Access>,
}

/// One step of a reference's walk.
#[derive(Debug, Clone)]
enum Access {
    /// `.name`
    Prop(String),
    /// `[expr]`
    Index(Expr),
    /// `.name(args)`
    Call(String, Vec<Expr>),
}

/// An expression, as a condition, a `#set` value or an argument.
#[derive(Debug, Clone)]
enum Expr {
    Literal(Json),
    /// A double-quoted string carrying references, which renders as a template of its own.
    Interpolated(Vec<Node>),
    Reference(Reference),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Not(Box<Expr>),
    Negate(Box<Expr>),
    Binary(Operator, Box<Expr>, Box<Expr>),
}

/// The operators an expression may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Or,
    And,
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
}

/// What ended a block of nodes.
#[derive(Debug)]
enum Terminator {
    Eof,
    End,
    Else,
    ElseIf(Expr),
}

/// Parse a whole template.
fn parse(template: &str) -> Result<Vec<Node>, String> {
    let characters: Vec<char> = template.chars().collect();
    let mut parse = Parse {
        src: &characters,
        at: 0,
        nesting: 0,
    };
    let (nodes, terminator) = parse.block()?;
    match terminator {
        Terminator::Eof => Ok(nodes),
        _ => Err(UNEXPECTED_DIRECTIVE.to_string()),
    }
}

/// The parser's cursor over one template.
struct Parse<'t> {
    src: &'t [char],
    at: usize,
    /// How deeply the parse is nested, which [`MAX_NESTING`] bounds.
    nesting: usize,
}

impl Parse<'_> {
    /// Enter one level of nesting, or refuse a template that nests past the bound.
    fn deeper(&mut self) -> Result<(), String> {
        self.nesting = self.nesting.saturating_add(1);
        if self.nesting > MAX_NESTING {
            return Err(format!("{TOO_DEEP} {MAX_NESTING}."));
        }
        Ok(())
    }

    /// Leave it again.
    fn shallower(&mut self) {
        self.nesting = self.nesting.saturating_sub(1);
    }

    /// Parse nodes until the template ends or a directive that closes a block is reached.
    fn block(&mut self) -> Result<(Vec<Node>, Terminator), String> {
        self.deeper()?;
        let parsed = self.block_nodes();
        self.shallower();
        parsed
    }

    /// The body of [`Self::block`], which that one bounds the nesting of.
    fn block_nodes(&mut self) -> Result<(Vec<Node>, Terminator), String> {
        let mut nodes: Vec<Node> = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, nodes: &mut Vec<Node>| {
            if !text.is_empty() {
                nodes.push(Node::Text(::std::mem::take(text)));
            }
        };
        while let Some(character) = self.peek(0) {
            match character {
                // An escaped `$` or `#` is the character itself.
                '\\' if matches!(self.peek(1), Some('$' | '#')) => {
                    text.push(self.peek(1).unwrap_or('\\'));
                    self.at += 2;
                }
                '#' => {
                    // A comment, an unparsed block, a directive — or, for anything else, a
                    // `#` of the text.
                    if self.matches(&['#', '#']) {
                        self.skip_line();
                        continue;
                    }
                    if self.matches(&['#', '*']) {
                        self.at += 2;
                        self.skip_until(&['*', '#']);
                        continue;
                    }
                    if self.matches(&['#', '[', '[']) {
                        self.at += 3;
                        let start = self.at;
                        // Text written out UNPARSED, to its closing marker or to the end of
                        // the template when it carries none.
                        let end = if self.skip_until(&[']', ']', '#']) {
                            self.at.saturating_sub(3)
                        } else {
                            self.at
                        };
                        text.extend(self.src[start..end.max(start)].iter());
                        continue;
                    }
                    let Some(name) = self.directive_name() else {
                        text.push('#');
                        self.at += 1;
                        continue;
                    };
                    match name.as_str() {
                        "set" => {
                            let (target, value) = self.assignment()?;
                            flush(&mut text, &mut nodes);
                            nodes.push(Node::Set { target, value });
                            self.skip_trailing_newline();
                        }
                        "if" => {
                            let node = self.conditional()?;
                            flush(&mut text, &mut nodes);
                            nodes.push(node);
                        }
                        "foreach" => {
                            let node = self.loop_over()?;
                            flush(&mut text, &mut nodes);
                            nodes.push(node);
                        }
                        "break" | "stop" => {
                            flush(&mut text, &mut nodes);
                            nodes.push(if name == "break" {
                                Node::Break
                            } else {
                                Node::Stop
                            });
                            self.skip_trailing_newline();
                        }
                        "end" => {
                            self.skip_trailing_newline();
                            flush(&mut text, &mut nodes);
                            return Ok((nodes, Terminator::End));
                        }
                        "else" => {
                            self.skip_trailing_newline();
                            flush(&mut text, &mut nodes);
                            return Ok((nodes, Terminator::Else));
                        }
                        "elseif" => {
                            let condition = self.parenthesised()?;
                            self.skip_trailing_newline();
                            flush(&mut text, &mut nodes);
                            return Ok((nodes, Terminator::ElseIf(condition)));
                        }
                        // A directive this does not know is text: a `#` followed by a word
                        // is only a directive when it is one of the above.
                        _ => {
                            text.push('#');
                            self.at += 1;
                        }
                    }
                }
                '$' => match self.reference() {
                    Some((reference, quiet, literal)) => {
                        flush(&mut text, &mut nodes);
                        nodes.push(Node::Write {
                            reference,
                            quiet,
                            literal,
                        });
                    }
                    None => {
                        text.push('$');
                        self.at += 1;
                    }
                },
                other => {
                    text.push(other);
                    self.at += 1;
                }
            }
        }
        if !text.is_empty() {
            nodes.push(Node::Text(text));
        }
        Ok((nodes, Terminator::Eof))
    }

    /// The character `ahead` positions on, when the template has one.
    fn peek(&self, ahead: usize) -> Option<char> {
        self.src.get(self.at.saturating_add(ahead)).copied()
    }

    /// Whether the cursor is on `expected`.
    fn matches(&self, expected: &[char]) -> bool {
        expected
            .iter()
            .enumerate()
            .all(|(offset, character)| self.peek(offset) == Some(*character))
    }

    /// Skip to just past the end of this line.
    fn skip_line(&mut self) {
        while let Some(character) = self.peek(0) {
            self.at += 1;
            if character == '\n' {
                return;
            }
        }
    }

    /// Skip to just past the next `marker`, or to the end of the template; `true` when the
    /// marker was the reason it stopped.
    fn skip_until(&mut self, marker: &[char]) -> bool {
        while self.at < self.src.len() {
            if self.matches(marker) {
                self.at = self.at.saturating_add(marker.len());
                return true;
            }
            self.at += 1;
        }
        false
    }

    /// Velocity eats the line break that follows a directive, so a template written one
    /// directive per line renders without the blank lines between them.
    fn skip_trailing_newline(&mut self) {
        let mut at = self.at;
        while matches!(self.src.get(at), Some(' ' | '\t')) {
            at += 1;
        }
        if self.src.get(at) == Some(&'\r') {
            at += 1;
        }
        if self.src.get(at) == Some(&'\n') {
            self.at = at.saturating_add(1);
        }
    }

    /// Skip the whitespace an expression may be written around.
    fn skip_spaces(&mut self) {
        while matches!(self.peek(0), Some(character) if character.is_whitespace()) {
            self.at += 1;
        }
    }

    /// The name of the directive the cursor is on, with the cursor left just past it —
    /// `None` for a `#` that starts no directive, with the cursor left where it was.
    fn directive_name(&mut self) -> Option<String> {
        let start = self.at;
        self.at += 1;
        let braced = self.peek(0) == Some('{');
        if braced {
            self.at += 1;
        }
        let name = self.identifier();
        match name {
            Some(name) if !name.is_empty() => {
                if braced && self.peek(0) == Some('}') {
                    self.at += 1;
                }
                Some(name)
            }
            _ => {
                self.at = start;
                None
            }
        }
    }

    /// The identifier the cursor is on, with the cursor left just past it.
    fn identifier(&mut self) -> Option<String> {
        let first = self.peek(0)?;
        if !(first.is_alphabetic() || first == '_') {
            return None;
        }
        let mut name = String::new();
        while let Some(character) = self.peek(0) {
            if character.is_alphanumeric() || character == '_' || character == '-' {
                name.push(character);
                self.at += 1;
            } else {
                break;
            }
        }
        Some(name)
    }

    /// The reference the cursor is on, its quiet flag and its own text — `None` for a `$`
    /// that starts no reference, with the cursor left where it was.
    fn reference(&mut self) -> Option<(Reference, bool, String)> {
        let start = self.at;
        self.at += 1;
        let quiet = self.peek(0) == Some('!');
        if quiet {
            self.at += 1;
        }
        let braced = self.peek(0) == Some('{');
        if braced {
            self.at += 1;
        }
        let Some(root) = self.identifier() else {
            self.at = start;
            return None;
        };
        let mut accesses = Vec::new();
        loop {
            if self.peek(0) == Some('.')
                && matches!(self.peek(1), Some(character) if character.is_alphabetic() || character == '_')
            {
                self.at += 1;
                let Some(name) = self.identifier() else { break };
                if self.peek(0) == Some('(') {
                    match self.arguments() {
                        Ok(args) => accesses.push(Access::Call(name, args)),
                        Err(_) => {
                            self.at = start;
                            return None;
                        }
                    }
                } else {
                    accesses.push(Access::Prop(name));
                }
                continue;
            }
            if self.peek(0) == Some('[') {
                self.at += 1;
                let Ok(index) = self.expression() else {
                    self.at = start;
                    return None;
                };
                self.skip_spaces();
                if self.peek(0) != Some(']') {
                    self.at = start;
                    return None;
                }
                self.at += 1;
                accesses.push(Access::Index(index));
                continue;
            }
            break;
        }
        if braced {
            if self.peek(0) != Some('}') {
                self.at = start;
                return None;
            }
            self.at += 1;
        }
        if accesses.len() > MAX_ACCESSES {
            self.at = start;
            return None;
        }
        let literal: String = self.src[start..self.at].iter().collect();
        Some((Reference { root, accesses }, quiet, literal))
    }

    /// The argument list the cursor is on: `(` expressions `)`.
    fn arguments(&mut self) -> Result<Vec<Expr>, String> {
        if self.peek(0) != Some('(') {
            return Err(NOT_AN_ARGUMENT_LIST.to_string());
        }
        self.at += 1;
        let mut args = Vec::new();
        self.skip_spaces();
        if self.peek(0) == Some(')') {
            self.at += 1;
            return Ok(args);
        }
        loop {
            args.push(self.expression()?);
            self.skip_spaces();
            match self.peek(0) {
                Some(',') => {
                    self.at += 1;
                    self.skip_spaces();
                }
                Some(')') => {
                    self.at += 1;
                    return Ok(args);
                }
                _ => return Err(NOT_AN_ARGUMENT_LIST.to_string()),
            }
        }
    }

    /// The `( expression )` a directive takes.
    fn parenthesised(&mut self) -> Result<Expr, String> {
        self.skip_spaces();
        if self.peek(0) != Some('(') {
            return Err(NOT_A_DIRECTIVE.to_string());
        }
        self.at += 1;
        let expression = self.expression()?;
        self.skip_spaces();
        if self.peek(0) != Some(')') {
            return Err(NOT_A_DIRECTIVE.to_string());
        }
        self.at += 1;
        Ok(expression)
    }

    /// The `( $target = value )` a `#set` takes.
    fn assignment(&mut self) -> Result<(Reference, Expr), String> {
        self.skip_spaces();
        if self.peek(0) != Some('(') {
            return Err(NOT_A_DIRECTIVE.to_string());
        }
        self.at += 1;
        self.skip_spaces();
        let Some((target, _, _)) = self.reference() else {
            return Err(NOT_AN_ASSIGNMENT.to_string());
        };
        self.skip_spaces();
        if self.peek(0) != Some('=') {
            return Err(NOT_AN_ASSIGNMENT.to_string());
        }
        self.at += 1;
        let value = self.expression()?;
        self.skip_spaces();
        if self.peek(0) != Some(')') {
            return Err(NOT_AN_ASSIGNMENT.to_string());
        }
        self.at += 1;
        Ok((target, value))
    }

    /// `#if` through its `#end`.
    fn conditional(&mut self) -> Result<Node, String> {
        let mut condition = self.parenthesised()?;
        self.skip_trailing_newline();
        let mut branches = Vec::new();
        loop {
            let (body, terminator) = self.block()?;
            match terminator {
                Terminator::End => {
                    branches.push((condition, body));
                    return Ok(Node::If {
                        branches,
                        otherwise: Vec::new(),
                    });
                }
                Terminator::ElseIf(next) => {
                    branches.push((condition, body));
                    condition = next;
                }
                Terminator::Else => {
                    branches.push((condition, body));
                    let (otherwise, terminator) = self.block()?;
                    return match terminator {
                        Terminator::End => Ok(Node::If {
                            branches,
                            otherwise,
                        }),
                        _ => Err(NO_END.to_string()),
                    };
                }
                Terminator::Eof => return Err(NO_END.to_string()),
            }
        }
    }

    /// `#foreach` through its `#end`.
    fn loop_over(&mut self) -> Result<Node, String> {
        self.skip_spaces();
        if self.peek(0) != Some('(') {
            return Err(NOT_A_DIRECTIVE.to_string());
        }
        self.at += 1;
        self.skip_spaces();
        if self.peek(0) != Some('$') {
            return Err(NOT_A_LOOP.to_string());
        }
        self.at += 1;
        let braced = self.peek(0) == Some('{');
        if braced {
            self.at += 1;
        }
        let Some(name) = self.identifier() else {
            return Err(NOT_A_LOOP.to_string());
        };
        if braced && self.peek(0) == Some('}') {
            self.at += 1;
        }
        self.skip_spaces();
        if !self.matches(&['i', 'n']) {
            return Err(NOT_A_LOOP.to_string());
        }
        self.at += 2;
        let over = self.expression()?;
        self.skip_spaces();
        if self.peek(0) != Some(')') {
            return Err(NOT_A_LOOP.to_string());
        }
        self.at += 1;
        self.skip_trailing_newline();
        let (body, terminator) = self.block()?;
        match terminator {
            Terminator::End => Ok(Node::ForEach { name, over, body }),
            _ => Err(NO_END.to_string()),
        }
    }

    /// One expression, lowest precedence first.
    fn expression(&mut self) -> Result<Expr, String> {
        self.deeper()?;
        let parsed = self.either();
        self.shallower();
        parsed
    }

    /// A left-associative operator CHAIN: `operand` parses each side and `operator` reads the
    /// operator at the cursor, answering it and how many characters it takes.
    ///
    /// Every operator is one more level of the tree this builds, charged against the nesting
    /// bound like every other level — evaluation recurses over that tree, so its depth is
    /// bounded however the text spells it — and the levels are released when the chain ends.
    fn chain(
        &mut self,
        operator: impl Fn(&mut Self) -> Option<(Operator, usize)>,
        operand: impl Fn(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        let mut left = operand(self)?;
        let mut charged = 0usize;
        let parsed = loop {
            self.skip_spaces();
            let Some((named, width)) = operator(self) else {
                break Ok(left);
            };
            self.at = self.at.saturating_add(width);
            if let Err(why) = self.deeper() {
                break Err(why);
            }
            charged = charged.saturating_add(1);
            match operand(self) {
                Ok(right) => left = Expr::Binary(named, Box::new(left), Box::new(right)),
                Err(why) => break Err(why),
            }
        };
        for _ in 0..charged {
            self.shallower();
        }
        parsed
    }

    fn either(&mut self) -> Result<Expr, String> {
        self.chain(
            |parse| parse.matches(&['|', '|']).then_some((Operator::Or, 2)),
            Self::both,
        )
    }

    fn both(&mut self) -> Result<Expr, String> {
        self.chain(
            |parse| parse.matches(&['&', '&']).then_some((Operator::And, 2)),
            Self::equality,
        )
    }

    fn equality(&mut self) -> Result<Expr, String> {
        self.chain(
            |parse| {
                if parse.matches(&['=', '=']) {
                    Some((Operator::Equal, 2))
                } else if parse.matches(&['!', '=']) {
                    Some((Operator::NotEqual, 2))
                } else {
                    None
                }
            },
            Self::relational,
        )
    }

    fn relational(&mut self) -> Result<Expr, String> {
        self.chain(
            |parse| {
                if parse.matches(&['<', '=']) {
                    Some((Operator::LessOrEqual, 2))
                } else if parse.matches(&['>', '=']) {
                    Some((Operator::GreaterOrEqual, 2))
                } else if parse.matches(&['<']) {
                    Some((Operator::Less, 1))
                } else if parse.matches(&['>']) {
                    Some((Operator::Greater, 1))
                } else {
                    None
                }
            },
            Self::additive,
        )
    }

    fn additive(&mut self) -> Result<Expr, String> {
        self.chain(
            |parse| match parse.peek(0) {
                Some('+') => Some((Operator::Add, 1)),
                // A `-` that starts a number is part of it, not an operator; the cursor is
                // past an operand here, so this one is the operator.
                Some('-') => Some((Operator::Subtract, 1)),
                _ => None,
            },
            Self::multiplicative,
        )
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        self.chain(
            |parse| match parse.peek(0) {
                Some('*') => Some((Operator::Multiply, 1)),
                Some('/') => Some((Operator::Divide, 1)),
                Some('%') => Some((Operator::Remainder, 1)),
                _ => None,
            },
            Self::unary,
        )
    }

    fn unary(&mut self) -> Result<Expr, String> {
        self.skip_spaces();
        match self.peek(0) {
            // A prefix operator is one more level of nesting, charged like every other, so a
            // chain of them cannot drive this recursion past the bound.
            Some('!') if self.peek(1) != Some('=') => {
                self.at += 1;
                self.deeper()?;
                let inner = self.unary();
                self.shallower();
                Ok(Expr::Not(Box::new(inner?)))
            }
            Some('-') if !matches!(self.peek(1), Some(digit) if digit.is_ascii_digit()) => {
                self.at += 1;
                self.deeper()?;
                let inner = self.unary();
                self.shallower();
                Ok(Expr::Negate(Box::new(inner?)))
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        self.skip_spaces();
        match self.peek(0) {
            Some('(') => {
                self.at += 1;
                let inner = self.expression()?;
                self.skip_spaces();
                if self.peek(0) != Some(')') {
                    return Err(NOT_AN_EXPRESSION.to_string());
                }
                self.at += 1;
                Ok(inner)
            }
            Some('$') => match self.reference() {
                Some((reference, _, _)) => Ok(Expr::Reference(reference)),
                None => Err(NOT_AN_EXPRESSION.to_string()),
            },
            Some('\'') => {
                let text = self.quoted('\'')?;
                Ok(Expr::Literal(Json::String(text)))
            }
            Some('"') => {
                let text = self.quoted('"')?;
                // A double-quoted string carries references, so it renders as a template.
                if text.contains('$') || text.contains('#') {
                    let nested: Vec<char> = text.chars().collect();
                    let mut parse = Parse {
                        src: &nested,
                        at: 0,
                        nesting: self.nesting,
                    };
                    let (nodes, terminator) = parse.block()?;
                    return match terminator {
                        Terminator::Eof => Ok(Expr::Interpolated(nodes)),
                        _ => Err(UNEXPECTED_DIRECTIVE.to_string()),
                    };
                }
                Ok(Expr::Literal(Json::String(text)))
            }
            Some('[') => {
                self.at += 1;
                let mut elements = Vec::new();
                self.skip_spaces();
                if self.peek(0) == Some(']') {
                    self.at += 1;
                    return Ok(Expr::List(elements));
                }
                loop {
                    elements.push(self.expression()?);
                    self.skip_spaces();
                    match self.peek(0) {
                        Some(',') => self.at += 1,
                        Some(']') => {
                            self.at += 1;
                            return Ok(Expr::List(elements));
                        }
                        _ => return Err(NOT_AN_EXPRESSION.to_string()),
                    }
                }
            }
            Some('{') => {
                self.at += 1;
                let mut entries = Vec::new();
                self.skip_spaces();
                if self.peek(0) == Some('}') {
                    self.at += 1;
                    return Ok(Expr::Map(entries));
                }
                loop {
                    let key = self.expression()?;
                    self.skip_spaces();
                    if self.peek(0) != Some(':') {
                        return Err(NOT_AN_EXPRESSION.to_string());
                    }
                    self.at += 1;
                    let value = self.expression()?;
                    entries.push((key, value));
                    self.skip_spaces();
                    match self.peek(0) {
                        Some(',') => self.at += 1,
                        Some('}') => {
                            self.at += 1;
                            return Ok(Expr::Map(entries));
                        }
                        _ => return Err(NOT_AN_EXPRESSION.to_string()),
                    }
                }
            }
            Some(character) if character.is_ascii_digit() || character == '-' => self.number(),
            Some(character) if character.is_alphabetic() => {
                let Some(word) = self.identifier() else {
                    return Err(NOT_AN_EXPRESSION.to_string());
                };
                match word.as_str() {
                    "true" => Ok(Expr::Literal(Json::Bool(true))),
                    "false" => Ok(Expr::Literal(Json::Bool(false))),
                    "null" => Ok(Expr::Literal(Json::Null)),
                    _ => Err(NOT_AN_EXPRESSION.to_string()),
                }
            }
            _ => Err(NOT_AN_EXPRESSION.to_string()),
        }
    }

    /// The number the cursor is on.
    fn number(&mut self) -> Result<Expr, String> {
        let start = self.at;
        if self.peek(0) == Some('-') {
            self.at += 1;
        }
        let mut seen_point = false;
        while let Some(character) = self.peek(0) {
            if character.is_ascii_digit() {
                self.at += 1;
            } else if character == '.'
                && !seen_point
                && matches!(self.peek(1), Some(digit) if digit.is_ascii_digit())
            {
                seen_point = true;
                self.at += 1;
            } else {
                break;
            }
        }
        let text: String = self.src[start..self.at].iter().collect();
        text.parse::<f64>()
            .map(|parsed| Expr::Literal(values::number(parsed)))
            .map_err(|_| NOT_AN_EXPRESSION.to_string())
    }

    /// The quoted string the cursor is on, with its escapes resolved.
    fn quoted(&mut self, quote: char) -> Result<String, String> {
        self.at += 1;
        let mut text = String::new();
        while let Some(character) = self.peek(0) {
            self.at += 1;
            if character == '\\' {
                if let Some(escaped) = self.peek(0) {
                    self.at += 1;
                    text.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                }
                continue;
            }
            if character == quote {
                return Ok(text);
            }
            text.push(character);
        }
        Err(NOT_AN_EXPRESSION.to_string())
    }
}

// ---------------------------------------------------------------------------------------
// The template, evaluated.
// ---------------------------------------------------------------------------------------

/// Where one evaluation reached a loop or the end of the template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Break,
    Stop,
}

/// One evaluation: the variables it holds, the text it has produced, and the bounds it is
/// held to.
struct Run {
    env: JsonMap<String, Json>,
    out: String,
    steps: usize,
    /// When this evaluation began, which is what its budget is measured from.
    started: ::std::time::Instant,
    /// How long this evaluation may run, at most [`MAX_RUN`].
    budget: ::std::time::Duration,
    appended: Vec<Raised>,
}

impl Run {
    /// Charge one step against this run's budget, and against its clock.
    fn charge(&mut self) -> Result<(), Fault> {
        self.charge_steps(1)
    }

    /// Charge `steps` of them at once — what copying a value costs, by the byte
    /// ([`BYTES_PER_STEP`]).
    fn charge_steps(&mut self, steps: usize) -> Result<(), Fault> {
        let before = self.steps;
        self.steps = self.steps.saturating_add(steps.max(1));
        if self.steps > MAX_STEPS {
            return Err(Fault::Fault(format!("{TOO_MANY_STEPS} {MAX_STEPS}.")));
        }
        // The clock is read when the count crosses a reading point, so a charge of many
        // steps at once meets it too.
        if before / STEPS_PER_CLOCK_READING != self.steps / STEPS_PER_CLOCK_READING
            && self.started.elapsed() > self.budget
        {
            return Err(Fault::Fault(format!(
                "{TOO_LONG} {} ms.",
                self.budget.as_millis()
            )));
        }
        Ok(())
    }

    /// Account for a value this run has built or is about to hold: refuse one past the size
    /// or the nesting a value may have, and charge what copying it cost.
    ///
    /// This is the MEMORY bound beside the step budget: a construct that grows a value — a
    /// list or map built from what a variable already holds, a string added to itself, a
    /// collection added to itself — is refused at [`MAX_VALUE_BYTES`] and pays for every byte
    /// it copied on the way, so it cannot outrun the budget.
    fn account(&mut self, value: &Json) -> Result<(), Fault> {
        let weight = weigh(value, 0).map_err(Fault::Fault)?;
        self.charge_steps(weight / BYTES_PER_STEP)
    }

    /// Write `text` into the rendered output.
    fn write(&mut self, text: &str) -> Result<(), Fault> {
        if self.out.len().saturating_add(text.len()) > MAX_OUTPUT_BYTES {
            return Err(Fault::Fault(format!(
                "{TOO_MUCH_OUTPUT} {MAX_OUTPUT_BYTES} bytes."
            )));
        }
        self.out.push_str(text);
        Ok(())
    }

    /// Evaluate a block of nodes.
    fn block(&mut self, nodes: &[Node]) -> Result<Flow, Fault> {
        for node in nodes {
            self.charge()?;
            match node {
                Node::Text(text) => self.write(text)?,
                Node::Write {
                    reference,
                    quiet,
                    literal,
                } => {
                    let resolved = self.resolve(reference)?;
                    match resolved {
                        // A reference that names nothing writes its own text, which is what
                        // Velocity renders an unresolved one as — unless it was written
                        // quietly (`$!`), which is what quiet means.
                        Resolved::Missing => {
                            if !quiet {
                                let literal = literal.clone();
                                self.write(&literal)?;
                            }
                        }
                        // A call made for its effect rather than for its value writes
                        // nothing at all.
                        Resolved::Nothing => {}
                        Resolved::Value(value) => {
                            let rendered = values::rendered(&value);
                            self.write(&rendered)?;
                        }
                    }
                }
                Node::Set { target, value } => {
                    let value = self.expr(value)?;
                    self.assign(target, value)?;
                }
                Node::If {
                    branches,
                    otherwise,
                } => {
                    let mut taken = false;
                    for (condition, body) in branches {
                        let decided = self.expr(condition)?;
                        if values::truthy(Some(&decided)) {
                            taken = true;
                            match self.block(body)? {
                                Flow::Continue => {}
                                other => return Ok(other),
                            }
                            break;
                        }
                    }
                    if !taken {
                        match self.block(otherwise)? {
                            Flow::Continue => {}
                            other => return Ok(other),
                        }
                    }
                }
                Node::ForEach { name, over, body } => match self.iterate(name, over, body)? {
                    Flow::Stop => return Ok(Flow::Stop),
                    _ => continue,
                },
                Node::Break => return Ok(Flow::Break),
                Node::Stop => return Ok(Flow::Stop),
            }
        }
        Ok(Flow::Continue)
    }

    /// One `#foreach`: each element of the value, under the loop's own variable and beside
    /// the `$foreach` and `$velocityCount` the body reads.
    fn iterate(&mut self, name: &str, over: &Expr, body: &[Node]) -> Result<Flow, Fault> {
        let elements = match self.expr(over)? {
            Json::Array(list) => list,
            Json::Object(map) => map.values().cloned().collect(),
            Json::Null => Vec::new(),
            single => vec![single],
        };
        let held = self.env.get(name).cloned();
        let held_loop = self.env.get(FOREACH_VARIABLE).cloned();
        let held_count = self.env.get(COUNT_VARIABLE).cloned();
        let mut flow = Flow::Continue;
        for (index, element) in elements.iter().enumerate() {
            self.charge()?;
            self.env.insert(name.to_string(), element.clone());
            let count = index.saturating_add(1);
            self.env.insert(
                FOREACH_VARIABLE.to_string(),
                ::serde_json::json!({
                    "index": index,
                    "count": count,
                    "hasNext": count < elements.len(),
                }),
            );
            self.env
                .insert(COUNT_VARIABLE.to_string(), ::serde_json::json!(count));
            match self.block(body)? {
                Flow::Continue => {}
                Flow::Break => break,
                Flow::Stop => {
                    flow = Flow::Stop;
                    break;
                }
            }
        }
        // The loop's own variables are the loop's: what the template held under those names
        // before it stands again after it.
        for (name, held) in [
            (name, held),
            (FOREACH_VARIABLE, held_loop),
            (COUNT_VARIABLE, held_count),
        ] {
            match held {
                Some(value) => self.env.insert(name.to_string(), value),
                None => self.env.remove(name),
            };
        }
        Ok(flow)
    }

    /// What one expression comes to. An absent value is `null`, which is what a condition
    /// and an argument read it as.
    ///
    /// Every value an expression produces is ACCOUNTED for ([`Run::account`]) before it is
    /// answered, so a construct that grows one is refused at the size bound and pays for the
    /// bytes it copied.
    fn expr(&mut self, expression: &Expr) -> Result<Json, Fault> {
        self.charge()?;
        let produced = self.produced(expression)?;
        self.account(&produced)?;
        Ok(produced)
    }

    /// The value one expression produces, before it is accounted for.
    fn produced(&mut self, expression: &Expr) -> Result<Json, Fault> {
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::Interpolated(nodes) => {
                // Rendered into THIS run's own output buffer, borrowed for the nested
                // block and handed back: the nested text is charged against the same
                // output budget, and nothing of the environment is copied.
                let held = ::std::mem::take(&mut self.out);
                let nested = self.block(nodes);
                let rendered = ::std::mem::replace(&mut self.out, held);
                nested?;
                Ok(Json::String(rendered))
            }
            Expr::Reference(reference) => Ok(self.resolve(reference)?.value()),
            Expr::List(elements) => {
                let mut values = Vec::with_capacity(elements.len());
                for element in elements {
                    values.push(self.expr(element)?);
                }
                Ok(Json::Array(values))
            }
            Expr::Map(entries) => {
                let mut map = JsonMap::new();
                for (key, value) in entries {
                    let key = self.expr(key)?;
                    let value = self.expr(value)?;
                    map.insert(
                        match key {
                            Json::String(key) => key,
                            other => values::rendered(&other),
                        },
                        value,
                    );
                }
                Ok(Json::Object(map))
            }
            Expr::Not(inner) => {
                let decided = self.expr(inner)?;
                Ok(Json::Bool(!values::truthy(Some(&decided))))
            }
            Expr::Negate(inner) => {
                let value = self.expr(inner)?;
                values::arithmetic('-', &Json::Number(0.into()), &value).map_err(Fault::Fault)
            }
            Expr::Binary(operator, left, right) => self.binary(*operator, left, right),
        }
    }

    /// What one binary expression comes to.
    fn binary(&mut self, operator: Operator, left: &Expr, right: &Expr) -> Result<Json, Fault> {
        // The logical operators decide the left side first and the right side only where it
        // can change the answer, as the language's own short circuit does.
        if matches!(operator, Operator::Or | Operator::And) {
            let left = self.expr(left)?;
            let decided = values::truthy(Some(&left));
            if (operator == Operator::Or) == decided {
                return Ok(Json::Bool(decided));
            }
            let right = self.expr(right)?;
            return Ok(Json::Bool(values::truthy(Some(&right))));
        }
        let left = self.expr(left)?;
        let right = self.expr(right)?;
        let ordered = |decided: ::std::cmp::Ordering, wanted: &[::std::cmp::Ordering]| {
            Json::Bool(wanted.contains(&decided))
        };
        use ::std::cmp::Ordering::{Equal, Greater, Less};
        match operator {
            Operator::Equal => Ok(Json::Bool(values::equal(Some(&left), Some(&right)))),
            Operator::NotEqual => Ok(Json::Bool(!values::equal(Some(&left), Some(&right)))),
            Operator::Less | Operator::LessOrEqual | Operator::Greater
            | Operator::GreaterOrEqual => {
                let decided =
                    values::ordering(Some(&left), Some(&right)).map_err(Fault::Fault)?;
                Ok(match operator {
                    Operator::Less => ordered(decided, &[Less]),
                    Operator::LessOrEqual => ordered(decided, &[Less, Equal]),
                    Operator::Greater => ordered(decided, &[Greater]),
                    _ => ordered(decided, &[Greater, Equal]),
                })
            }
            Operator::Add => values::arithmetic('+', &left, &right).map_err(Fault::Fault),
            Operator::Subtract => values::arithmetic('-', &left, &right).map_err(Fault::Fault),
            Operator::Multiply => values::arithmetic('*', &left, &right).map_err(Fault::Fault),
            Operator::Divide => values::arithmetic('/', &left, &right).map_err(Fault::Fault),
            Operator::Remainder => values::arithmetic('%', &left, &right).map_err(Fault::Fault),
            Operator::Or | Operator::And => Ok(Json::Bool(false)),
        }
    }

    /// What one reference resolves to.
    fn resolve(&mut self, reference: &Reference) -> Result<Resolved, Fault> {
        self.charge()?;
        // Every argument is evaluated BEFORE the value the reference walks into is reached,
        // because an argument may read — and a method may change — that very value.
        let mut args: Vec<Vec<Json>> = Vec::with_capacity(reference.accesses.len());
        for access in &reference.accesses {
            match access {
                Access::Prop(_) => args.push(Vec::new()),
                Access::Index(index) => {
                    let value = self.expr(index)?;
                    args.push(vec![value]);
                }
                Access::Call(_, given) => {
                    let mut row = Vec::with_capacity(given.len());
                    for expression in given {
                        row.push(self.expr(expression)?);
                    }
                    args.push(row);
                }
            }
        }
        if UTILITIES_ROOTS.contains(&reference.root.as_str()) {
            return self.utility(reference, &args);
        }
        let mutating = reference
            .accesses
            .iter()
            .any(|access| matches!(access, Access::Call(..)));
        let key = root_key(&reference.root).to_string();
        let Some(root) = self.env.get_mut(&key) else {
            return Ok(Resolved::Missing);
        };
        let walked = walk(root, &reference.accesses, &args)
            .map(Resolved::of)
            .map_err(Fault::Fault)?;
        // A method may have CHANGED the value it was called on — `put`, `add`, `addAll` are
        // how a template grows one — so what the variable now holds is accounted for.
        if mutating {
            if let Some(held) = self.env.get(&key).cloned() {
                self.account(&held)?;
            }
        }
        Ok(walked)
    }

    /// What one `$util` reference answers.
    fn utility(&mut self, reference: &Reference, args: &[Vec<Json>]) -> Result<Resolved, Fault> {
        let mut path: Vec<String> = Vec::new();
        let mut called: Option<&[Json]> = None;
        let mut rest: &[Access] = &[];
        let mut rest_args: &[Vec<Json>] = &[];
        for (at, access) in reference.accesses.iter().enumerate() {
            match access {
                Access::Prop(name) => path.push(name.clone()),
                Access::Call(name, _) => {
                    path.push(name.clone());
                    called = args.get(at).map(Vec::as_slice);
                    rest = reference.accesses.get(at.saturating_add(1)..).unwrap_or(&[]);
                    rest_args = args.get(at.saturating_add(1)..).unwrap_or(&[]);
                    break;
                }
                Access::Index(_) => return Err(Fault::Fault(NO_UTIL_INDEX.to_string())),
            }
        }
        // `$util` on its own, or a member of it that is never called, names no helper.
        let Some(given) = called else {
            return Err(Fault::Fault(format!(
                "{NO_SUCH_HELPER}$util{}{}",
                if path.is_empty() { "" } else { "." },
                path.join(".")
            )));
        };
        let context = self.env.get(CONTEXT_KEY).cloned().unwrap_or(Json::Null);
        let answered = utilities::call(&path, given, &context).map_err(Fault::Fault)?;
        match answered {
            utilities::Answer::Value(mut value) => walk(&mut value, rest, rest_args)
                .map(Resolved::of)
                .map_err(Fault::Fault),
            // A helper that answers nothing — and one that answers `null` — writes nothing:
            // the helper WAS called, so its own text is not what the template renders.
            utilities::Answer::Nothing => Ok(Resolved::Nothing),
            utilities::Answer::Raised(raised) => Err(Fault::Raised(Box::new(raised))),
            utilities::Answer::Appended(appended) => {
                self.appended.push(appended);
                Ok(Resolved::Nothing)
            }
        }
    }

    /// `#set`: write `value` where `target` points, creating the maps on the way.
    fn assign(&mut self, target: &Reference, value: Json) -> Result<(), Fault> {
        self.account(&value)?;
        let mut args: Vec<Vec<Json>> = Vec::with_capacity(target.accesses.len());
        for access in &target.accesses {
            match access {
                Access::Index(index) => {
                    let value = self.expr(index)?;
                    args.push(vec![value]);
                }
                _ => args.push(Vec::new()),
            }
        }
        let key = root_key(&target.root).to_string();
        if target.accesses.is_empty() {
            self.env.insert(key, value);
            return Ok(());
        }
        let slot = self
            .env
            .entry(key)
            .or_insert_with(|| Json::Object(JsonMap::new()));
        place(slot, &target.accesses, &args, value).map_err(Fault::Fault)
    }
}

/// The variables a `#foreach` body reads its position from.
const FOREACH_VARIABLE: &str = "foreach";
const COUNT_VARIABLE: &str = "velocityCount";

/// What a reference came to, which is what decides how it is WRITTEN.
#[derive(Debug, Clone)]
enum Resolved {
    /// A value, written as its own text.
    Value(Json),
    /// The reference names nothing: no such variable, no such member, or the member holds
    /// `null`, which Velocity's own renderer treats as not found.
    Missing,
    /// The reference is a call that produced no output of its own.
    Nothing,
}

impl Resolved {
    /// What a walk into a value came to.
    fn of(walked: Option<Json>) -> Self {
        match walked {
            None | Some(Json::Null) => Self::Missing,
            Some(value) => Self::Value(value),
        }
    }

    /// This as the value an expression reads it as: everything that is not a value is
    /// `null`, which is what a condition and an argument take it for.
    fn value(self) -> Json {
        match self {
            Self::Value(value) => value,
            _ => Json::Null,
        }
    }
}

/// Walk `accesses` into `target` and answer what the walk reached, or `None` for a walk that
/// names nothing.
///
/// `target` is borrowed mutably because a method call is allowed to change the value it is
/// called on, which is how a template builds up a map or a list it holds.
fn walk(
    target: &mut Json,
    accesses: &[Access],
    args: &[Vec<Json>],
) -> Result<Option<Json>, String> {
    let Some((first, rest)) = accesses.split_first() else {
        return Ok(Some(target.clone()));
    };
    let (row, rest_args) = split(args);
    match first {
        Access::Prop(name) => {
            if matches!(target, Json::Object(map) if map.contains_key(name)) {
                let Json::Object(map) = target else {
                    return Ok(None);
                };
                let Some(child) = map.get_mut(name) else {
                    return Ok(None);
                };
                return walk(child, rest, rest_args);
            }
            // A property that is no member is the no-argument method of that name, which is
            // how the language reads `$list.size`.
            if values::is_zero_argument_method(target, name) {
                let Some(mut produced) = values::method(target, name, &[])? else {
                    return Ok(None);
                };
                return walk(&mut produced, rest, rest_args);
            }
            Ok(None)
        }
        Access::Index(_) => {
            let index = row.first().cloned().unwrap_or(Json::Null);
            match target {
                Json::Array(list) => {
                    let at = index
                        .as_u64()
                        .and_then(|at| usize::try_from(at).ok())
                        .unwrap_or(usize::MAX);
                    match list.get_mut(at) {
                        Some(child) => walk(child, rest, rest_args),
                        None => Ok(None),
                    }
                }
                Json::Object(map) => {
                    let key = match &index {
                        Json::String(key) => key.clone(),
                        other => values::rendered(other),
                    };
                    match map.get_mut(&key) {
                        Some(child) => walk(child, rest, rest_args),
                        None => Ok(None),
                    }
                }
                _ => Ok(None),
            }
        }
        Access::Call(name, _) => {
            let Some(mut produced) = values::method(target, name, row)? else {
                return Ok(None);
            };
            walk(&mut produced, rest, rest_args)
        }
    }
}

/// Write `value` where `accesses` points within `target`, creating the maps on the way —
/// which is what `#set($ctx.stash.key = …)` does to a stash that holds nothing yet.
fn place(
    target: &mut Json,
    accesses: &[Access],
    args: &[Vec<Json>],
    value: Json,
) -> Result<(), String> {
    let Some((first, rest)) = accesses.split_first() else {
        *target = value;
        return Ok(());
    };
    let (row, rest_args) = split(args);
    if let Access::Call(name, _) = first {
        return Err(format!("{NOT_ASSIGNABLE}{name}"));
    }
    // An index into a list addresses an element that is already there; everything else
    // addresses a member of a map, which is created where it is missing.
    if matches!(first, Access::Index(_)) && target.is_array() {
        let Json::Array(list) = target else {
            return Err(NOT_ASSIGNABLE_ELEMENT.to_string());
        };
        let at = row
            .first()
            .and_then(Json::as_u64)
            .and_then(|at| usize::try_from(at).ok());
        let Some(child) = at.and_then(|at| list.get_mut(at)) else {
            return Err(NOT_ASSIGNABLE_ELEMENT.to_string());
        };
        return place(child, rest, rest_args, value);
    }
    let key = match first {
        Access::Prop(name) => name.clone(),
        _ => match row.first() {
            Some(Json::String(key)) => key.clone(),
            Some(index) => values::rendered(index),
            None => String::new(),
        },
    };
    if !target.is_object() {
        *target = Json::Object(JsonMap::new());
    }
    let Json::Object(map) = target else {
        return Err(NOT_ASSIGNABLE_ELEMENT.to_string());
    };
    let child = map.entry(key).or_insert(Json::Null);
    place(child, rest, rest_args, value)
}

/// The first row of arguments and the rest, for a walk that is about to take one step.
fn split(args: &[Vec<Json>]) -> (&[Json], &[Vec<Json>]) {
    match args.split_first() {
        Some((row, rest)) => (row.as_slice(), rest),
        None => (&[], &[]),
    }
}

/// Roughly how many BYTES a value holds, and whether it is one this evaluation may hold at
/// all.
///
/// `Err` for a value past [`MAX_VALUE_BYTES`] or nested past [`MAX_VALUE_DEPTH`] — the two
/// memory bounds of an evaluation. The walk stops at the first byte past the ceiling, so
/// weighing a refused value costs no more than weighing an admitted one.
fn weigh(value: &Json, depth: usize) -> Result<usize, String> {
    if depth > MAX_VALUE_DEPTH {
        return Err(format!("{TOO_DEEP_A_VALUE} {MAX_VALUE_DEPTH}."));
    }
    // One scalar's own footprint, so a list of a thousand nulls weighs something.
    let scalar = ::std::mem::size_of::<Json>();
    let weight = match value {
        Json::Null | Json::Bool(_) | Json::Number(_) => scalar,
        Json::String(text) => scalar.saturating_add(text.len()),
        Json::Array(elements) => {
            let mut weight = scalar;
            for element in elements {
                weight = weight.saturating_add(weigh(element, depth.saturating_add(1))?);
                if weight > MAX_VALUE_BYTES {
                    break;
                }
            }
            weight
        }
        Json::Object(members) => {
            let mut weight = scalar;
            for (name, member) in members {
                weight = weight
                    .saturating_add(name.len())
                    .saturating_add(weigh(member, depth.saturating_add(1))?);
                if weight > MAX_VALUE_BYTES {
                    break;
                }
            }
            weight
        }
    };
    if weight > MAX_VALUE_BYTES {
        return Err(format!("{TOO_LARGE_A_VALUE} {MAX_VALUE_BYTES} bytes."));
    }
    Ok(weight)
}

/// The sentences a template fault states.
const UNEXPECTED_DIRECTIVE: &str =
    "This template closes a block it never opened: #end, #else or #elseif with no #if or \
     #foreach.";
const NO_END: &str = "This template opens a block it never closes with #end.";
const NOT_A_DIRECTIVE: &str = "This directive is not written as ( … ).";
const NOT_AN_ASSIGNMENT: &str = "This #set is not written as ( $target = value ).";
const NOT_A_LOOP: &str = "This #foreach is not written as ( $element in value ).";
const NOT_AN_ARGUMENT_LIST: &str = "This call's arguments are not written as ( … ).";
const NOT_AN_EXPRESSION: &str = "This is not an expression.";
const NOT_ASSIGNABLE: &str = "A #set cannot assign to the result of a call: ";
const NOT_ASSIGNABLE_ELEMENT: &str = "A #set cannot assign to that element.";
const NO_UTIL_INDEX: &str = "A $util helper is named, never indexed.";
const NO_SUCH_HELPER: &str = "No such helper: ";
const TOO_LONG_A_TEMPLATE: &str = "This template is longer than this endpoint evaluates:";
const TOO_MANY_STEPS: &str = "This template took more steps than this endpoint evaluates:";
const TOO_LONG: &str = "This template ran for longer than this endpoint evaluates one:";
const TOO_LARGE_A_VALUE: &str = "This template built a value larger than this endpoint holds:";
const TOO_DEEP_A_VALUE: &str =
    "This template built a value nested deeper than this endpoint holds:";
const TOO_DEEP: &str = "This template nests deeper than this endpoint parses:";
const TOO_MUCH_OUTPUT: &str = "This template produced more text than this endpoint renders:";
