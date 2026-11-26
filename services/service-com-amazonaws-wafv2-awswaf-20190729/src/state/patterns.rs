// SPDX-License-Identifier: CC0-1.0

//! The caller-supplied regular expressions a WAFv2 save carries: compiling one into
//! something that matches, finding the one a save must be refused for, and the
//! refusal it draws.
//!
//! WHICH expressions a save may carry is the matching engine's flavour: WAF inspects
//! requests with a finite-automaton matcher, so an expression that asks for
//! backtracking — a zero-width assertion of any of the lookaround forms, a
//! backreference, an atomic group or possessive quantifier, a match-start reset, a
//! recursion or subroutine call, a conditional, a control verb — is one WAF cannot
//! run, and a save carrying it is refused. A construct OUTSIDE that class is
//! accepted, including ones the matcher documents as unsupported: recorded WAF
//! responses accept a capturing group and the single-byte directive and round-trip
//! both verbatim through `GetRegexPatternSet`.
//!
//! So the refused set is the backtracking class, which is very nearly what the engine
//! crate below cannot compile: the accepted constructs it spells differently are
//! rewritten into its own syntax first ([`ENGINE_EQUIVALENT_CONSTRUCTS`]) rather than
//! refused, and the two refused ones it reads as something else are taken off the
//! written form ([`uses_unrunnable_construct`]).
//!
//! Every expression here comes from a request, so [`compile_regex`] is the ONE place
//! that compiles one and it bounds every compile with [`REGEX_SIZE_LIMIT`] (the
//! compiled form) and [`REGEX_DFA_SIZE_LIMIT`] (the match cache); the one extra
//! judgement it makes runs the compiled expression against ONE fixed, empty input.
//! So the work one expression can ask for is bounded by those two constants and by
//! the modelled length of the member that carried it, and the number of expressions
//! one request carries is bounded by that member's own modelled list length and —
//! for a rule tree — by the depth bound [`validate_statement_nesting`] applies
//! before any walk.

use super::*;
use crate::ops::types::Regex;

/// Emulator resource guard, NOT an AWS quota: the largest compiled form this
/// emulator builds for one caller-supplied regular expression.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Emulator resource guard, NOT an AWS quota: the largest cache the matcher may use
/// while matching one value against one caller-supplied regular expression.
const REGEX_DFA_SIZE_LIMIT: usize = 1 << 20;

/// The constructs WAF accepts that the matching engine crate spells differently,
/// each paired with the source that crate reads it as.
///
/// `\C` is PCRE's single-byte directive — "match any one byte" — which over the byte
/// haystack this service inspects is a dot with Unicode matching off, so the
/// expression is rewritten into the crate's own syntax rather than refused for being
/// written in another one. It is OUTSIDE the backtracking class (it consumes a byte
/// and asserts nothing), and the recorded WAF responses accept it at write time and
/// hand it back verbatim.
///
/// Each key is a whole escape sequence, matched against the escapes
/// [`engine_source`] walks, so a literal backslash followed by the same letter
/// (`\\C`) is left alone.
const ENGINE_EQUIVALENT_CONSTRUCTS: &[(&str, &str)] = &[(r"\C", "(?s-u:.)")];

/// `pattern` as the matching engine crate reads it: every construct of
/// [`ENGINE_EQUIVALENT_CONSTRUCTS`] rewritten into that crate's syntax, and
/// everything else byte for byte as the request wrote it.
///
/// Walked escape by escape, so a construct's letter is only rewritten where it IS an
/// escape: in `\\C` the first escape is the literal backslash, which leaves the `C`
/// after it an ordinary character.
fn engine_source(pattern: &str) -> String {
    let mut source = String::with_capacity(pattern.len());
    let mut rest = pattern;
    while let Some(start) = rest.find('\\') {
        source.push_str(&rest[..start]);
        let escape = &rest[start..];
        // The escape sequence is the backslash and the character it escapes; a
        // trailing backslash escapes nothing, and is left for the crate's own parser
        // to refuse.
        let Some(escaped) = escape[1..].chars().next() else {
            source.push_str(escape);
            return source;
        };
        let width = 1 + escaped.len_utf8();
        match ENGINE_EQUIVALENT_CONSTRUCTS
            .iter()
            .find(|(construct, _)| *construct == &escape[..width])
        {
            Some((_, equivalent)) => source.push_str(equivalent),
            None => source.push_str(&escape[..width]),
        }
        rest = &escape[width..];
    }
    source.push_str(rest);
    source
}

