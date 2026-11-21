// SPDX-License-Identifier: CC0-1.0

//! The modelled `@pattern` this service's front door declares but cannot APPLY, applied.
//!
//! # The rule
//!
//! A member's `@pattern` is a rule about the REQUEST: a value the expression does not admit
//! is refused before any state this service holds is looked at, with the modelled
//! `InvalidInputException` and AWS's constraint envelope naming the member and quoting the
//! expression ([`super::validation`]). That holds for every expression the model declares,
//! including one no character grammar can state on its own.
//!
//! The framework's constraint walk compiles a `@pattern` with the `regex` crate and treats
//! one it cannot compile as SATISFIED (`pattern_matches` in
//! `vendor/smithy-schema-server/src/constraints.rs`, read-only from here). That crate has no
//! look-around, so a member whose expression carries one is admitted whatever it holds: the
//! requirement the model states for it reaches no request, and a value AWS refuses as
//! invalid input instead travels into a handler, where it is answered from a state lookup —
//! a different error class, a different status, and a diagnostic naming the wrong member.
//!
//! This module is that gap closed. No member and no expression is named here: which members
//! carry one is read off the operation's own input schema, and the CHECK is the model's own
//! expression text, evaluated by a look-around-capable engine (`fancy-regex`, as sibling
//! service crates already do for the same reason). Two bounds keep that evaluation finite,
//! and both are properties of the inputs rather than of the engine: the expressions are
//! MODEL CONSTANTS, never built from a request, and the backtracking engine is given an
//! explicit step budget ([`PATTERN_BACKTRACK_LIMIT`]). An expression NEITHER engine
//! compiles, and a match that exhausts that budget, are both left SATISFIED — the
//! framework's own treatment, so no value is refused because of an expression nothing can
//! evaluate.
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
//! The violations are handed to [`super::validation`], which counts them in the same
//! envelope as the ones the walk found: a request breaking an expression the boundary applies
//! AND one it skips is still ONE counted refusal.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use aws_smithy_runtime_api::http::Request as WireRequest;
use aws_smithy_schema::serde::SerializableStruct;
use aws_smithy_schema::traits::SensitiveTrait;
use aws_smithy_schema::{Schema, TraitMap};
use aws_smithy_schema_server::constraints::{
    path, ElementConstraintsTrait, PatternTrait, Violation, ViolationKind,
};
use service_common::validation::{quoted_echo, submitted_leaves};
use smithy_server_runtime::{DeserializableStruct, Operation, ServerHttpProtocol};

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
/// the clause ceiling a message can carry (`service_common::validation::MAX_VIOLATIONS`), so
/// no clause this door could state is lost to it; a value past it is left ADMITTED, the
/// treatment every other undecided value gets.
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
/// Read off the schema, so no member and no expression is named in this crate: the model says
/// which positions carry an expression and the engine behind `@pattern` says which of those
/// it compiles.
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
                    pattern,
                    // A collection whose member or whose element shape is `@sensitive` holds
                    // no value a refusal may echo, the reading the validating walk applies to
                    // one too.
                    sensitive: own.is_some_and(is_sensitive) || is_sensitive(element),
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

