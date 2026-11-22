// SPDX-License-Identifier: CC0-1.0

//! The modelled `@pattern`s the framework's boundary DECLARES but cannot APPLY, applied.
//!
//! # The rule
//!
//! A member's `@pattern` is a rule about the REQUEST: a value the expression does not admit
//! is refused before any state a service holds is looked at, in that service's constraint
//! envelope naming the member and quoting the expression. That holds for every expression a
//! model declares, including one no character grammar can state on its own.
//!
//! The framework's constraint walk compiles a `@pattern` with the `regex` crate and treats
//! one it cannot compile as SATISFIED (`pattern_matches` in
//! `vendor/smithy-schema-server/src/constraints.rs`, read-only from here). That crate has no
//! look-around, so a member whose expression carries one is admitted whatever it holds: the
//! requirement the model states for it reaches no request, and a value AWS refuses as
//! invalid input instead travels into a handler, where it is answered from a state lookup —
//! a different error class, a different status, and a diagnostic naming the wrong member.
//!
//! This module is that gap closed, and THE HOME for it: no member and no expression is named
//! here, because which members carry one is read off the operation's own input schema, and
//! the CHECK is the model's own expression text, evaluated by a look-around-capable engine
//! (`fancy-regex`). Two bounds keep that evaluation finite, and both are properties of the
//! inputs rather than of the engine: the expressions are MODEL CONSTANTS, never built from a
//! request, and the backtracking engine is given an explicit step budget
//! ([`PATTERN_BACKTRACK_LIMIT`]). An expression NEITHER engine compiles, and a match that
//! exhausts that budget, are both left SATISFIED — the framework's own treatment, so no
//! value is refused because of an expression nothing can evaluate.
//!
//! Two service crates still carry their own copy of this rule, written before there was
//! anywhere shared to put it
//! (`grep -rln 'fn unapplied_violations' services/*/src` lists them: today
//! `eventbridgev2` and `iotsitewise`). Those copies are to be MIGRATED onto this module;
//! they are left in place only because the change that moved the rule here does not own
//! those services' issues, and each is covered by its own suite. Nothing new should inline
//! the rule again — call this module.
//!
//! # What it reaches
//!
//! The positions of an operation's input the validating walk reports a violation at and the
//! input SCHEMA can name: a top-level member's own value, and the elements of a collection
//! member, whose element shape's constraints that walk reads off the collection member
//! itself. A member of a NESTED structure is not reached, because a member schema carries
//! its member's name and type rather than its target shape's members, and a map KEY is not
//! reached because the walk captures it as a path segment rather than as a leaf of its own —
//! a gap of that walk rather than of the engine behind `@pattern`, and so not this module's.
//!
//! # What a caller does with them
//!
//! The violations are handed back STRUCTURALLY, for the caller to state in its own service's
//! envelope and under its own error code, counted together with the ones the walk did find:
//! a request breaking an expression the boundary applies AND one it skips is still ONE
//! counted refusal. [`stated_together`] is that merge, in the order AWS's own validator
//! states the clauses of one request.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_schema::serde::SerializableStruct;
use aws_smithy_schema::traits::SensitiveTrait;
use aws_smithy_schema::{Schema, TraitMap};
use aws_smithy_schema_server::constraints::{
    path, ElementConstraintsTrait, PatternTrait, Violation, ViolationKind,
};
use smithy_server_runtime::{DeserializableStruct, Operation, ServerHttpProtocol};

use crate::validation::{quoted_echo, submitted_leaves, MAX_VIOLATIONS};

/// The step budget ONE match of one caller value may spend.
///
/// A backtracking engine has no linear-time guarantee in general, so the work one request
/// can buy is bounded here rather than left to the shape of the expression. The expressions
/// themselves are MODEL CONSTANTS, never built from a request, and the value was already
/// held to the collecting walk's own ceiling, so this is the backstop for a model expression
/// that is not tame: a match that exhausts it leaves the value admitted, never spins.
const PATTERN_BACKTRACK_LIMIT: usize = 1_000_000;

/// The most values ONE request may have evaluated here.
///
/// The budget above bounds a single match; this bounds their NUMBER, which a request chooses
/// when the expression belongs to the elements of a collection, so the work one request can
/// buy is a product of two constants rather than of one constant and a request. It is above
/// the clause ceiling a message can carry ([`MAX_VIOLATIONS`]), so no clause this could
/// state is lost to it; a value past it is left ADMITTED, the treatment every other
/// undecided value gets.
const MAX_EVALUATED_VALUES: usize = 128;