/// Whether `source` writes one of the two constructs of the backtracking class that
/// the matcher below does NOT report by failing to compile, because it reads the same
/// text as something else:
///
/// * a POSSESSIVE quantifier — `a++`, `x*+`, `b{2,3}+` — a repetition operator
///   applied DIRECTLY to another repetition. PCRE reads it as "repeat, and never give
///   any of it back", a backtracking control; the matcher reads it as an ordinary
///   nested repetition and compiles it. A repetition applied to a GROUP (`(?:a+)+`)
///   IS the ordinary nested repetition and is not this.
/// * a RECURSION — `(?R)`, the whole pattern called again. The matcher reads `R` as
///   one of its own inline flags, so the text compiles as a flag group; PCRE has no
///   such flag, so a pattern that sets it was written as a recursion.
///
/// Read off the AST of the same parser the matcher parses with, so one expression has
/// one reading; text that parser cannot read at all is reported by the compile in
/// [`compile_regex`], which is where every expression that is no expression, and every
/// other construct of the class, is refused.
fn uses_unrunnable_construct(source: &str) -> bool {
    use ::regex_syntax::ast::{Ast, Flag, Flags, FlagsItemKind, GroupKind};

    /// Whether `flags` sets the flag the matcher spells `R`. See
    /// [`uses_unrunnable_construct`].
    fn sets_crlf_flag(flags: &Flags) -> bool {
        flags
            .items
            .iter()
            .any(|item| matches!(item.kind, FlagsItemKind::Flag(Flag::CRLF)))
    }

    struct Scan {
        /// Whether either construct has been seen.
        found: bool,
    }

    impl ::regex_syntax::ast::Visitor for Scan {
        type Output = bool;
        type Err = ();

        fn finish(self) -> Result<bool, ()> {
            Ok(self.found)
        }

        fn visit_pre(&mut self, ast: &Ast) -> Result<(), ()> {
            self.found = self.found
                || match ast {
                    Ast::Repetition(repetition) => {
                        matches!(*repetition.ast, Ast::Repetition(_))
                    }
                    Ast::Flags(set) => sets_crlf_flag(&set.flags),
                    Ast::Group(group) => match &group.kind {
                        GroupKind::NonCapturing(flags) => sets_crlf_flag(flags),
                        _ => false,
                    },
                    _ => false,
                };
            Ok(())
        }
    }

    let Ok(parsed) = ::regex_syntax::ast::parse::Parser::new().parse(source) else {
        return false;
    };
    ::regex_syntax::ast::visit(&parsed, Scan { found: false }).unwrap_or(false)
}

