// SPDX-License-Identifier: CC0-1.0

//! How APPSYNC_JS `code` is READ by the call that would STORE it: the diagnostics code that
//! does not compile is reported with, and the APPSYNC_JS rule set code that compiles is
//! checked against.
//!
//! `code` is "The `resolver` code that contains the request and response functions"
//! (`Resolver$code`, and `FunctionConfiguration$code` for a function), written for the
//! APPSYNC_JS runtime. AppSync reads that code as the resolver or the function is written,
//! and a definition it could not read — or one that breaks a rule of the runtime's own rule
//! set — is refused rather than stored, with "the detail … contain[ing] a list of code
//! errors" (`BadRequestDetail`): one `CodeError` per diagnostic, carrying the diagnostic as
//! the compiler or the rule worded it, and where in the submitted code it sits.
//!
//! Two passes, in the order a compiler runs them, because a diagnostic of the second pass is
//! about a tree the first pass has to have built:
//!
//! 1. the COMPILE. The code is parsed by a maintained ECMAScript front end (`swc_ecma_parser`),
//!    and each diagnostic it produces is reported the way the compiler a definition is
//!    compiled with states one: `<unit>(<line>,<column>): error TS<code>: <sentence>`, on a
//!    line of its own, where the unit is the one the code is compiled under ([`CODE_UNIT`]),
//!    the position is the one in the submitted code, and the code and the sentence are the
//!    TypeScript diagnostic the fault falls under ([`TypeScriptDiagnostic`]). A definition
//!    that does not compile is refused on those diagnostics alone — the rule set is not
//!    reached, because there is no tree to check;
//! 2. the RULE SET. The parsed module is walked and every construct the APPSYNC_JS rule set
//!    forbids is reported as `<unit>(<line>,<column>): error <rule>: <sentence>`, naming the
//!    rule that forbids it ([`appsync_js_rules`]), with the position AND the length of the
//!    construct it is about.
//!
//! Nothing of the code is EVALUATED here: this is a read, so no host function, no context
//! and no module body of the caller's is run. What runs caller code is the engine embedded
//! in this process (`crate::ops::evaluate_code::appsync_js`), which is also what bounds this
//! read: it happens on a thread of that sandbox's own, under the same slot count and the same
//! caller-wait ceiling an evaluation is held to, and never on the executor serving the
//! request. How deeply a read goes into the code is how deeply the code nests, and what the
//! code may be is bounded by the model — `code` targets `Code`, whose `@length` is 1..32768,
//! and a level of nesting takes at least one character — so the thread's stack is sized for
//! the deepest structure a definition can describe ([`READ_THREAD_STACK_BYTES`]) and the walk
//! over what compiled takes its own depth onto the heap ([`appsync_js_rules`]). Every
//! definition the model admits is therefore read to an answer: stored, or refused with the
//! diagnostics it earned.
//!
//! The rule is over the MEMBER rather than over one operation, so every operation that
//! stores `code` (`CreateFunction`, `UpdateFunction`, `CreateResolver`, `UpdateResolver`)
//! reads it the same way through [`super::code_definition`]; a request that carries no
//! `code` defines nothing to read.

use ::std::ops::Range;

use ::swc_common::{BytePos, Spanned};
use ::swc_ecma_ast::{EsVersion, Module};
use ::swc_ecma_parser::error::{Error, SyntaxError};
use ::swc_ecma_parser::lexer::Lexer;
use ::swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};

use crate::ops::evaluate_code::appsync_js::{
    in_sandbox, CodeFault, Entered, CODE_UNIT, PARSE_ERROR,
};

/// The APPSYNC_JS rule set the parsed module is checked against.
mod appsync_js_rules;
pub use appsync_js_rules::{RULE_SET, RULE_SET_PREFIX};

/// The `errorType` a fault the RULE SET found is reported under: "Examples include, but
/// aren't limited to: `LINT_ERROR`, `PARSER_ERROR`" (`CodeError$errorType`), and recorded
/// behaviour reports a rule the code broke under this one.
pub const LINT_ERROR: &str = "LINT_ERROR";