/// Whether an expression constrains the value a member itself holds, or each element of the
/// collection it holds.
///
/// The distinction a member schema makes: its own trait map carries the member's
/// constraints, while the element shape's travel in an [`ElementConstraintsTrait`] wrapper on
/// that same member, which is where the validating walk reads them from too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holds {
    /// The member's own value.
    TheValue,
    /// Every element (or map value) the member holds.
    EachElement,
}

/// One position of an operation's input whose modelled `@pattern` the boundary does not
/// apply: the member path a validation message names it by, which of that member's values
/// the expression is about, the expression as the model wrote it, and whether a clause about
/// it may quote the value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unapplied {
    member: String,
    holds: Holds,
    pattern: &'static str,
    sensitive: bool,
}

impl Unapplied {
    /// Whether the leaf the collecting walk reports at `leaf` is a value this position's
    /// expression is about: the member itself for [`Holds::TheValue`], and anything the walk
    /// reached INSIDE that member for [`Holds::EachElement`], whatever segments it spells an
    /// element or an entry with.
    fn covers(&self, leaf: &str) -> bool {
        match self.holds {
            Holds::TheValue => leaf == self.member,
            Holds::EachElement => leaf
                .strip_prefix(self.member.as_str())
                .is_some_and(|inside| inside.starts_with('.')),
        }
    }
}

/// The `@pattern` a trait map declares, as the model wrote it.
fn modelled_pattern(traits: &TraitMap) -> Option<&'static str> {
    let declared = traits.get(&PatternTrait::TRAIT_ID)?;
    Some(declared.as_any().downcast_ref::<PatternTrait>()?.regex)
}

