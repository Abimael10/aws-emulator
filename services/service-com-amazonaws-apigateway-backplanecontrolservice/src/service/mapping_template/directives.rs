// SPDX-License-Identifier: CC0-1.0

//! The Velocity DIRECTIVES a mapping template is performed for: `#set` and the conditional
//! family (`#if`, `#elseif`, `#else`, `#end`).
//!
//! A mapping template is "a map of Velocity templates" (`Integration$requestTemplates`), and
//! the language has two halves: REFERENCES, which name values of the call, and DIRECTIVES,
//! which are statements about them. Two kinds of statement are decidable from the call alone,
//! and both are what API Gateway's own mapping-template examples are written with:
//!
//! * `#set` — "the `#set` directive is used to set the value of a reference": a BINDING of a
//!   name to a value of this call, which is what lets a template name a sub-document of the
//!   payload (`#set($inputRoot = $input.path('$'))`) and read its members;
//! * `#if` / `#elseif` / `#else` / `#end` — a CHOICE between two pieces of the template,
//!   decided by a condition over those same values, which is how a template renders one
//!   document for a request that carries a member and another for one that does not.
//!
//! The GRAMMAR is not read here: the language is the same one AppSync's resolver templates
//! are written in, so a directive, the expression it takes and the line break that ends its
//! own line are read by the tree's ONE reader of that language
//! ([`::service_common::velocity_template`]). What this module holds is which of its
//! directives THIS plane performs, and what each of them states to the renderer
//! ([`super::Render::perform`]) — evaluating an expression against the call is the renderer's
//! own work, because only the renderer holds the call.
//!
//! The directives that are neither a binding nor a choice (`#foreach`, `#macro`, `#parse`, a
//! comment) stay unperformed, and a template carrying one is refused before rendering.

use ::service_common::velocity_template::{Expr, Parse};

/// One directive of a template, as this plane performs it.
#[derive(Debug)]
pub(super) enum Directive {
    /// `#set($name = expression)`: the name it binds, and the expression bound to it.
    Set { name: String, value: Expr },
    /// `#if(condition)`
    If(Expr),
    /// `#elseif(condition)`
    ElseIf(Expr),
    /// `#else`
    Else,
    /// `#end`
    End,
}

/// What the text at `at` (the index of a `#`) is:
///
/// * `None` — not a directive this plane performs, so the `#` is text (or a directive the
///   caller already refused);
/// * `Some(Err(what))` — it IS one and the language's reader cannot read it, carrying what it
///   stopped at, so the caller refuses the template;
/// * `Some(Ok((directive, next)))` — the directive and the index just past it.
///
/// Both spellings are read, the bare `#if` and the formal `#{if}`, because the shared reader
/// reads the language's own grammar.
pub(super) fn directive_at(
    glyphs: &[char],
    at: usize,
) -> Option<Result<(Directive, usize), String>> {
    let mut parse = Parse::at(glyphs, at);
    let name = parse.directive_name()?;
    match name.as_str() {
        "set" => Some(match parse.assignment() {
            // A `#set` binds a NAME of its own here: assigning to a member of a value is a
            // write into the call, which this plane performs nothing of.
            Ok((target, value)) if target.accesses.is_empty() => Ok((
                Directive::Set {
                    name: target.root,
                    value,
                },
                parse.at,
            )),
            Ok(_) => Err(
                "it binds a member of a reference rather than a name of its own, which this \
                 emulator does not set"
                    .to_string(),
            ),
            Err(why) => Err(why),
        }),
        "if" => Some(
            parse
                .parenthesised()
                .map(|condition| (Directive::If(condition), parse.at)),
        ),
        "elseif" => Some(
            parse
                .parenthesised()
                .map(|condition| (Directive::ElseIf(condition), parse.at)),
        ),
        "else" => Some(Ok((Directive::Else, parse.at))),
        "end" => Some(Ok((Directive::End, parse.at))),
        // A directive this plane does not perform is not read as one: the caller refuses a
        // template carrying it before rendering, and a `#` followed by anything else is text.
        _ => None,
    }
}