/// What reading a definition's `code` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnosed {
    /// The code compiled and broke no rule: the definition may be stored.
    Read,
    /// The faults the read found, in the order they sit in the code: the refusal's detail.
    /// Either every fault is a compile diagnostic or every fault is a broken rule, because
    /// the rule set is only reached for code that compiled.
    Faults(Vec<CodeFault>),
    /// The read did not happen or did not come back, so nothing is known about the code: the
    /// text says why. A definition nothing could read is not stored — the call answers the
    /// server fault its model declares instead, because storing it would be
    /// indistinguishable from storing one that compiled.
    Unknown(String),
}

/// Read `code` the way the call that would store it reads it, on a thread of the sandbox's
/// own and under its bounds.
pub async fn code_diagnostics(code: String) -> Diagnosed {
    match in_sandbox(READ_THREAD_STACK_BYTES, move || read(&code)).await {
        Entered::Answered(diagnosed) => diagnosed,
        Entered::GaveUp(reason) => Diagnosed::Unknown(reason),
    }
}

/// The stack the thread a read happens on is given.
///
/// A front end reads a nested construct by reading what is inside it, so how far into itself
/// a read goes is how deeply the code nests — and what the code may be is bounded by the
/// model: `code` targets `Code`, whose `@length` is 1..32768, and the shortest text that adds
/// a level of nesting is one character, so no definition can describe more than that many
/// levels. This is sized for that worst case, with room per level well above what a front
/// end's own frames take, so EVERY definition the model admits is read to an answer rather
/// than being turned away or taking the thread down.
///
/// The front end grows its own stack onto the heap where it can, and the walk over what
/// compiled does the same at every level it recurses at ([`appsync_js_rules`]), but a front
/// end does not do that at every recursion it has. Measured on this front end: a definition
/// whose one expression chains 12 000 operators — well inside the modelled length — exhausts a
/// 32 MiB stack, so a level costs it more than 2 KiB, and the 32 768 levels the model admits
/// come to roughly 90 MiB. This is that with several times' headroom, and the deepest
/// definitions the model can describe are read on it in
/// `tests/all/code_definition_refusals.rs` and `tests/all/appsync_js_code_rules.rs`.
///
/// It is reserved address space rather than memory spent: only the pages a read really
/// touches are taken — a written handler touches a few — and the thread is gone when the read
/// is. How many such threads exist at once is the sandbox's own slot bound. A host that
/// cannot give the thread that reservation is one where the read does not happen at all,
/// which the caller already reports as the server fault its model declares.
const READ_THREAD_STACK_BYTES: usize = 512 * 1024 * 1024;

/// Read `code` on THIS thread: compile it, and check what compiled against the rule set.
fn read(code: &str) -> Diagnosed {
    // The bracket the code never closes, which the diagnostic for a read that ran out of
    // input names. Read off the TOKENS in one straight pass, so a bracket inside a string, a
    // comment, a template or a regular expression is not taken for one the code opened.
    let unclosed = unclosed_bracket(code);
    // A misplaced `export` is read PAST rather than reported (see [`misplaced_declaration`]),
    // and each round of the read deals with one of them, so the rounds are bounded by how
    // many a definition can carry at all.
    let mut source = code.to_string();
    for _ in 0..=MAX_MISPLACED_DECLARATIONS {
        match compile(&source, unclosed) {
            Compiled::Misplaced(keyword) => {
                // The keyword is read past by blanking exactly the characters it occupies,
                // which leaves every other position in the code where it was — so a
                // diagnostic of the next round names a position in the code the caller
                // submitted.
                let blanked = " ".repeat(keyword.len());
                source.replace_range(keyword, &blanked);
            }
            Compiled::Faults(faults) => return Diagnosed::Faults(faults),
            Compiled::Unreadable => {
                return Diagnosed::Unknown("the code could not be read".to_string())
            }
            Compiled::Module(module) => {
                let faults = appsync_js_rules::rule_faults(code, &module);
                return if faults.is_empty() {
                    Diagnosed::Read
                } else {
                    Diagnosed::Faults(faults)
                };
            }
        }
    }
    Diagnosed::Unknown("the code could not be read".to_string())
}