/// The element (or map value) shape's own constraint traits, which a collection member
/// carries in an [`ElementConstraintsTrait`] wrapper.
fn element_constraints(member: &'static Schema) -> Option<&'static TraitMap> {
    let wrapper = member.traits()?.get(&ElementConstraintsTrait::TRAIT_ID)?;
    let element = wrapper.as_any().downcast_ref::<ElementConstraintsTrait>()?;
    Some(element.inner())
}

/// Whether a trait map marks its member `@sensitive`, so a clause about it states no value.
fn is_sensitive(traits: &TraitMap) -> bool {
    traits.get(&SensitiveTrait::TRAIT_ID).is_some()
}

/// Every position of `schema` whose modelled `@pattern` the boundary validator declares and
/// does not apply.
///
/// Read off the schema, so no member and no expression is named here: the model says which
/// positions carry an expression and the engine behind `@pattern` says which of those it
/// compiles.
fn unapplied_in(schema: &'static Schema) -> Vec<Unapplied> {
    let mut found = Vec::new();
    for member in schema.members() {
        let Some(name) = member.member_name() else {
            continue;
        };
        let at = path::member(name);
        let own = member.traits();
        if let Some(pattern) = own.and_then(modelled_pattern) {
            if !applied_at_the_boundary(pattern) {
                found.push(Unapplied {
                    member: at.clone(),
                    holds: Holds::TheValue,
                    pattern,
                    sensitive: own.is_some_and(is_sensitive),
                });
            }
        }
        let Some(element) = element_constraints(member) else {
            continue;
        };
        if let Some(pattern) = modelled_pattern(element) {
            if !applied_at_the_boundary(pattern) {
                found.push(Unapplied {
                    member: at,
                    holds: Holds::EachElement,
                    // A collection whose member or whose element shape is `@sensitive` holds
                    // no value a refusal may echo, the reading the validating walk applies
                    // to one too.
                    sensitive: own.is_some_and(is_sensitive) || is_sensitive(element),
                    pattern,
                });
            }
        }
    }
    found
}

/// Every `@pattern` violation of `input` the boundary validator will not find, against the
/// positions `unapplied` names.
///
/// The values come from the SAME built input that walk validates, through the one shared
/// collecting walk ([`submitted_leaves`]), so a clause states the bytes the caller submitted
/// and names them by the path a violation is reported at. A clause about a `@sensitive`
/// member states NO value.
fn unseen(
    input: &dyn SerializableStruct,
    schema: &'static Schema,
    unapplied: &[Unapplied],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut evaluated = 0usize;
    for (at, value) in submitted_leaves(input, schema) {
        let Some(position) = unapplied.iter().find(|position| position.covers(&at)) else {
            continue;
        };
        // The aggregate budget ([`MAX_EVALUATED_VALUES`]): a request chooses how many values
        // a collection member holds, so the number of matches it buys is bounded here as
        // well as the steps of each one.
        if evaluated >= MAX_EVALUATED_VALUES {
            break;
        }
        evaluated += 1;
        if satisfies(position.pattern, &value) != Some(false) {
            continue;
        }
        violations.push(Violation {
            path: at,
            kind: ViolationKind::Pattern(position.pattern.to_string()),
            value: (!position.sensitive).then(|| quoted_echo(&value).into_owned()),
            sensitive: position.sensitive,
        });
    }
    violations
}

/// Whether `value` satisfies the modelled `pattern`, or `None` when no engine this crate
/// reaches can decide it — an expression neither compiles, or a match that spent its whole
/// step budget. Undecided leaves the value admitted, which is the framework's own treatment.
///
/// A Smithy `@pattern` is a FIND rather than a whole-value match
/// (`vendor/smithy-schema-server/src/constraints.rs`), so the expression is searched in the
/// value exactly as the boundary searches the ones it does compile.
fn satisfies(pattern: &'static str, value: &str) -> Option<bool> {
    reading(pattern).compiled?.is_match(value).ok()
}

/// Whether the boundary validator APPLIES `pattern`: whether the `regex` crate it validates
/// with compiles the expression at all. One that does not compile is treated there as
/// satisfied, which is the gap this module closes.
fn applied_at_the_boundary(pattern: &'static str) -> bool {
    reading(pattern).applied_at_the_boundary
}

/// What one modelled expression is to this check: whether the engine behind `@pattern`
/// compiles it, and its compiled form under the look-around-capable engine (`None` when that
/// engine cannot compile it either).
#[derive(Debug, Clone)]
struct Reading {
    applied_at_the_boundary: bool,
    compiled: Option<Arc<::fancy_regex::Regex>>,
}

/// The reading of `pattern`, compiled once per expression and held, keyed by the source text
/// so the compiled form cannot drift from the text a clause quotes.
///
/// Nothing expensive happens under the lock: it is taken for a lookup and for an insert and
/// never held across a compile or a match, so concurrent requests do not serialise behind
/// one.
fn reading(pattern: &'static str) -> Reading {
    static READINGS: LazyLock<Mutex<HashMap<&'static str, Reading>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    let held = locked(&READINGS).get(pattern).cloned();
    if let Some(held) = held {
        return held;
    }
    let read = Reading {
        applied_at_the_boundary: ::regex::Regex::new(pattern).is_ok(),
        compiled: ::fancy_regex::RegexBuilder::new(pattern)
            .backtrack_limit(PATTERN_BACKTRACK_LIMIT)
            .build()
            .ok()
            .map(Arc::new),
    };
    locked(&READINGS)
        .entry(pattern)
        .or_insert_with(|| read.clone())
        .clone()
}

/// This module's cache, locked. `into_inner` on poisoning rather than `unwrap`: a panic
/// elsewhere cannot corrupt a map of compiled model constants, and must not make every later
/// request panic with it.
fn locked<T>(cache: &'static LazyLock<Mutex<T>>) -> MutexGuard<'static, T> {
    cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The violations of ONE request as one refusal states them: the ones the shared collecting
/// walk reported ([`crate::validation::refusing_violations`]) and the ones whose expression
/// that walk could not compile ([`unapplied_violations`]) together.
///
/// Two orders are stated here, both AWS's:
///
/// * the MEMBERS are stated in the order the operation's input shape declares them — the
///   order the framework's own walk reports its violations in — so a clause the walk could
///   not see is stated where its member sits rather than after everything the walk did see;
/// * the clauses about ONE member state that member's `@pattern` requirement BEFORE its
///   other requirements, which is the order AWS's validator aggregates them in (a value
///   outside both a `@pattern` and a `@length` is reported as the pattern clause then the
///   length clause), where the framework's walk emits the length clause first.
///
/// Held to [`MAX_VIOLATIONS`], the ceiling the walk itself collects under, so the clause
/// count of one message stays a constant of this tree rather than something a request
/// chooses by breaking both classes at once.
#[must_use]
pub fn stated_together(
    schema: &'static Schema,
    walked: Vec<Violation>,
    unapplied: Vec<Violation>,
) -> Vec<Violation> {
    if unapplied.is_empty() {
        return walked;
    }
    let mut violations = walked;
    violations.extend(unapplied);
    violations.sort_by_key(|violation| {
        (
            declared_position(schema, &violation.path),
            pattern_first(&violation.kind),
        )
    });
    violations.truncate(MAX_VIOLATIONS);
    violations
}

/// Where the member a violation's `path` begins with sits among the members `schema`
/// declares, or past the last of them for a path naming none of them.
fn declared_position(schema: &'static Schema, at: &str) -> usize {
    let member = at.split('.').next().unwrap_or(at);
    schema
        .members()
        .iter()
        .position(|declared| {
            declared
                .member_name()
                .is_some_and(|name| path::member(name) == member)
        })
        .unwrap_or(usize::MAX)
}

/// The rank a clause about one member is stated at: a `@pattern` requirement before every
/// other, which is the order AWS states two clauses about one value in.
fn pattern_first(kind: &ViolationKind) -> usize {
    match kind {
        ViolationKind::Pattern(_) => 0,
        _ => 1,
    }
}

/// Every violation of `req` the boundary validator will not find under the operation `Op`,
/// because it could not compile the expression — empty for a request that breaks none, and
/// for an operation whose input declares no such expression, which is read first so no
/// request is read twice for a clause that cannot arise in it.
///
/// A body the codec cannot open, and a `@required` member the request omits, leave nothing
/// built to walk: those are refusals the shared collecting walk already states, so nothing is
/// collected for them here.
#[must_use]
pub fn unapplied_violations<Op: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Vec<Violation> {
    let unapplied = unapplied_in(Op::INPUT_SCHEMA);
    if unapplied.is_empty() {
        return Vec::new();
    }
    let Ok(mut deserializer) = protocol.deserialize_input(req, Op::INPUT_SCHEMA) else {
        return Vec::new();
    };
    let Ok(input) = <Op::Input as DeserializableStruct>::deserialize(&mut *deserializer) else {
        return Vec::new();
    };
    unseen(&input, Op::INPUT_SCHEMA, &unapplied)
}

#[cfg(test)]
mod tests {
    use super::*;

    use aws_smithy_schema::{ShapeId, ShapeType};

    /// An input shape declaring NO member, for the cases about clause ORDER: with no member
    /// to place a path at, every clause sits at the same position and the rank within one
    /// member is what the order states.
    static NO_DECLARED_MEMBERS: Schema = Schema::new_member(
        ShapeId::from_static("test#Input$input", "test", "input"),
        ShapeType::Structure,
        "input",
        0,
    );

    /// RULE: the check IS the model's own expression, look-around honoured, as a FIND over
    /// the value; an expression no engine here compiles decides nothing.
    ///
    /// The expression is the one AWS Cloud Map declares on `NamespaceNamePrivate`
    /// (`smithy_query(query="shape", shape="NamespaceNamePrivate")`), whose negative
    /// look-ahead is exactly what the boundary's engine cannot compile.
    #[test]
    fn the_check_is_the_modelled_expression() {
        const NAMESPACE_NAME_PRIVATE: &str = "^(?!arn:)[!-~]{1,253}$";
        let rows: [(&str, &str, Option<bool>); 6] = [
            ("a plain name", "example.internal", Some(true)),
            ("the reserved prefix", "arn:aws:x", Some(false)),
            // The look-ahead reserves the prefix and nothing else: the same letters
            // elsewhere in the value are a name.
            ("the prefix not at the start", "my.arn:name", Some(true)),
            ("a character the class excludes", "has a space", Some(false)),
            ("the empty value", "", Some(false)),
            ("one character past the declared run", &"x".repeat(254), Some(false)),
        ];
        for (case, value, expected) in rows {
            let decided = satisfies(NAMESPACE_NAME_PRIVATE, value);
            assert_eq!(
                decided, expected,
                "case {case:?}: expected the modelled expression to decide {expected:?}, got \
                 {decided:?}"
            );
        }

        assert_eq!(
            satisfies("^(?<!x)(?R)$", "anything"),
            None,
            "an expression no engine here compiles must decide nothing"
        );
    }

    /// RULE: an expression the boundary's own engine compiles is NOT this module's business
    /// — it is applied there — and one carrying look-around is.
    #[test]
    fn only_an_expression_the_boundary_cannot_compile_is_collected() {
        // (case, the expression, whether the boundary applies it)
        let rows: [(&str, &str, bool); 4] = [
            ("a character class", "^[!-~]{1,1024}$", true),
            ("a label grammar", r"^([a-zA-Z0-9]([a-zA-Z0-9\-]{0,61})?\.)+$", true),
            ("a negative look-ahead", "^(?!arn:)[!-~]{1,253}$", false),
            (
                "a positive look-ahead",
                "^((?=^.{1,127}$)^[a-zA-Z0-9]+$)$",
                false,
            ),
        ];
        for (case, pattern, expected) in rows {
            let applied = applied_at_the_boundary(pattern);
            assert_eq!(
                applied, expected,
                "case {case:?}: expected applied_at_the_boundary == {expected}, got {applied}"
            );
        }
    }

    /// RULE: a position's expression is about the member's own value, or about every value
    /// the walk reaches inside it, whatever segments that walk spells an element with.
    #[test]
    fn a_position_covers_the_leaves_its_expression_is_about() {
        let scalar = Unapplied {
            member: "name".to_string(),
            holds: Holds::TheValue,
            pattern: "^x$",
            sensitive: false,
        };
        let elements = Unapplied {
            member: "tagKeys".to_string(),
            holds: Holds::EachElement,
            pattern: "^x$",
            sensitive: false,
        };
        let rows: [(&str, &Unapplied, &str, bool); 6] = [
            ("the scalar member itself", &scalar, "name", true),
            ("another member", &scalar, "description", false),
            ("a member whose name starts alike", &scalar, "nameSuffix", false),
            ("an element of the collection", &elements, "tagKeys.1.member", true),
            ("an entry of the collection", &elements, "tagKeys.alpha.member", true),
            ("the collection member itself", &elements, "tagKeys", false),
        ];
        for (case, position, leaf, expected) in rows {
            let actual = position.covers(leaf);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected covers({leaf:?}) == {expected}, got {actual}"
            );
        }
    }

    /// RULE: two clauses about ONE member state the `@pattern` requirement first, and a
    /// clause the walk could not see is stated where its member sits.
    #[test]
    fn a_members_pattern_clause_is_stated_before_its_other_clauses() {
        fn about(at: &str, kind: ViolationKind) -> Violation {
            Violation {
                path: at.to_string(),
                kind,
                value: Some("v".to_string()),
                sensitive: false,
            }
        }
        // No schema is needed to order clauses about ONE member: every path resolves to the
        // same (unnamed) position, so the rank decides.
        let ordered = stated_together(
            &NO_DECLARED_MEMBERS,
            vec![
                about("name", ViolationKind::MaxLength(253)),
                about("other", ViolationKind::MaxLength(8)),
            ],
            vec![about("name", ViolationKind::Pattern("^x$".to_string()))],
        );
        let stated: Vec<(String, ViolationKind)> = ordered
            .into_iter()
            .map(|violation| (violation.path, violation.kind))
            .collect();
        assert_eq!(
            stated,
            vec![
                ("name".to_string(), ViolationKind::Pattern("^x$".to_string())),
                ("name".to_string(), ViolationKind::MaxLength(253)),
                ("other".to_string(), ViolationKind::MaxLength(8)),
            ],
            "expected the pattern clause of a member before its length clause, got {stated:?}"
        );
    }

    /// RULE: with nothing the boundary skipped, the walk's own violations are handed back
    /// untouched — this module reorders no refusal it has nothing to add to.
    #[test]
    fn a_refusal_with_nothing_unapplied_is_untouched() {
        let walked = vec![
            Violation {
                path: "name".to_string(),
                kind: ViolationKind::MaxLength(253),
                value: Some("v".to_string()),
                sensitive: false,
            },
            Violation {
                path: "name".to_string(),
                kind: ViolationKind::Pattern("^x$".to_string()),
                value: Some("v".to_string()),
                sensitive: false,
            },
        ];
        let stated = stated_together(&NO_DECLARED_MEMBERS, walked.clone(), Vec::new());
        assert_eq!(
            stated, walked,
            "expected the walk's own violations unchanged, got {stated:?}"
        );
    }
}