/// `violations` in the order the input shape declares the members they are about — the order
/// the framework's own walk reports its violations in, so a clause the walk could not see is
/// stated where its member sits rather than after everything the walk did see.
///
/// A STABLE sort on the member's declared position, so two clauses about one member keep the
/// order they were found in (a length clause before that member's pattern clause, as the walk
/// emits them), and a path the schema does not name is stated last rather than dropped.
pub(super) fn in_member_order(
    schema: &'static Schema,
    mut violations: Vec<Violation>,
) -> Vec<Violation> {
    violations.sort_by_key(|violation| declared_position(schema, &violation.path));
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

/// This module's cache, locked. `into_inner` on poisoning rather than `unwrap`: a panic
/// elsewhere cannot corrupt a map of compiled model constants, and must not make every later
/// request panic with it.
fn locked<T>(cache: &'static LazyLock<Mutex<T>>) -> MutexGuard<'static, T> {
    cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Every violation of `req` the boundary validator will not find under the operation `Op`,
/// because it could not compile the expression — empty for a request that breaks none, and
/// for an operation whose input declares no such expression, which is read first so no
/// request is read twice for a clause that cannot arise in it.
///
/// A body the codec cannot open, and a `@required` member the request omits, leave nothing
/// built to walk: those are refusals the shared collecting walk already states, so nothing is
/// collected for them here.
pub(super) fn unapplied_violations<Op: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &WireRequest,
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

    /// RULE: the positions collected are exactly the ones whose modelled `@pattern` the
    /// engine behind `@pattern` cannot compile — read off the input schema, with no member
    /// named here.
    #[test]
    fn the_collected_positions_are_the_ones_the_boundary_skips() {
        // `CreateEventSource` declares three: `Name` (target `EventSourceName`, whose
        // expression opens with the look-ahead reserving the `aws.` prefix), `Description`
        // (whose expression closes with a look-ahead standing for the end of the value), and
        // the VALUES of its `Tags` map (target `TagValue`, whose expression closes with the
        // same look-ahead). Source: `smithy_query(query="shape",
        // shape="EventSourceName,Description,TagMap")`.
        let collected = unapplied_in(crate::ops::types::CREATE_EVENT_SOURCE_INPUT_SCHEMA);
        let positions: Vec<(&str, Holds)> = collected
            .iter()
            .map(|position| (position.member.as_str(), position.holds))
            .collect();
        for expected in [
            ("name", Holds::TheValue),
            ("description", Holds::TheValue),
            ("tags", Holds::EachElement),
        ] {
            assert!(
                positions.contains(&expected),
                "case {expected:?}: expected the position to be collected, got {positions:?}"
            );
        }
        for position in &collected {
            assert!(
                ::regex::Regex::new(position.pattern).is_err(),
                "case {:?}: only an expression the boundary cannot compile belongs here, got \
                 {:?}",
                position.member,
                position.pattern
            );
        }

        // An operation whose input declares only expressions the boundary itself applies
        // collects nothing: its members are refused by the walk, not from here.
        assert!(
            unapplied_in(crate::ops::types::DESCRIBE_EVENT_SOURCE_INPUT_SCHEMA).is_empty(),
            "DescribeEventSource's only member is an ARN whose expression the boundary \
             compiles, so nothing is collected for it"
        );
    }

    /// RULE: the check IS the model's own expression, look-around honoured, as a FIND over
    /// the value; an expression no engine here compiles decides nothing.
    #[test]
    fn the_check_is_the_modelled_expression() {
        // `EventSourceName`: `^(?!aws\.)[A-Za-z0-9][\.\-_A-Za-z0-9]*$`.
        // Source: `smithy_query(query="shape", shape="EventSourceName")`.
        const EVENT_SOURCE_NAME: &str = r"^(?!aws\.)[A-Za-z0-9][\.\-_A-Za-z0-9]*$";
        let rows: [(&str, &str, Option<bool>); 7] = [
            ("a plain name", "orders", Some(true)),
            ("every separator the class adds", "a.b-c_d9", Some(true)),
            ("the reserved prefix", "aws.reserved", Some(false)),
            // The look-ahead reserves the prefix `aws.` and nothing else: the same letters
            // without the dot are a name, and the prefix elsewhere in the value is too.
            ("the reserved letters with no dot", "awsreserved", Some(true)),
            ("the reserved prefix not at the start", "my.aws.source", Some(true)),
            ("a leading separator the class forbids", ".orders", Some(false)),
            ("the empty value", "", Some(false)),
        ];
        for (case, value, expected) in rows {
            let decided = satisfies(EVENT_SOURCE_NAME, value);
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

    /// RULE: clauses are stated in the order the input shape declares the members they are
    /// about, two clauses about one member keep the order they were found in, and a path the
    /// shape names nothing for is stated last rather than dropped.
    #[test]
    fn clauses_are_stated_in_the_order_the_shape_declares_their_members() {
        fn about(at: &str) -> Violation {
            Violation {
                path: at.to_string(),
                kind: ViolationKind::Pattern("^x$".to_string()),
                value: Some(at.to_string()),
                sensitive: false,
            }
        }
        // `CreateEventSourceRequest` declares Name, EventBusArn, Configuration, Description,
        // Tags, ClientToken, in that order.
        // Source: `smithy_query(query="operation", operation="CreateEventSource")`.
        let found = vec![
            about("clientToken"),
            about("tags.owner"),
            about("name"),
            about("nowhere"),
            about("description"),
        ];
        let stated: Vec<String> = in_member_order(
            crate::ops::types::CREATE_EVENT_SOURCE_INPUT_SCHEMA,
            found.clone(),
        )
        .into_iter()
        .map(|violation| violation.path)
        .collect();
        assert_eq!(
            stated,
            vec![
                "name".to_string(),
                "description".to_string(),
                "tags.owner".to_string(),
                "clientToken".to_string(),
                "nowhere".to_string(),
            ],
            "expected the shape's own member order, got {stated:?}"
        );

        // Two clauses about ONE member keep the order they arrived in: the walk states a
        // member's length clause before its pattern clause, and this must not resequence them.
        let same_member = vec![
            Violation {
                kind: ViolationKind::MinLength(1),
                ..about("name")
            },
            about("name"),
        ];
        let kinds: Vec<ViolationKind> = in_member_order(
            crate::ops::types::CREATE_EVENT_SOURCE_INPUT_SCHEMA,
            same_member.clone(),
        )
        .into_iter()
        .map(|violation| violation.kind)
        .collect();
        assert_eq!(
            kinds,
            same_member
                .into_iter()
                .map(|violation| violation.kind)
                .collect::<Vec<_>>(),
            "two clauses about one member must keep the order they were found in, got {kinds:?}"
        );
    }
}
