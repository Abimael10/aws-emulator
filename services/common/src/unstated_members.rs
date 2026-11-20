// SPDX-License-Identifier: CC0-1.0

//! What a request left UNSTATED about the shapes its operation's input is read into: every
//! `@required` member it omits, and every union-typed member it does not set exactly one
//! variant of.
//!
//! # The two gaps this closes
//!
//! A pre-handler refusal is worded from the violations the framework's own machinery finds
//! ([`crate::validation::refusing_violations`]), and both of these are found while the
//! request is READ rather than while the built value is walked — so the read stops at the
//! first one and the refusal states only that one:
//!
//! * a `@required` member the request omits is raised by the generated
//!   `DeserializableStruct::deserialize`, which assembles its fields in declaration order
//!   and returns `SerdeError::MissingMember` at the first slot nothing filled. Every member
//!   after it, and every later element of a list whose elements each carry required members,
//!   is never looked at. AWS states ONE CLAUSE PER VIOLATION — a batch whose second entry
//!   omits one member and whose third omits another is refused with both clauses, which is
//!   the only thing that says WHICH entry was malformed.
//! * a union member the request sets other than exactly once is not a constraint at all:
//!   the generated union read takes the last variant the request stated and fails, with an
//!   error carrying no violation, when it stated none. So a request that sets both variants
//!   is read as if it had set one, and a request that sets neither is refused in an envelope
//!   the service never chose.
//!
//! # How they are found
//!
//! [`unstated_members`] drives the operation's OWN generated input type over the request
//! through [`Reading`], a deserializer that is the protocol's own at every leaf and adds two
//! things at the containers it passes through:
//!
//! * it keeps the dotted member path of the position being read, spelled by
//!   [`aws_smithy_schema_server::constraints::path`] — the same speller the deserializer and
//!   the constraint walk use, so a position this reports and one they report are the same
//!   text;
//! * where the request omitted a member an EARLIER read already reported as required, it
//!   supplies a placeholder ([`Supplying`]) so the read carries on past it and reaches the
//!   next one. The read is repeated, each pass supplying every member reported so far, until
//!   it completes or stops making progress — so the members collected are exactly the ones
//!   the framework's own reads would have raised, one per pass, and nothing here decides for
//!   itself whether a member is required.
//!
//! The values [`Supplying`] hands back are never used: the walk is run for the PATHS it
//! collects and its built input is dropped. That is also why a placeholder cannot make a
//! request pass — this walk refuses nothing and serves nothing; the service's own refusal
//! path reads the request again through the framework's unmodified sequence.
//!
//! # The bounds this states
//!
//! One pass per omitted member, so the passes are bounded by
//! [`crate::validation::MAX_VIOLATIONS`], the most violations one refusal states — and the
//! positions read ACROSS those passes are bounded by [`MAX_REREAD_POSITIONS`], so what one
//! request can spend here is a product of two constants rather than the pass count times a
//! body the caller sized. The passes are separated by a yield point, so a timeout, a
//! cancellation or a graceful shutdown lands between two of them. The placeholder subtree a
//! supplied structure member needs is bounded by [`SUPPLIED_NODES`] positions, so a model
//! whose shapes nest into one another cannot make one pass unbounded; a subtree cut short by
//! that budget leaves a member unfilled, the next pass makes no progress and the walk stops
//! with what it has.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_schema::serde::{SerdeError, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeType};
use aws_smithy_schema_server::constraints::{path, Violation, ViolationKind};
use aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};
use smithy_server_runtime::{DeserializableStruct, Operation, ServerHttpProtocol};

/// The most placeholder positions one pass may write while supplying the members earlier
/// passes reported. A member whose target shape reaches itself, directly or through another
/// shape, has no finite subtree, so the budget — and not the model — is what ends the fill.
const SUPPLIED_NODES: usize = 512;

/// The most member positions this walk reads ACROSS every pass of one request.
///
/// The pass COUNT is bounded on its own ([`crate::validation::MAX_VIOLATIONS`]), but that
/// bounds only one factor: each pass re-reads the whole request, so without this the work
/// one request costs would be that count TIMES a body whose size the caller chose. This is
/// the other factor, so the product of the two is a product of two constants: a request
/// large enough to spend the whole allowance in its first passes gets no further ones, and
/// is refused with the absences those passes did find. 8192 positions is far above the
/// member count of any input shape a model of this tree declares, so no ordinary request
/// reaches it.
const MAX_REREAD_POSITIONS: usize = 8_192;