/// `pattern` compiled into something that MATCHES, or `None` when WAF will not
/// inspect requests with it.
///
/// The ONE place a caller-supplied expression becomes something that matches, so the
/// engine that decides whether a stored pattern matches a request is the engine that
/// decides, when the request that carries it is saved, whether it is an expression
/// WAF can run at all: no expression can be accepted by one and rejected by the
/// other. Compiled with the maintained crate — the matcher WAF runs is a
/// finite-automaton one, and that is the flavour this crate implements — under a
/// compiled-size and a match-cache bound, because the pattern comes from the
/// request, and from the source that crate reads the expression as
/// ([`engine_source`]).
///
/// Two ways text is not something that matches, because the modelled constraints on
/// a `RegexString` bound its length only (`Regex$RegexString` is `@length(1..512)`
/// with `@pattern(".*")`, `smithy_query(query="shape",shape="Regex")`), so every byte
/// sequence of legal length reaches the service and what WAF can do with it is the
/// service's own judgement:
///
/// * the engine cannot parse it — which is the backtracking class this flavour
///   excludes (see the module doc), and text that is no expression at all, with the
///   two constructs of that class the engine reads as something else taken off the
///   written form ([`uses_unrunnable_construct`]); or
/// * its match can consume NOTHING. An expression that matches the EMPTY string
///   matches at the start of every value whatever that value holds, so it states no
///   criterion a request can fail — `.*`, `a*`, `(x)?` and `^$` alike — and WAF
///   refuses it as text that is not in the form this parameter takes, rather than
///   storing an inspection criterion that inspects nothing.
pub fn compile_regex(pattern: &str) -> Option<::regex::bytes::Regex> {
    let source = engine_source(pattern);
    if uses_unrunnable_construct(&source) {
        return None;
    }
    let compiled = ::regex::bytes::RegexBuilder::new(&source)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build()
        .ok()?;
    (!compiled.is_match(b"")).then_some(compiled)
}

/// A `RegexString` of `expressions` this service cannot compile into something that
/// matches, if any (the first such the scan reaches). See [`compile_regex`].
pub fn unparseable_regex(expressions: &[Regex]) -> Option<&str> {
    expressions
        .iter()
        .filter_map(|expression| expression.regex_string.as_deref())
        .find(|pattern| compile_regex(pattern).is_none())
}

/// A regular expression `rules` carries that this service cannot compile into
/// something that matches, if any (the first such the walk reaches): a
/// `RegexMatchStatement$RegexString` at any nesting depth, or one of the
/// `ClientSideAction$ExemptUriRegularExpressions` a managed rule group's
/// configuration carries.
///
/// Every expression a save would store is judged by [`compile_regex`], whichever
/// member carries it: a rule's own expression is as unusable as a pattern set's when
/// it does not parse or its match can consume nothing.
///
/// Walked with an explicit worklist, and only after [`validate_statement_nesting`]
/// has bounded the DEPTH of the same caller-supplied tree: the tree comes off the
/// wire.
pub fn unparseable_rule_regex(rules: &[Rule]) -> Option<&str> {
    let mut pending: Vec<&Statement> = rules.iter().map(|rule| &rule.statement).collect();
    while let Some(statement) = pending.pop() {
        if let Some(regex_match) = statement.regex_match_statement.as_ref() {
            if compile_regex(&regex_match.regex_string).is_none() {
                return Some(&regex_match.regex_string);
            }
        }
        if let Some(managed) = statement.managed_rule_group_statement.as_ref() {
            for config in managed.managed_rule_group_configs.iter().flatten() {
                let exempt = config
                    .aws_managed_rules_anti_d_do_s_rule_set
                    .as_ref()
                    .and_then(|anti_ddos| {
                        anti_ddos
                            .client_side_action_config
                            .challenge
                            .exempt_uri_regular_expressions
                            .as_deref()
                    })
                    .unwrap_or_default();
                if let Some(pattern) = unparseable_regex(exempt) {
                    return Some(pattern);
                }
            }
        }
        pending.extend(nested_statements(statement));
    }
    None
}

/// [`invalid_parameter_exception`] for a regular expression WAF will not inspect
/// requests with ([`compile_regex`]): `Field` the settings the expression was found
/// in, `Parameter` the expression as the request wrote it and `Reason`
/// [`FORMAT_NOT_VALID_REASON`].
pub fn invalid_regex_exception(field: &str, pattern: &str) -> WAFInvalidParameterException {
    invalid_parameter_exception(field, pattern, FORMAT_NOT_VALID_REASON)
}