/// The token that would close the bracket `code` opens last and never closes, or `None` for
/// code that closes every bracket it opens.
///
/// What a read that ran out of input asks for, where the front end did not name the token
/// itself: the brackets are counted over the TOKENS the front end read rather than over the
/// text, so one inside a string, a template or a comment is not counted as one the code
/// opened. (Telling a regular expression from a division takes the parse this pass does not
/// run, so a bracket inside one may be counted; all that rides on it is which bracket a read
/// that ran out of input names.) Lexing reads the text straight through, so this pass goes no
/// deeper for a deeply nested definition than for a flat one.
fn unclosed_bracket(code: &str) -> Option<&'static str> {
    let mut open: Vec<&'static str> = Vec::new();
    for token in lexer(code) {
        // The token's own text is what says which token it is, so this reading does not
        // depend on how the front end spells one.
        match code
            .get(offset(token.span.lo)..offset(token.span.hi))
            .unwrap_or_default()
        {
            "{" => open.push("}"),
            "(" => open.push(")"),
            "[" => open.push("]"),
            "}" | ")" | "]" => {
                open.pop();
            }
            _ => {}
        }
    }
    open.pop()
}

/// The front end's lexer over `code`, which both the bracket pass and the compile read it
/// with.
fn lexer(code: &str) -> Lexer<'_> {
    let end = BytePos(CODE_START.0.saturating_add(u32::try_from(code.len()).unwrap_or(u32::MAX)));
    Lexer::new(
        // The code is JavaScript for the APPSYNC_JS runtime, read at the newest language
        // version the front end knows, so a definition is not refused for a construct that
        // is only newer than some older target.
        Syntax::Es(EsSyntax::default()),
        EsVersion::latest(),
        StringInput::new(code, CODE_START, end),
        None,
    )
}

/// How many misplaced declarations one read may pass over, each costing one round of
/// [`read`]: well above the number a written definition carries in practice, and finite, so
/// one read cannot spend the thread it is on.
const MAX_MISPLACED_DECLARATIONS: usize = 64;

/// What ONE compile of the code came to.
enum Compiled {
    /// The front end read the code to the end: the module it built.
    Module(Box<Module>),
    /// The diagnostics it produced, stated as the compiler states one. Never empty: a
    /// refusal carries one `CodeError` per diagnostic, so a read with nothing to state is
    /// [`Compiled::Unreadable`] instead.
    Faults(Vec<CodeFault>),
    /// The front end did not build a module and stated nothing this module could report, so
    /// nothing is known about the code — which is not the same as a clean read.
    Unreadable,
    /// The code carries an `import` or an `export` where the front end does not admit one,
    /// which is not a fault of the kind this compile reports: the characters the keyword
    /// occupies, for the caller to read past (see [`misplaced_declaration`]).
    Misplaced(Range<usize>),
}

/// Where the code's first character sits, as the front end counts positions. Everything this
/// module reports is counted from the code ITSELF, so each position a diagnostic names is
/// taken back to an offset into the submitted text by subtracting this.
const CODE_START: BytePos = BytePos(1);

/// Parse `code` once and report what that came to.
fn compile(code: &str, unclosed: Option<&'static str>) -> Compiled {
    let mut parser = Parser::new_from(lexer(code));
    let parsed = parser.parse_module();
    let mut errors = parser.take_errors();
    let module = match parsed {
        Ok(module) => Some(module),
        Err(error) => {
            errors.push(error);
            None
        }
    };
    if let Some(keyword) = errors
        .iter()
        .find_map(|error| misplaced_declaration(code, error))
    {
        return Compiled::Misplaced(keyword);
    }
    let faults = compile_faults(code, &errors, unclosed);
    match module {
        // A module the front end built WITH diagnostics still did not compile: the
        // diagnostics are the answer.
        Some(module) if faults.is_empty() => Compiled::Module(Box::new(module)),
        _ if !faults.is_empty() => Compiled::Faults(faults),
        _ => Compiled::Unreadable,
    }
}