/// What one request left unstated about the shapes it is read into.
#[derive(Debug, Default)]
pub struct Unstated {
    /// The dotted member path of every `@required` member the request omitted, in the order
    /// the read reaches them: the spelling a clause states a position with
    /// (`entries.2.member.detailType`).
    pub omitted: Vec<String>,
    /// The Smithy MEMBER NAME of every union-typed member the request did not set exactly
    /// one variant of — as the model spells it, since the member name is what a refusal of
    /// the union's arity names.
    pub unions: Vec<String>,
}

/// What `req` left unstated about `O`'s input shapes.
///
/// Both lists are empty for a request that states every required member and exactly one
/// variant of every union it carries, and for a request this walk could not read at all (a
/// body outside the protocol's own grammar), which the framework answers itself.
///
/// ASYNC because it is driven from the request-handling path and does bounded but repeated
/// work: the passes are separated by a yield point, so a request timeout, a cancellation or
/// a graceful shutdown lands between two passes rather than only after the last one. What
/// one request may spend here is bounded twice over — [`crate::validation::MAX_VIOLATIONS`]
/// passes and [`MAX_REREAD_POSITIONS`] positions across all of them — so neither the number
/// of re-reads nor their total size is chosen by the caller.
pub async fn unstated_members<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Unstated {
    let mut omitted: Vec<String> = Vec::new();
    let mut unions: Vec<String> = Vec::new();
    let mut positions = 0usize;
    while omitted.len() < crate::validation::MAX_VIOLATIONS {
        // One pass, whose deserializer and built input are both dropped before it returns,
        // so nothing of the request is held across the yield below.
        let Some(pass) = one_pass::<O>(protocol, req, &omitted) else {
            return Unstated { omitted, unions };
        };
        positions = positions.saturating_add(pass.positions);
        // The union arity is decided by the FIRST pass, which reads the request as the
        // caller sent it; a later pass reads the same body with placeholders in it and
        // cannot see a different set.
        if omitted.is_empty() {
            unions = pass.unions;
        }
        match pass.missing {
            // Nothing left to find: either the read completed with every member an earlier
            // pass reported supplied, or it failed for another reason — which the
            // framework's own walk words.
            None => return Unstated { omitted, unions },
            // A member UNDER one this pass already supplied is a position the placeholder
            // created rather than one the caller omitted, and no progress besides.
            Some(member_name) if supplied_already(&member_name, &omitted) => {
                return Unstated { omitted, unions }
            }
            Some(member_name) => omitted.push(member_name),
        }
        // The allowance for re-reading THIS request, spent: what the passes so far found is
        // what the refusal states.
        if positions >= MAX_REREAD_POSITIONS {
            return Unstated { omitted, unions };
        }
        ::tokio::task::yield_now().await;
    }
    Unstated { omitted, unions }
}

/// What one pass over the request found.
struct Pass {
    /// The dotted path of the required member the read stopped at, and `None` when the read
    /// completed or failed for another reason.
    missing: Option<String>,
    /// The union members this pass found set other than exactly once.
    unions: Vec<String>,
    /// How many member positions this pass read, which is what it cost.
    positions: usize,
}

/// One pass over `req`, supplying the members `supplied` names, or `None` when the protocol
/// could not read the request at all.
///
/// Holds nothing of the request when it returns: the deserializer and the input it built are
/// both dropped here, which is what lets the caller await between passes.
fn one_pass<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
    supplied: &[String],
) -> Option<Pass> {
    let mut inner = protocol.deserialize_input(req, O::INPUT_SCHEMA).ok()?;
    let walk = Walk {
        supplied: supplied.iter().cloned().collect(),
        unions: RefCell::new(Vec::new()),
        positions: Cell::new(0),
    };
    let read = {
        let mut reading = Reading::root(&mut *inner, &walk);
        <O::Input as DeserializableStruct>::deserialize(&mut reading)
    };
    drop(inner);
    let missing = match read {
        Err(SerdeError::MissingMember { member_name }) => Some(member_name),
        _ => None,
    };
    Some(Pass {
        missing,
        unions: walk.unions.into_inner(),
        positions: walk.positions.get(),
    })
}