/// The index rendering resumes at after a directive that ended at `at`: past the line break
/// that CLOSES the directive's own line, when nothing but space follows it.
///
/// "the newline after a directive is gobbled" — the language's own rule, so it is read by the
/// language's reader. A directive with text after it on the same line keeps that text, which
/// is what the one-line form (`#set(…) {"a": 1}`) renders.
pub(super) fn past_gobbled_newline(glyphs: &[char], at: usize) -> usize {
    let mut parse = Parse::at(glyphs, at);
    parse.skip_trailing_newline();
    parse.at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The directive at the start of `template`, as `(its shape, the index just past it)`.
    fn read(template: &str) -> Option<Result<(String, usize), String>> {
        let glyphs: Vec<char> = template.chars().collect();
        directive_at(&glyphs, 0).map(|read| {
            read.map(|(directive, next)| {
                let shape = match directive {
                    Directive::Set { name, .. } => format!("set {name}"),
                    Directive::If(_) => "if".to_string(),
                    Directive::ElseIf(_) => "elseif".to_string(),
                    Directive::Else => "else".to_string(),
                    Directive::End => "end".to_string(),
                };
                (shape, next)
            })
        })
    }

    /// RULE: each directive this plane performs is read as what it states, in both spellings
    /// of a directive and whatever space the developer wrote around it.
    #[test]
    fn each_performed_directive_is_read_as_what_it_states() {
        /// (case, the template, the shape it reads as, the index rendering resumes at)
        type Row<'a> = (&'a str, &'a str, &'a str, usize);
        let rows: &[Row<'_>] = &[
            (
                "the form every mapping-template example opens with",
                "#set($inputRoot = $input.path('$'))",
                "set inputRoot",
                35,
            ),
            (
                "no space around the assignment",
                "#set($i=$input.path('$.Item'))",
                "set i",
                30,
            ),
            (
                "the formal spelling of the directive",
                "#{set}($i = $input.body)",
                "set i",
                24,
            ),
            ("a single-quoted literal", "#set($a = 'text')", "set a", 17),
            ("a double-quoted literal", "#set($a = \"text\")", "set a", 17),
            ("a number", "#set($a = 42)", "set a", 13),
            ("a negative decimal", "#set($a = -1.5)", "set a", 15),
            ("a boolean", "#set($a = true)", "set a", 15),
            (
                "a member of another binding",
                "#set($n = $i.desired.N)",
                "set n",
                23,
            ),
            ("space before the arguments", "#set ($a = 1)", "set a", 13),
            (
                "the comparison the recorded fixture is written with",
                "#if($d < 0)",
                "if",
                11,
            ),
            ("a bare reference", "#if($d)", "if", 7),
            ("a negation", "#if(!$d)", "if", 8),
            ("an equality against a literal", "#if($d == 'x')", "if", 14),
            ("a conjunction", "#if($a && $b)", "if", 13),
            ("a disjunction", "#if($a || $b)", "if", 13),
            ("a parenthesised condition", "#if(($a || $b) && $c)", "if", 21),
            ("space inside the condition", "#if( $d != 1 )", "if", 14),
            ("an elseif", "#elseif($d >= 2)", "elseif", 16),
            ("else", "#else", "else", 5),
            ("end", "#end", "end", 4),
            ("the formal spelling of end", "#{end}", "end", 6),
        ];
        for (case, template, shape, next) in rows {
            let actual = read(template);
            assert_eq!(
                actual
                    .as_ref()
                    .map(|read| read.as_ref().map(|read| (read.0.as_str(), read.1))),
                Some(Ok((*shape, *next))),
                "case {case:?}: expected {shape:?} ending at {next}, got {actual:?}"
            );
        }
    }

    /// RULE: text that is not a directive this plane performs is not read as one — a `#` of
    /// the body, and the directives that stay statements.
    #[test]
    fn only_a_performed_directive_is_read_as_one() {
        let rows: &[(&str, &str)] = &[
            ("a colour in the body", "#fff0aa"),
            ("a hash of its own", "# set($a = 1)"),
            ("a loop", "#foreach($a in $b)x#end"),
            ("a macro", "#macro(m)x#end"),
            ("a name that only starts with set", "#settle($a = 1)"),
            ("a name that only starts with if", "#ifconfig"),
        ];
        for (case, template) in rows {
            let actual = read(template);
            assert!(
                actual.is_none(),
                "case {case:?}: expected no performed directive to be read, got {actual:?}"
            );
        }
    }

    /// RULE: a directive the language's reader cannot read leaves the template unrendered
    /// rather than being read in part.
    #[test]
    fn a_directive_that_cannot_be_read_is_reported() {
        let rows: &[(&str, &str)] = &[
            ("a member assignment", "#set($a.b = 1)"),
            ("no closing parenthesis", "#set($a = 1"),
            ("no value at all", "#set($a)"),
            ("no arguments", "#set"),
            ("a condition that is no expression", "#if(,)"),
            ("an unclosed condition", "#if($a < 1"),
            ("a comparison with nothing to compare", "#if($a <)"),
            ("a condition of its own", "#if"),
        ];
        for (case, template) in rows {
            let actual = read(template);
            assert!(
                matches!(actual, Some(Err(_))),
                "case {case:?}: expected the directive to be reported unread, got {actual:?}"
            );
        }
    }

    /// RULE: the line break that closes a directive's own line renders nothing, and text on
    /// the same line as the directive renders as it stands.
    #[test]
    fn a_directives_own_line_renders_nothing() {
        /// (case, the text after the directive, the index rendering resumes at)
        type Row<'a> = (&'a str, &'a str, usize);
        let rows: &[Row<'_>] = &[
            ("a line break of its own", "\n{\"a\": 1}", 1),
            ("spaces and then a line break", "   \n{", 4),
            ("a carriage return and a line break", "\r\n{", 2),
            ("text on the same line", " {\"a\": 1}", 0),
            ("nothing at all", "", 0),
        ];
        for (case, rest, expected) in rows {
            let glyphs: Vec<char> = rest.chars().collect();
            let actual = past_gobbled_newline(&glyphs, 0);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected rendering to resume at {expected}, got {actual}"
            );
        }
    }
}