/// The characters a misplaced `import` or `export` keyword occupies, for a diagnostic about
/// one, or `None` for every other diagnostic.
///
/// Where such a declaration may stand is a GRAMMAR rule rather than a reading rule: the
/// compiler a definition is compiled with reads an export declaration in any statement
/// position and goes on reading the code, and reports its placement separately from the
/// diagnostics a definition is refused on. This front end stops at one instead, so the
/// keyword is read past and the rest of the code is read ([`read`]) — which is what lets a
/// definition whose braces never close be reported as the missing brace it is, wherever its
/// later declarations sit.
fn misplaced_declaration(code: &str, error: &Error) -> Option<Range<usize>> {
    if !matches!(error.kind(), SyntaxError::ImportExportInScript) {
        return None;
    }
    let span = Spanned::span(error);
    let keyword = offset(span.lo)..offset(span.hi);
    // Only the characters the code really holds can be blanked, and only a keyword's worth of
    // them: anything else is left to be reported as the diagnostic it is.
    code.get(keyword.clone())
        .filter(|text| text.chars().all(|character| character.is_ascii_alphabetic()))
        .map(|_| keyword)
}

/// The diagnostics `errors` state, as the compiler states one, in the order they sit in the
/// code and with the duplicates a recovering parse produces dropped — the compiler reports
/// one diagnostic once, however many of its recoveries ran into the same fault.
fn compile_faults(code: &str, errors: &[Error], unclosed: Option<&'static str>) -> Vec<CodeFault> {
    let mut faults: Vec<CodeFault> = Vec::new();
    for error in errors {
        let span = Spanned::span(error);
        let (line, column) = position(code, offset(span.lo));
        let diagnostic = TypeScriptDiagnostic::of(error, unclosed);
        let fault = CodeFault {
            error_type: PARSE_ERROR,
            value: format!(
                "{CODE_UNIT}({line},{column}): error TS{}: {}\n",
                diagnostic.code, diagnostic.sentence
            ),
            line,
            column,
            span: length(code, offset(span.lo)..offset(span.hi)),
        };
        if !faults.contains(&fault) {
            faults.push(fault);
        }
    }
    faults.sort_by_key(|fault| (fault.line, fault.column));
    faults
}

/// One TypeScript diagnostic: the number a compiler names it by and the sentence it states.
///
/// The numbers and the sentences are TypeScript's own, from the compiler's diagnostic
/// message catalogue (`diagnosticMessages.json`, the source of every `TS<number>` a compiler
/// prints): `TS1002` "Unterminated string literal.", `TS1005` "'{0}' expected.", `TS1109`
/// "Expression expected.", `TS1127` "Invalid character." and `TS1128` "Declaration or
/// statement expected." — the classes a written definition's faults fall under.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TypeScriptDiagnostic {
    /// The diagnostic's number, as the `TS` prefix is followed by.
    code: u32,
    /// The sentence it states, with the token or the character it is about already in it.
    sentence: String,
}