/// `violations` with EVERY omitted required member stated, where the read could only
/// report the first one.
///
/// A no-op unless the framework's walk reported exactly one absence and this walk found
/// more: anything else — a value outside a constraint, an absence this walk agrees is the
/// only one, a request neither could read — keeps the framework's own violations exactly as
/// they were reported.
#[must_use]
pub fn stating_every_absence(violations: Vec<Violation>, omitted: &[String]) -> Vec<Violation> {
    if omitted.len() < 2 || !states_one_absence(&violations) {
        return violations;
    }
    omitted.iter().map(|path| absence(path.clone())).collect()
}

/// Whether a walk over the request could add anything to `violations`.
///
/// Only two cases: exactly ONE absence, which is where the read stopped and so the only set
/// [`stating_every_absence`] can extend, and NO violation at all, where a union whose arity
/// is broken is what the walk can still find — the arity is no constraint, so it appears in
/// no violation list, and it cannot be settled from the input schema either, because a
/// member schema carries its member's NAME rather than its target shape. A request with two
/// violations, or with one that is not an absence, is already stated in full and is answered
/// without the walk.
#[must_use]
pub fn worth_walking(violations: &[Violation]) -> bool {
    violations.is_empty() || states_one_absence(violations)
}

/// Whether `violations` is the single absence a read that stopped at the first missing
/// member produces.
fn states_one_absence(violations: &[Violation]) -> bool {
    match violations {
        [only] => only.kind == ViolationKind::MissingRequiredMember,
        _ => false,
    }
}

/// The violation of a member the request omitted: the path it sits at, and no value, since
/// an absent member has none to echo.
fn absence(path: String) -> Violation {
    Violation {
        path,
        kind: ViolationKind::MissingRequiredMember,
        value: None,
        sensitive: false,
    }
}

/// Whether `path` names a position a pass already supplied a placeholder at, or one INSIDE
/// such a position — a member of a structure this walk made up, which no caller omitted.
fn supplied_already(path: &str, omitted: &[String]) -> bool {
    omitted
        .iter()
        .any(|supplied| path == supplied || path.starts_with(&format!("{supplied}.")))
}

/// The state one pass of the walk shares across every nesting level it opens.
struct Walk {
    /// The member paths this pass supplies a placeholder at.
    supplied: HashSet<String>,
    /// The union members found set other than exactly once, in walk order.
    unions: RefCell<Vec<String>>,
    /// How many member positions this pass has read, which is what the pass costs and what
    /// [`MAX_REREAD_POSITIONS`] is spent out of.
    positions: Cell<usize>,
}

impl Walk {
    /// Record one more member position read.
    fn read_a_position(&self) {
        self.positions.set(self.positions.get().saturating_add(1));
    }
}

/// The deserializer the request is read with: the protocol's own at every leaf, keeping the
/// path of the position it reads and supplying the members [`Walk::supplied`] names.
struct Reading<'a, 'w> {
    inner: &'a mut dyn ShapeDeserializer,
    walk: &'w Walk,
    /// The dotted path of this position, empty at the input shape itself.
    at: String,
    /// The Smithy member name of this position, empty at the input shape itself.
    member: String,
}

impl<'a, 'w> Reading<'a, 'w> {
    /// The reader of the operation's input shape, at the root of the path.
    fn root(inner: &'a mut dyn ShapeDeserializer, walk: &'w Walk) -> Self {
        Self {
            inner,
            walk,
            at: String::new(),
            member: String::new(),
        }
    }
}

impl ShapeDeserializer for Reading<'_, '_> {
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let at = self.at.clone();
        let walk = self.walk;
        let stated: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
        let outcome = self.inner.read_struct(schema, &mut |member, inner| {
            walk.read_a_position();
            let at_member = member_path(&at, member);
            stated.borrow_mut().insert(at_member.clone());
            let mut nested = Reading {
                inner,
                walk,
                at: at_member,
                member: member.member_name().unwrap_or_default().to_string(),
            };
            consumer(member, &mut nested)
        });
        let stated = stated.into_inner();
        // "Exactly one variant must be set" is the rule a discriminated union carries, and
        // the request states its variants the way it states a structure's members — so the
        // count the read saw here is the arity, whether the read then succeeded or not.
        if matches!(schema.shape_type(), ShapeType::Union) && stated.len() != 1 {
            walk.unions.borrow_mut().push(self.member.clone());
        }
        outcome?;
        // The members this pass supplies: reported as required by an earlier pass, so the
        // read goes on past them instead of stopping at the first one again.
        for member in schema.members() {
            let at_member = member_path(&at, member);
            if stated.contains(&at_member) || !walk.supplied.contains(&at_member) {
                continue;
            }
            let budget = Cell::new(SUPPLIED_NODES);
            consumer(member, &mut Supplying { budget: &budget })?;
        }
        Ok(())
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let at = self.at.clone();
        let member = self.member.clone();
        let walk = self.walk;
        // The 1-based position of the element being read, counted the way the generated
        // read counts it: every `read_list` calls the consumer once per element, in order.
        let position = Cell::new(0usize);
        self.inner.read_list(schema, &mut |inner| {
            walk.read_a_position();
            position.set(position.get() + 1);
            let mut nested = Reading {
                inner,
                walk,
                at: joined(&at, &path::element(position.get())),
                member: member.clone(),
            };
            consumer(&mut nested)
        })
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let at = self.at.clone();
        let member = self.member.clone();
        let walk = self.walk;
        self.inner.read_map(schema, &mut |key, inner| {
            walk.read_a_position();
            let entry = joined(&at, &path::entry(&key));
            let mut nested = Reading {
                inner,
                walk,
                at: entry,
                member: member.clone(),
            };
            consumer(key, &mut nested)
        })
    }

    fn read_boolean(&mut self, schema: &Schema) -> Result<bool, SerdeError> {
        self.inner.read_boolean(schema)
    }

    fn read_byte(&mut self, schema: &Schema) -> Result<i8, SerdeError> {
        self.inner.read_byte(schema)
    }

    fn read_short(&mut self, schema: &Schema) -> Result<i16, SerdeError> {
        self.inner.read_short(schema)
    }

    fn read_integer(&mut self, schema: &Schema) -> Result<i32, SerdeError> {
        self.inner.read_integer(schema)
    }

    fn read_long(&mut self, schema: &Schema) -> Result<i64, SerdeError> {
        self.inner.read_long(schema)
    }

    fn read_float(&mut self, schema: &Schema) -> Result<f32, SerdeError> {
        self.inner.read_float(schema)
    }

    fn read_double(&mut self, schema: &Schema) -> Result<f64, SerdeError> {
        self.inner.read_double(schema)
    }

    fn read_big_integer(&mut self, schema: &Schema) -> Result<BigInteger, SerdeError> {
        self.inner.read_big_integer(schema)
    }

    fn read_big_decimal(&mut self, schema: &Schema) -> Result<BigDecimal, SerdeError> {
        self.inner.read_big_decimal(schema)
    }

    fn read_string(&mut self, schema: &Schema) -> Result<String, SerdeError> {
        self.inner.read_string(schema)
    }

    fn read_blob(&mut self, schema: &Schema) -> Result<Blob, SerdeError> {
        self.inner.read_blob(schema)
    }

    fn read_timestamp(&mut self, schema: &Schema) -> Result<DateTime, SerdeError> {
        self.inner.read_timestamp(schema)
    }

    fn read_document(&mut self, schema: &Schema) -> Result<Document, SerdeError> {
        self.inner.read_document(schema)
    }

    fn is_null(&self) -> bool {
        self.inner.is_null()
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        self.inner.read_null()
    }

    fn container_size(&self) -> Option<usize> {
        self.inner.container_size()
    }
}

/// The deserializer a SUPPLIED member is read through: it reads nothing of the request and
/// hands back a placeholder of the member's modelled kind, so the read carries on past a
/// member an earlier pass already reported.
///
/// Every value it produces is thrown away with the input it was built into (see the module
/// header). A structure is filled member by member so a shape nested under the supplied one
/// raises no absence of its own; the fill is bounded by `budget`, which the whole subtree
/// shares.
struct Supplying<'b> {
    budget: &'b Cell<usize>,
}