impl TypeScriptDiagnostic {
    /// The diagnostic `error` falls under.
    fn of(error: &Error, unclosed: Option<&'static str>) -> Self {
        match error.kind() {
            // A token the parse required and did not find: the compiler names the token it
            // expected.
            SyntaxError::Expected(token, _got) => Self::expected(token),
            // The parse ran out of a statement before its semicolon.
            SyntaxError::ExpectedSemiForExprStmt { .. } => Self::expected(";"),
            // A token the parse did not expect. Where the parse ran out of INPUT, the token
            // the compiler asks for is the one that would close the construct still open at
            // the end of the code; anywhere else, the code is not a declaration or a
            // statement where one belongs.
            SyntaxError::Unexpected { got, .. } if got == END_OF_INPUT => {
                Self::expected(unclosed.unwrap_or("}"))
            }
            SyntaxError::UnterminatedStrLit => Self {
                code: 1002,
                sentence: "Unterminated string literal.".to_string(),
            },
            SyntaxError::TS1109 => Self {
                code: 1109,
                sentence: "Expression expected.".to_string(),
            },
            SyntaxError::UnexpectedChar { .. } | SyntaxError::InvalidIdentChar => Self {
                code: 1127,
                sentence: "Invalid character.".to_string(),
            },
            _ => Self {
                code: 1128,
                sentence: "Declaration or statement expected.".to_string(),
            },
        }
    }

    /// The diagnostic for a `token` the compile required: TypeScript's `TS1005`, whose
    /// sentence names that token.
    fn expected(token: &str) -> Self {
        Self {
            code: 1005,
            sentence: format!("'{token}' expected."),
        }
    }
}

/// How the front end names the end of the code in a diagnostic about the token it found.
const END_OF_INPUT: &str = "<eof>";

/// Where a position the front end states sits in the submitted code, as an offset into it.
fn offset(position: BytePos) -> usize {
    usize::try_from(position.0.saturating_sub(CODE_START.0)).unwrap_or(usize::MAX)
}

/// The line and the column `offset` sits at in `code`, both counted from one, the way a
/// compiler's diagnostic and the modelled `CodeErrorLocation` count them.
///
/// The position just past the code's last character is counted the same way as any other, so
/// a diagnostic about code that ran out mid-construct names the position the next character
/// would take — the line after the last for a text ending in a line terminator.
pub(super) fn position(code: &str, offset: usize) -> (i32, i32) {
    // The offset is taken back to the nearest position the text really has, so a diagnostic
    // is reported where the fault sits rather than at the end of the code.
    let mut at = offset.min(code.len());
    while at > 0 && !code.is_char_boundary(at) {
        at -= 1;
    }
    let read = code.get(..at).unwrap_or_default();
    let line = read.matches('\n').count().saturating_add(1);
    let column = read
        .rsplit_once('\n')
        .map_or(read, |(_, last)| last)
        .chars()
        .count()
        .saturating_add(1);
    (
        i32::try_from(line).unwrap_or(i32::MAX),
        i32::try_from(column).unwrap_or(i32::MAX),
    )
}

/// How long the stretch of `code` a diagnostic is about is, as `CodeErrorLocation$span` —
/// "The span/length of the error" — counts it: in the units JavaScript itself measures text
/// in, so a diagnostic about one character is one long whatever that character takes in the
/// request's bytes. A diagnostic about a POSITION rather than a stretch is zero long.
pub(super) fn length(code: &str, stretch: Range<usize>) -> i32 {
    let text = code.get(stretch).unwrap_or_default();
    i32::try_from(text.encode_utf16().count()).unwrap_or(i32::MAX)
}