impl Supplying<'_> {
    /// Whether one more placeholder position fits the budget, charging it when it does.
    fn charged(&self) -> bool {
        match self.budget.get() {
            0 => false,
            left => {
                self.budget.set(left - 1);
                true
            }
        }
    }
}

impl ShapeDeserializer for Supplying<'_> {
    /// Every member the shape declares, each supplied in turn — including the variants of a
    /// union, whose generated read keeps the last one it is handed.
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        for member in schema.members() {
            if !self.charged() {
                return Ok(());
            }
            consumer(
                member,
                &mut Supplying {
                    budget: self.budget,
                },
            )?;
        }
        Ok(())
    }

    /// A collection with no elements: a placeholder states nothing the caller sent, and an
    /// element of its own would carry its own required members.
    fn read_list(
        &mut self,
        _schema: &Schema,
        _consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        Ok(())
    }

    /// A map with no entries, for the reason [`Supplying::read_list`] gives.
    fn read_map(
        &mut self,
        _schema: &Schema,
        _consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        Ok(())
    }

    fn read_boolean(&mut self, _schema: &Schema) -> Result<bool, SerdeError> {
        Ok(false)
    }

    fn read_byte(&mut self, _schema: &Schema) -> Result<i8, SerdeError> {
        Ok(0)
    }

    fn read_short(&mut self, _schema: &Schema) -> Result<i16, SerdeError> {
        Ok(0)
    }

    fn read_integer(&mut self, _schema: &Schema) -> Result<i32, SerdeError> {
        Ok(0)
    }

    fn read_long(&mut self, _schema: &Schema) -> Result<i64, SerdeError> {
        Ok(0)
    }

    fn read_float(&mut self, _schema: &Schema) -> Result<f32, SerdeError> {
        Ok(0.0)
    }

    fn read_double(&mut self, _schema: &Schema) -> Result<f64, SerdeError> {
        Ok(0.0)
    }

    fn read_big_integer(&mut self, _schema: &Schema) -> Result<BigInteger, SerdeError> {
        Ok(BigInteger::default())
    }

    fn read_big_decimal(&mut self, _schema: &Schema) -> Result<BigDecimal, SerdeError> {
        Ok(BigDecimal::default())
    }

    fn read_string(&mut self, _schema: &Schema) -> Result<String, SerdeError> {
        Ok(String::new())
    }

    fn read_blob(&mut self, _schema: &Schema) -> Result<Blob, SerdeError> {
        Ok(Blob::new(Vec::new()))
    }

    fn read_timestamp(&mut self, _schema: &Schema) -> Result<DateTime, SerdeError> {
        Ok(DateTime::from_secs(0))
    }

    fn read_document(&mut self, _schema: &Schema) -> Result<Document, SerdeError> {
        Ok(Document::Null)
    }

    /// A supplied member stands in for one the request omitted, so it reads as a value that
    /// is THERE: a reader that called itself null would have the generated read skip it and
    /// raise the very absence this is supplying past.
    fn is_null(&self) -> bool {
        false
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        Ok(())
    }

    fn container_size(&self) -> Option<usize> {
        Some(0)
    }
}

/// The dotted path of `member` inside the position at `at`, spelled the way a clause spells
/// a member. A schema carrying no member name — a collection's own sub-member — adds no
/// segment of its own.
fn member_path(at: &str, member: &Schema) -> String {
    match member.member_name() {
        Some(name) => joined(at, &path::member(name)),
        None => at.to_string(),
    }
}

/// `segment` appended to the path `at`, which is empty at the input shape itself.
fn joined(at: &str, segment: &str) -> String {
    if at.is_empty() {
        segment.to_string()
    } else {
        format!("{at}.{segment}")
    }
}

#[cfg(test)]
mod tests {
    use super::{absence, joined, stating_every_absence, supplied_already, worth_walking};
    use aws_smithy_schema_server::constraints::{path, Violation, ViolationKind};