/// One fault of the RULE SET, as the rule that found it states it.
pub(super) fn rule_fault(code: &str, rule: &str, sentence: &str, stretch: Range<usize>) -> CodeFault {
    let (line, column) = position(code, stretch.start);
    CodeFault {
        error_type: LINT_ERROR,
        value: format!("{CODE_UNIT}({line},{column}): error {rule}: {sentence}"),
        line,
        column,
        span: length(code, stretch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the compile-diagnostic table: what the row is about, the code, the whole
    /// diagnostic the compiler states first, and the line, the column and the length that
    /// diagnostic's location carries.
    type CompileCase<'a> = (&'a str, &'a str, &'a str, i32, i32, i32);

    /// The diagnostic a compile states for code it could not read, over the ways a
    /// definition fails to compile: the position is the one in the submitted code and the
    /// sentence is the compiler's own, naming the TypeScript diagnostic the fault falls
    /// under.
    #[test]
    fn code_that_does_not_compile_is_reported_as_the_compiler_states_it() {
        let cases: &[CompileCase<'_>] = &[
            (
                "a function body that never closes",
                "export function request(ctx) {\n  return {};\n",
                "code.js(3,1): error TS1005: '}' expected.\n",
                3,
                1,
                0,
            ),
            (
                "an object literal that never closes, with a later export inside it",
                "export function request(ctx) {\n  return {\n    operation: 'GetItem',\n    \
                 key: { id: ctx.args.id },\n  };\nexport function response(ctx) { return \
                 ctx.result; }\n",
                "code.js(7,1): error TS1005: '}' expected.\n",
                7,
                1,
                0,
            ),
            (
                "an object literal that runs out of input",
                "export function request(ctx) {\n  return {\n    payload: ctx.args,\n",
                "code.js(4,1): error TS1005: '}' expected.\n",
                4,
                1,
                0,
            ),
            (
                "a parenthesis that never closes",
                "export function request(ctx) {\n  return ((((\n",
                "code.js(3,1): error TS1109: Expression expected.\n",
                3,
                1,
                0,
            ),
            (
                "an unterminated string literal",
                "export function request(ctx) { return { payload: 'unterminated }; }",
                "code.js(1,50): error TS1002: Unterminated string literal.\n",
                1,
                50,
                18,
            ),
            (
                "a statement that is not JavaScript",
                "this is not javascript at all",
                "code.js(1,6): error TS1005: ';' expected.\n",
                1,
                6,
                2,
            ),
            (
                "a template literal whose interpolation never closes",
                "export function request(ctx) {\n  return `id: ${ctx.args.id\n",
                "code.js(3,1): error TS1005: '}' expected.\n",
                3,
                1,
                0,
            ),
        ];

        for (case, code, value, line, column, span) in cases {
            let faults = match read(code) {
                Diagnosed::Faults(faults) => faults,
                other => panic!("case {case:?}: expected compile diagnostics, got {other:?}"),
            };
            let first = faults
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: expected a diagnostic, got none"));
            assert_eq!(
                (
                    first.error_type,
                    first.value.as_str(),
                    (first.line, first.column),
                    first.span
                ),
                (PARSE_ERROR, *value, (*line, *column), *span),
                "case {case:?}: expected the compiler's own diagnostic, got {first:?}"
            );
        }
    }

    /// The rule set over the code recorded as breaking two of its rules at once: one
    /// diagnostic per broken rule, in the order the constructs sit in the code, each naming
    /// the rule, stating the position of the construct and carrying its length.
    #[test]
    fn code_that_breaks_the_rule_set_is_reported_rule_by_rule() {
        let code = "export function request(ctx) {\n  for (let i = 0; i < 3; i++) {}\n  return {};\n}\n";
        let faults = match read(code) {
            Diagnosed::Faults(faults) => faults,
            other => panic!("case a for loop using ++: expected rule diagnostics, got {other:?}"),
        };
        let stated: Vec<(&str, &str, i32, i32, i32)> = faults
            .iter()
            .map(|fault| {
                (
                    fault.error_type,
                    fault.value.as_str(),
                    fault.line,
                    fault.column,
                    fault.span,
                )
            })
            .collect();
        assert_eq!(
            stated,
            vec![
                (
                    LINT_ERROR,
                    "code.js(2,3): error @aws-appsync/no-for: For statements are not supported.",
                    2,
                    3,
                    30,
                ),
                (
                    LINT_ERROR,
                    "code.js(2,26): error @aws-appsync/no-disallowed-unary-operators: The \
                     operator ++ is not supported",
                    2,
                    26,
                    3,
                ),
            ],
            "case a for loop using ++: expected one diagnostic per broken rule, got {faults:?}"
        );
    }

    /// Every rule of the set is checked, and each is reported under its own name: one row
    /// per rule, each a construct that rule is about, plus the rows that show a construct is
    /// read from the TREE rather than from the text (a rule's keyword as a property name, in
    /// a string or in a comment breaks nothing).
    #[test]
    fn every_rule_of_the_set_is_checked_over_the_tree() {
        // (case, code, the rules it is expected to break)
        let cases: &[(&str, &str, &[&str])] = &[
            (
                "a for-of loop",
                "export function request(ctx) { for (const i of ctx.args.ids) { } return {}; }",
                &["@aws-appsync/no-for"],
            ),
            (
                "a for-in loop",
                "export function request(ctx) { for (const k in ctx.args) { } return {}; }",
                &["@aws-appsync/no-for"],
            ),
            (
                "a while loop",
                "export function request(ctx) { while (true) { } return {}; }",
                &["@aws-appsync/no-while"],
            ),
            (
                "a do-while loop",
                "export function request(ctx) { do { } while (false); return {}; }",
                &["@aws-appsync/no-while"],
            ),
            (
                "a continue inside a loop",
                "export function request(ctx) { while (true) { continue; } return {}; }",
                &["@aws-appsync/no-while", "@aws-appsync/no-continue"],
            ),
            (
                "a labeled statement",
                "export function request(ctx) { top: { break top; } return {}; }",
                &["@aws-appsync/no-labels"],
            ),
            (
                "a try statement",
                "export function request(ctx) { try { return {}; } catch (e) { return {}; } }",
                &["@aws-appsync/no-try"],
            ),
            (
                "a class declaration",
                "class Key { }\nexport function request(ctx) { return {}; }\n",
                &["@aws-appsync/no-classes"],
            ),
            (
                "an async handler awaiting a value",
                "export async function request(ctx) { return await ctx.args; }",
                &["@aws-appsync/no-async", "@aws-appsync/no-await"],
            ),
            (
                "an async arrow function",
                "export function request(ctx) { const f = async () => 1; return {}; }",
                &["@aws-appsync/no-async"],
            ),
            (
                "a generator function yielding",
                "export function request(ctx) { function* g() { yield 1; } return {}; }",
                &["@aws-appsync/no-generators", "@aws-appsync/no-yield"],
            ),
            (
                "a regular expression literal",
                "export function request(ctx) { return { ok: /^a+$/.test(ctx.args.s) }; }",
                &["@aws-appsync/no-regex"],
            ),
            (
                "the this keyword",
                "export function request(ctx) { return this; }",
                &["@aws-appsync/no-this"],
            ),
            (
                "a promise",
                "export function request(ctx) { return Promise.resolve(1); }",
                &["@aws-appsync/no-promise"],
            ),
            (
                "a decrement operator",
                "export function request(ctx) { let n = ctx.args.n; n--; return { n }; }",
                &["@aws-appsync/no-disallowed-unary-operators"],
            ),
            (
                "a prefix increment operator",
                "export function request(ctx) { let n = ctx.args.n; ++n; return { n }; }",
                &["@aws-appsync/no-disallowed-unary-operators"],
            ),
            // The constructs are read from the tree, so a rule's keyword that is not one is
            // not a construct: a property named like a statement, a keyword inside a string
            // and one inside a comment all break nothing.
            (
                "the keywords as property and member names",
                "export function request(ctx) { const o = { for: 1, this: 2, class: 3, \
                 await: 4 }; return { v: o.for + o.this + ctx.args.try }; }",
                &[],
            ),
            (
                "the keywords inside a string and a comment",
                "export function request(ctx) {\n  // for (;;) { this ++ }\n  return { s: \
                 'for while try this ++ /a/' };\n}\n",
                &[],
            ),
        ];

        for (case, code, broken) in cases {
            let diagnosed = read(code);
            if broken.is_empty() {
                assert_eq!(
                    diagnosed,
                    Diagnosed::Read,
                    "case {case:?}: expected the code to break no rule, got {diagnosed:?}"
                );
                continue;
            }
            let faults = match diagnosed {
                Diagnosed::Faults(faults) => faults,
                other => panic!("case {case:?}: expected rule diagnostics, got {other:?}"),
            };
            let mut named: Vec<String> = faults
                .iter()
                .map(|fault| {
                    assert_eq!(
                        fault.error_type, LINT_ERROR,
                        "case {case:?}: expected a rule diagnostic, got {fault:?}"
                    );
                    let stated = format!("code.js({},{}): error ", fault.line, fault.column);
                    assert!(
                        fault.value.starts_with(&stated) && fault.span > 0,
                        "case {case:?}: expected a diagnostic stating {stated:?} over a \
                         construct of its own, got {fault:?}"
                    );
                    fault
                        .value
                        .split_whitespace()
                        .nth(2)
                        .unwrap_or_default()
                        .trim_end_matches(':')
                        .to_string()
                })
                .collect();
            named.sort();
            let mut expected: Vec<String> = broken.iter().map(|rule| (*rule).to_string()).collect();
            expected.sort();
            assert_eq!(
                named, expected,
                "case {case:?}: expected exactly the rules it breaks, got the diagnostics above"
            );
        }
    }

    /// Every rule the set declares is named under the set's own prefix.
    #[test]
    fn the_set_names_every_rule_under_its_prefix() {
        for rule in RULE_SET {
            assert!(
                rule.starts_with(&format!("{RULE_SET_PREFIX}/")),
                "case the rule {rule:?}: expected a name under {RULE_SET_PREFIX:?}"
            );
        }
    }

    /// The bracket a read that runs out of input asks for is the one the code opens last and
    /// never closes, read off the tokens — so a bracket inside a string, a comment, a
    /// template or a regular expression is not taken for one the code opened.
    #[test]
    fn the_bracket_a_read_asks_for_is_the_one_the_code_never_closed() {
        let cases: &[(&str, &str, Option<&str>)] = &[
            ("a block", "function f() {", Some("}")),
            ("a call", "f(1, 2", Some(")")),
            ("an array literal", "const a = [1", Some("]")),
            (
                // A template is one token of the front end's own, interpolations included, so
                // the code opened no bracket this pass can see: the brace such a read asks
                // for is the one the front end names itself (the compile table's own template
                // row covers it).
                "a template interpolation",
                "const s = `id: ${x",
                None,
            ),
            (
                "the innermost of several",
                "function f() { g([1, (2",
                Some(")"),
            ),
            (
                "a bracket inside a string and a comment",
                "const s = '([{'; // ([{\n",
                None,
            ),
            (
                "a bracket inside a template",
                "const s = `([{`;\n",
                None,
            ),
            ("code that closes every bracket", "f([1, 2]);\n", None),
        ];
        for (case, code, expected) in cases {
            let unclosed = unclosed_bracket(code);
            assert_eq!(
                unclosed, *expected,
                "case {case:?}: expected the bracket the code never closed, got {unclosed:?}"
            );
        }
    }

    /// Code the compiler reads to the end and that breaks no rule is read, in the forms a
    /// real APPSYNC_JS definition takes.
    #[test]
    fn code_that_compiles_and_breaks_no_rule_is_read() {
        let cases: &[(&str, &str)] = &[
            ("the smallest handler", "export function request(ctx) { return {}; }"),
            (
                "both handlers",
                "export function request(ctx) { return { payload: ctx.args }; }\n\
                 export function response(ctx) { return ctx.result; }\n",
            ),
            (
                "a handler importing the util helpers",
                "import { util } from '@aws-appsync/utils';\n\
                 export function request(ctx) { return { id: util.autoId() }; }\n",
            ),
            (
                "a handler whose literals carry non-ASCII text",
                "export function request(ctx) {\n  // héllo 世界 🎉\n  return { greeting: 'Ω≈ç√' };\n}\n",
            ),
            (
                "a handler mapping over a list",
                "export function response(ctx) { return ctx.result.items.map((i) => i.id); }\n",
            ),
        ];
        for (case, code) in cases {
            let read = read(code);
            assert_eq!(
                read,
                Diagnosed::Read,
                "case {case:?}: expected the code to be read, got {read:?}"
            );
        }
    }
}