    /// One row of the replacement table: the case, the violations the framework reported,
    /// the members the walk found, and the paths the refusal then states.
    type Replacement = (&'static str, Vec<Violation>, Vec<String>, Vec<String>);

    /// A violation of a value outside a declared constraint, which no walk can add to.
    fn pattern_violation(at: &str) -> Violation {
        Violation {
            path: at.to_string(),
            kind: ViolationKind::Pattern("[a-z]+".to_string()),
            value: Some("A".to_string()),
            sensitive: false,
        }
    }

    /// RULE: the walk is run for exactly two violation sets — none at all, and one absence.
    /// The rows span what the predicate generalises over: the count, and the kind of the
    /// single violation.
    #[test]
    fn only_an_empty_set_and_a_single_absence_are_worth_walking() {
        let rows: Vec<(&str, Vec<Violation>, bool)> = vec![
            ("no violation at all", Vec::new(), true),
            (
                "one absence, where the read stopped",
                vec![absence("name".to_string())],
                true,
            ),
            (
                "one violation that is not an absence",
                vec![pattern_violation("name")],
                false,
            ),
            (
                "two absences, already stated in full",
                vec![
                    absence("name".to_string()),
                    absence("eventBusArn".to_string()),
                ],
                false,
            ),
            (
                "an absence beside a broken constraint",
                vec![absence("name".to_string()), pattern_violation("other")],
                false,
            ),
        ];
        for (case, violations, expected) in rows {
            let walked = worth_walking(&violations);
            assert_eq!(
                walked, expected,
                "case {case:?}: expected worth_walking {expected}, got {walked}"
            );
        }
    }

    /// RULE: the violations are replaced by one clause per omitted member only where the
    /// framework reported a single absence and the walk found more; every other pairing
    /// keeps the framework's own violations exactly as reported.
    #[test]
    fn every_omitted_member_replaces_the_single_absence_and_nothing_else() {
        let one = vec![absence("entries.1.member.source".to_string())];
        let found = [
            "entries.1.member.source".to_string(),
            "entries.2.member.detailType".to_string(),
        ];
        let rows: Vec<Replacement> = vec![
            (
                "one absence, two found",
                one.clone(),
                found.to_vec(),
                found.to_vec(),
            ),
            (
                "one absence the walk agrees is the only one",
                one.clone(),
                vec!["entries.1.member.source".to_string()],
                vec!["entries.1.member.source".to_string()],
            ),
            (
                "one absence and a walk that found none",
                one.clone(),
                Vec::new(),
                vec!["entries.1.member.source".to_string()],
            ),
            (
                "a broken constraint, whatever the walk found",
                vec![pattern_violation("name")],
                found.to_vec(),
                vec!["name".to_string()],
            ),
        ];
        for (case, violations, omitted, expected) in rows {
            let stated: Vec<String> = stating_every_absence(violations, &omitted)
                .into_iter()
                .map(|violation| violation.path)
                .collect();
            assert_eq!(
                stated, expected,
                "case {case:?}: expected the clauses at {expected:?}, got {stated:?}"
            );
        }
    }

    /// RULE: a path a pass already supplied, and any position INSIDE it, is a position the
    /// placeholder created rather than one the caller omitted. A SIBLING of a supplied
    /// member, or one whose path merely starts with the same characters, is not.
    #[test]
    fn a_position_inside_a_supplied_member_is_not_one_the_caller_omitted() {
        let supplied = ["configuration".to_string(), "entries.1.member".to_string()];
        let rows: [(&str, bool); 6] = [
            ("configuration", true),
            ("configuration.awsService", true),
            ("entries.1.member.source", true),
            ("entries.2.member.source", false),
            ("configurationExtra", false),
            ("name", false),
        ];
        for (at, expected) in rows {
            let inside = supplied_already(at, &supplied);
            assert_eq!(
                inside, expected,
                "case {at:?}: expected supplied_already {expected}, got {inside}"
            );
        }
    }

    /// RULE: a path is the segments joined with `.`, and the input shape itself contributes
    /// no segment — so the spelling is the one a clause states a position with.
    #[test]
    fn a_path_opens_at_the_input_shape_and_joins_its_segments() {
        assert_eq!(joined("", &path::member("Entries")), "entries");
        assert_eq!(joined("entries", &path::element(2)), "entries.2.member");
        assert_eq!(
            joined("entries.2.member", &path::member("DetailType")),
            "entries.2.member.detailType"
        );
        assert_eq!(
            joined("requestItems", &path::entry("MyTable")),
            "requestItems.MyTable.member"
        );
    }
}
