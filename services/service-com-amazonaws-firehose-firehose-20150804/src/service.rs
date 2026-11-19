// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-firehose-firehose-20150804` service: how it claims requests and how it words the
//! failures the framework hands back.
//!
//! EDIT THIS FILE. It is emitted once with working defaults and is never
//! regenerated, so a choice made here survives adding another service or
//! re-running the generator.
//!
//! `EmulatedService` is what the router mounts (`EmulatedService::new(engine)`).
//! It wraps the generated [`ServiceDefinition`], which carries the model-derived half
//! of this service — its identity, its wire protocol, and its operation routing
//! table — built from the Smithy model at compile time. You do not edit the
//! entry; you edit the behavioral seams below.
//!
//! Five opt-in seams are yours to change; every one defaults to the framework's
//! own behaviour, so an unedited service still routes and answers correctly:
//!
//! - `can_handle` — which requests this service claims. The default matches the
//!   request's resolved route key against this service's signing name. Edit it
//!   for anything the model does not express: a signing name shared by several
//!   services, or routing that keys on the access-key id.
//! - `override_operation` — serve a request the model's routing key cannot name
//!   correctly. The default returns `None`, so every request resolves through the
//!   generated routing table. Return `Some(op)` to route one yourself.
//! - `override_refusal` — how this service words a request the framework refused
//!   before any handler ran. Firehose words those refusals itself: the clause names the
//!   member path and the constraint and never the value that was rejected, a
//!   `@pattern` is reported as the expression a value must satisfy rather than as the
//!   model's anchored string (see `constraint_clause` / `reported_pattern`), and a value
//!   submitted as an element of a list of constrained strings is reported against the list
//!   with that element's whole constraint set, and an enumerated type's value set is
//!   listed in the order the table it is held in is walked, over the order this service's
//!   own set is built in (`enum_value_sets`).
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. The default returns `None`, so the entry
//!   renders the protocol-correct default (query -> 400 InvalidAction, awsJson ->
//!   404, …). Return `Some(resp)` to answer in your own envelope if desired.
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.
//!
//! This file also owns the sentences the HANDLERS state for the rules they enforce
//! themselves (`already_exists_message` and friends), so every operation words the same
//! refusal the same way instead of spelling its own — and the modelled constraints the
//! request boundary cannot enforce (`check_tag_keys` / `check_tag_key_list` /
//! `check_bucket_arns`), so a handler judging one refuses in the words the boundary would
//! have used.
//!
//! WHICH of the shared stated value-set orders this service takes, and the evidence for
//! it, is `enum_value_sets` — which also publishes the order an enumerated type's own set
//! is built in, for the one type this checkout's model declares in another order.

mod enum_value_sets;

use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType, TraitMap};
use aws_smithy_schema_server::constraints::{validate, PatternTrait};
use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service, Violation, ViolationKind,
};
use std::sync::{Arc, LazyLock};

/// The error code this service answers an input-constraint violation with: the
/// `ValidationException` the framework's own renderer names for a request refused at the
/// model's boundary (HTTP 400). Restated here because `override_refusal` replaces the
/// code and the message together.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The error code Firehose answers a value that is not an IAM role ARN with.
///
/// NOT a modelled shape — the service model carries no `InvalidParameterValueException`,
/// and `CreateDeliveryStream`'s modelled error set is `InvalidArgumentException`,
/// `InvalidKMSResourceException`, `LimitExceededException`, `ResourceInUseException`
/// (`smithy_query(query="operation", operation="CreateDeliveryStream")`). Firehose
/// nevertheless answers a role ARN of the wrong SHAPE under this code rather than under
/// the generic constraint error: it validates the role itself, and it is the code a
/// client branches on, so it is what this service answers too.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The `@pattern` a `RoleARN` member carries
/// (`smithy_query(query="shape", shape="RoleARN")`), which is what says a value is being
/// read as an IAM role ARN rather than as any other constrained string. Every `RoleARN`
/// member of every operation targets that one shape, so matching the expression — and not
/// a member path — is what makes the rule below hold wherever a role is taken.
const ROLE_ARN_PATTERN: &str = r"^arn:.*:iam::\d{12}:role/[a-zA-Z_0-9+=,.@\-_/]+$";

/// What Firehose states for a role ARN that does not have the shape of one: the fault is
/// the role, and the sentence names neither the value nor the member, because a role is
/// validated by the service and not by the generic constraint walk.
const INVALID_ROLE_ARN: &str = "Invalid role ARN.";

/// Render one constraint violation the way Firehose states it: the member path and the
/// violated constraint, and NOTHING of the value that was rejected.
///
/// Returns `None` for a violation class this service has no wording for, which lets
/// [`constraint_envelope`] decline the whole refusal and keep the framework's wording
/// rather than invent a sentence for it ([`ViolationKind`] is `#[non_exhaustive]`, so a
/// class can be added upstream).
fn constraint_clause(violation: &Violation) -> Option<String> {
    // An enumerated type's admissible values are stated in the order the table they are
    // held in is walked rather than in the order the schema presented them, over the order
    // the service's own set is built in, so the violation is re-ordered before its
    // requirement is worded (`enum_value_sets`). Only the values move: the member path, the
    // subject and the requirement's wording are built below either way, and every other
    // violation class comes back untouched.
    let violation = &enum_value_sets::as_this_service_states(violation);
    let requirement = constraint_requirement(&violation.kind)?;
    // A value submitted as an ELEMENT of a list of constrained strings has no member name
    // of its own, so it is reported against the LIST it was submitted in, and with that
    // element shape's whole constraint SET rather than the one clause that failed.
    if let Some(member) = tag_key_list_member(&violation.path) {
        return Some(element_clause(member, &tag_key_constraints()));
    }
    // An absent member has no submitted value at all, so it is reported as the null it
    // was: that is a fact of the request's shape, not a rendering of a value the caller
    // sent. Every other violation names the member ONLY — the values this service
    // constrains include record payloads (`Data`, up to 1,024,000 bytes) and tag values,
    // and none of them belongs in a 400 body.
    let subject = match &violation.kind {
        ViolationKind::MissingRequiredMember => "Value null",
        _ => "Value",
    };
    Some(format!(
        "{subject} at '{}' failed to satisfy constraint: {requirement}",
        violation.path
    ))
}

/// The requirement one violated constraint states, with no subject and no member path —
/// the half of a clause that describes the CONSTRAINT.
///
/// `None` for a violation class this service has no wording for, which lets
/// [`constraint_envelope`] decline the whole refusal and keep the framework's wording
/// rather than invent a sentence for it ([`ViolationKind`] is `#[non_exhaustive]`, so a
/// class can be added upstream).
fn constraint_requirement(kind: &ViolationKind) -> Option<String> {
    Some(match kind {
        ViolationKind::MissingRequiredMember => "Member must not be null".to_string(),
        ViolationKind::MinLength(min) => {
            format!("Member must have length greater than or equal to {min}")
        }
        ViolationKind::MaxLength(max) => {
            format!("Member must have length less than or equal to {max}")
        }
        ViolationKind::MinValue(min) => {
            format!("Member must have value greater than or equal to {min}")
        }
        ViolationKind::MaxValue(max) => {
            format!("Member must have value less than or equal to {max}")
        }
        ViolationKind::Pattern(pattern) => format!(
            "Member must satisfy regular expression pattern: {}",
            reported_pattern(pattern)
        ),
        ViolationKind::EnumValue(allowed) => {
            format!("Member must satisfy enum value set: [{}]", allowed.join(", "))
        }
        ViolationKind::UniqueItems => "Member must have unique values".to_string(),
        _ => return None,
    })
}

/// The clause for a value submitted as an element of a list of constrained strings: the
/// LIST member it was submitted in, and every constraint that list's elements carry, in
/// the order a refusal lists them.
///
/// The element shape is what is constrained and the element has no member name, so there
/// is no path below the list to report one constraint at — the whole set is what describes
/// the element the list admits.
fn element_clause(member: &str, constraints: &[String]) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must satisfy constraint: \
         [{}]",
        constraints.join(", ")
    )
}

/// A `@pattern` expression as a constraint message REPORTS it, which is not quite the
/// expression the model spells.
///
/// A reported expression describes a WHOLE value — the clause says what a member must
/// satisfy, not what it must contain somewhere — so the model's anchoring is normalised
/// away and whatever the model left unanchored is spelled out instead:
///
/// * anchored at BOTH ends, the anchors are dropped, so `^[a-zA-Z0-9_.-]+$` is reported as
///   `[a-zA-Z0-9_.-]+`;
/// * anchored at ONE end, the other end admits any run of characters, and the reported
///   expression spells that run out rather than dropping the anchor and describing a
///   narrower language than the constraint enforces: `^https://` is reported as
///   `https://.*`;
/// * anchored at NEITHER end the constraint is satisfied by an occurrence anywhere in the
///   value, which is what the expression already says, so it is reported as spelled;
/// * a TOP-LEVEL alternation cannot lose its anchoring branch by branch without changing
///   the language it accepts, so for one anchored at both ends the anchoring is restated
///   once around the whole group: `^$|\.[0-9a-z]+$` is reported as `^(|\.[0-9a-z]+)$`;
/// * an expression carrying a LOOK-AROUND keeps the anchoring the model spells — the
///   look-around is positioned by the anchor it sits behind, so an expression without it
///   admits values the constraint refuses — except an end anchor standing behind an
///   unbounded run of ANY character, which that run already reaches on its own:
///   `^(?!\s*$).+$` is reported as `^(?!\s*$).+`.
///
/// Every form describes exactly the values the constraint admits, and none of them is the
/// raw model string, which is what a client re-validating with the pattern it read out of
/// a message would otherwise get.
fn reported_pattern(pattern: &str) -> String {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = without_end_anchor(pattern).is_some();
    if carries_look_around(pattern) {
        return match without_end_anchor(pattern) {
            Some(body) if ends_with_any_character_run(body) => body.to_string(),
            _ => pattern.to_string(),
        };
    }
    let body = unanchored(pattern);
    let branches = top_level_alternatives(body);
    match branches.as_slice() {
        [] | [_] => match (anchored_start, anchored_end) {
            // One end anchored: the value continues past the expression, so the run the
            // constraint leaves free is stated rather than left implied.
            (true, false) => format!("{body}{ANY_CHARACTER_RUN}"),
            (false, true) => format!("{ANY_CHARACTER_RUN}{body}"),
            _ => body.to_string(),
        },
        // Anchored at one end only, or not at all: there is no total match to normalise,
        // and grouping it would state a narrower language than the constraint enforces.
        _ if !(anchored_start && anchored_end) => pattern.to_string(),
        branches => format!(
            "^({})$",
            branches
                .iter()
                .map(|branch| unanchored(branch))
                .collect::<Vec<&str>>()
                .join("|")
        ),
    }
}

/// The expression for a run of any characters, which is what an end a model left
/// unanchored admits.
const ANY_CHARACTER_RUN: &str = ".*";

/// Whether `expression` ends in an UNBOUNDED run of any character (`.*` or `.+`). Such a
/// run reaches a value's last character by itself, so an end anchor behind one states
/// nothing the expression does not state already.
fn ends_with_any_character_run(expression: &str) -> bool {
    let Some(atom) = ['*', '+']
        .iter()
        .find_map(|quantifier| expression.strip_suffix(*quantifier))
    else {
        return false;
    };
    atom.ends_with('.') && !tail_is_escaped(atom)
}

/// Whether the last character of `expression` is escaped by a backslash — an escaped dot
/// is a literal dot and not the any-character class.
fn tail_is_escaped(expression: &str) -> bool {
    let Some(last) = expression.chars().next_back() else {
        return false;
    };
    let head = &expression[..expression.len() - last.len_utf8()];
    (head.len() - head.trim_end_matches('\\').len()) % 2 == 1
}

/// A `@pattern` expression without the anchors a model may spell it with.
fn unanchored(pattern: &str) -> &str {
    let body = pattern.strip_prefix('^').unwrap_or(pattern);
    without_end_anchor(body).unwrap_or(body)
}

/// `pattern` without its end anchor, or `None` when it has none. A `$` escaped by a
/// backslash is a literal dollar sign inside the expression, not the end anchor, so it
/// stays.
fn without_end_anchor(pattern: &str) -> Option<&str> {
    let trimmed = pattern.strip_suffix('$')?;
    let backslashes = trimmed.len() - trimmed.trim_end_matches('\\').len();
    (backslashes % 2 == 0).then_some(trimmed)
}

/// Whether `expression` carries a look-around group — `(?=`, `(?!`, `(?<=` or `(?<!`. A
/// `(` that is escaped or inside a character class is a literal parenthesis and opens no
/// group.
fn carries_look_around(expression: &str) -> bool {
    let mut in_class = false;
    let mut escaped = false;
    for (index, character) in expression.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => {
                let group = &expression[index..];
                if ["(?=", "(?!", "(?<=", "(?<!"]
                    .iter()
                    .any(|opener| group.starts_with(opener))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The branches of `expression`'s top-level alternation — the `|`s that are not inside a
/// group, a character class or an escape. A single-branch answer means the expression is
/// not an alternation at all.
fn top_level_alternatives(expression: &str) -> Vec<&str> {
    let mut branches = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut in_class = false;
    let mut escaped = false;
    for (index, character) in expression.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth = depth.saturating_sub(1),
            '|' if !in_class && depth == 0 => {
                branches.push(&expression[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    branches.push(&expression[start..]);
    branches
}

/// The message a refusal states: the AWS envelope (`N validation error(s) detected: `)
/// over one clause per violation.
///
/// `None` declines the whole refusal — see [`constraint_clause`].
fn constraint_envelope(violations: &[Violation]) -> Option<String> {
    let clauses: Vec<String> = violations
        .iter()
        .map(constraint_clause)
        .collect::<Option<Vec<String>>>()?;
    violation_envelope(&clauses)
}

/// The AWS envelope over the clauses of one refusal, counting them the way the envelope
/// states them. `None` when there is no clause at all: nothing was violated, so there is
/// no refusal to word.
fn violation_envelope(clauses: &[String]) -> Option<String> {
    match clauses.len() {
        0 => None,
        1 => Some(format!("1 validation error detected: {}", clauses[0])),
        count => Some(format!(
            "{count} validation errors detected: {}",
            clauses.join("; ")
        )),
    }
}

// ---------------------------------------------------------------------------------
// The modelled constraints the request boundary cannot enforce
//
// The boundary's constraint walk compiles a `@pattern` with the linear-time `regex` crate
// and reads a pattern it cannot compile as satisfied — so a value constrained by an
// expression that engine refuses reaches a handler UNCHECKED. Three of this model's
// expressions are such: two written with a LOOK-AHEAD (which a linear-time engine has no
// form for) and one whose BOUNDED repetition over a character class the engine declines to
// expand. A handler judges such a value itself, by the clause the engine cannot decide and
// the same walk for the rest of the expression, and words the refusal with the renderer
// above so it is the sentence the boundary would have stated for the same violation.
// ---------------------------------------------------------------------------------

/// The `@pattern` of `TagKey`, the shape every member that takes a tag key targets
/// (`smithy_query(query="shape", shape="TagKey")`): the characters a key may be written
/// with, and the reserved prefix a key may not begin with. Held as the model spells it,
/// because that expression is what a refusal reports.
const TAG_KEY_PATTERN: &str = r"^(?!aws:)[\p{L}\p{Z}\p{N}_.:\/=+\-@%]*$";

/// The prefix [`TAG_KEY_PATTERN`]'s look-ahead excludes, which Amazon Web Services keeps
/// for the tags it sets itself. A look-ahead this simple is a prefix test, so the
/// expression is decided WITHOUT a backtracking engine: this clause here, and the rest of
/// it by the walk below.
const RESERVED_TAG_KEY_PREFIX: &str = "aws:";

/// [`TAG_KEY_PATTERN`] without that clause: the characters a tag key may be written with.
const TAG_KEY_CHARACTERS: &str = r"^[\p{L}\p{Z}\p{N}_.:\/=+\-@%]*$";

/// This service's model namespace, for the shadow member schema below. It is only ever a
/// shape id's namespace; nothing reads it off the wire.
const NAMESPACE: &str = "com.amazonaws.firehose";

/// [`TAG_KEY_CHARACTERS`] hung on a member schema, so the decision below is the
/// framework's own constraint walk and the Unicode general categories the expression names
/// mean there exactly what they mean at the request boundary.
///
/// The shadow member is the shape this tree already uses for a `@pattern` a handler has to
/// apply itself (`service-com-amazonaws-amp-amazonprometheusservice/src/state.rs`, for a
/// tag key of its own): the alternative readings each cost fidelity the walk does not —
/// `char` predicates admit a combining mark `\p{L}` refuses, and a second, look-around
/// pattern engine would decide an untrusted value by backtracking where the boundary
/// decides it in linear time.
static TAG_KEY_TRAITS: LazyLock<TraitMap> = LazyLock::new(|| {
    let mut traits = TraitMap::new();
    traits.insert(Box::new(PatternTrait::new(TAG_KEY_CHARACTERS)));
    traits
});

/// The member schema carrying [`TAG_KEY_TRAITS`]. Its name never reaches a message: a
/// refusal names the member the key was SUBMITTED at, which the checks below are given.
static M_TAG_KEY: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.firehose#CheckedMember$tagKey",
        NAMESPACE,
        "tagKey",
    ),
    ShapeType::String,
    "tagKey",
    0,
)
.with_traits(&TAG_KEY_TRAITS);

/// The root of the shadow input the walk is driven over. Its member list is empty on
/// purpose: the walk visits what [`CheckedString`] WRITES, and a root struct schema carries
/// no member name, so it contributes no path segment. The shape id is deliberately one the
/// model does not declare, so it cannot collide with a modelled shape.
static CHECKED_MEMBER_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "com.amazonaws.firehose#CheckedMember",
        NAMESPACE,
        "CheckedMember",
    ),
    ShapeType::Structure,
    &[],
);

/// One string written for the validating walk the way the codec writes a string member,
/// against the member schema whose re-declared constraint the walk is to apply.
struct CheckedString<'a> {
    member: &'static Schema,
    value: &'a str,
}

impl SerializableStruct for CheckedString<'_> {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(self.member, self.value)
    }
}

/// Whether [`TAG_KEY_PATTERN`] admits `key`: it does not begin with the reserved prefix the
/// look-ahead excludes, and its characters are the ones the rest of the expression lists.
fn admits_tag_key(key: &str) -> bool {
    !names_a_system_tag(key) && admits_tag_key_characters(key)
}

/// Whether the character clause of [`TAG_KEY_PATTERN`] admits `key` — the half of the
/// expression that says what a key may be WRITTEN with, judged by the framework's own walk
/// over [`TAG_KEY_CHARACTERS`].
fn admits_tag_key_characters(key: &str) -> bool {
    validate(
        &CheckedString {
            member: &M_TAG_KEY,
            value: key,
        },
        &CHECKED_MEMBER_SCHEMA,
    )
    .is_empty()
}

/// Whether `key` names a SYSTEM tag: one beginning with the prefix
/// [`RESERVED_TAG_KEY_PREFIX`] that Amazon Web Services keeps for the tags it sets itself.
fn names_a_system_tag(key: &str) -> bool {
    key.starts_with(RESERVED_TAG_KEY_PREFIX)
}

/// Why a request's tag keys were refused — the two faults are answered under different
/// codes, so the check that finds one says WHICH it found and the operation answers it
/// through its own error channel.
#[derive(Debug)]
pub(crate) enum TagKeyRefusal {
    /// A key outside the characters the modelled pattern lists: a fault of the request's
    /// SHAPE, worded the way the request boundary words one (HTTP 400
    /// `ValidationException`).
    Shape(String),
    /// A key naming a SYSTEM tag. Not a fault of the request's shape but of what it asks
    /// for — to write or remove a tag Amazon Web Services owns — so it is answered under
    /// the operation's modelled argument error with [`system_tag_message`].
    SystemTag,
}

/// What Firehose states for a request that would write or remove a system tag: the
/// reserved prefix such a key begins with, and that those tags cannot be modified.
///
/// Fixed text over that prefix — the sentence names NO key, however many of the request's
/// keys carry it.
pub fn system_tag_message() -> String {
    format!(
        "Some tag keys in the request started with '{RESERVED_TAG_KEY_PREFIX}'. \
         System tags cannot be modified."
    )
}

/// Refuse every tag key of a request that [`TAG_KEY_PATTERN`] does not admit, each named
/// at the dotted path it was submitted at; `Ok(())` when they all satisfy it.
///
/// For a key submitted at a NAMED member that names no tag to write or remove — a listing
/// start key — where the whole modelled expression, reserved prefix included, is a
/// constraint on the value the request boundary would have applied. A key the request
/// WRITES is judged by [`check_modified_tag_keys`] instead.
pub(crate) fn check_tag_keys<'a, I>(keys: I) -> Result<(), String>
where
    I: IntoIterator<Item = (String, &'a str)>,
{
    check_tag_keys_against(keys, admits_tag_key)
}

/// Refuse the tag keys a request WRITES OR REMOVES, each named at the dotted path it was
/// submitted at; `Ok(())` when the request may carry them all.
///
/// Two rules judge such a key, and they are not the same fault: the characters it is
/// written with are a constraint on the request's shape, and the reserved prefix is the tag
/// policy refusing to let a caller modify a tag Amazon Web Services owns. The shape is
/// judged first — the request boundary would have judged it before any rule of the
/// service's own — and one refusal states every key that violates it.
pub(crate) fn check_modified_tag_keys<'a, I>(keys: I) -> Result<(), TagKeyRefusal>
where
    I: IntoIterator<Item = (String, &'a str)>,
{
    let keys: Vec<(String, &str)> = keys.into_iter().collect();
    check_tag_keys_against(
        keys.iter().map(|(path, key)| (path.clone(), *key)),
        admits_tag_key_characters,
    )
    .map_err(TagKeyRefusal::Shape)?;
    if keys.iter().any(|(_, key)| names_a_system_tag(key)) {
        return Err(TagKeyRefusal::SystemTag);
    }
    Ok(())
}

/// Refuse every tag key `admits` does not admit, each named at the dotted path it was
/// submitted at.
///
/// One refusal states every violating key, the way the boundary walk accumulates
/// violations rather than reporting only the first, and the clause quotes the whole
/// modelled expression whichever of its clauses the key failed — it is that one constraint
/// the key did not satisfy. The key's `@length` bounds and the `TagValue` pattern are
/// separate traits the boundary enforces itself, so they are not re-checked here.
fn check_tag_keys_against<'a, I>(keys: I, admits: fn(&str) -> bool) -> Result<(), String>
where
    I: IntoIterator<Item = (String, &'a str)>,
{
    let clauses: Vec<String> = keys
        .into_iter()
        .filter(|(_, key)| !admits(key))
        .filter_map(|(path, _)| {
            constraint_clause(&Violation {
                path,
                kind: ViolationKind::Pattern(TAG_KEY_PATTERN.to_string()),
                // The rejected key is the value, and this service states no value in a
                // constraint refusal at all — see `constraint_clause`.
                value: None,
                sensitive: false,
            })
        })
        .collect();
    match violation_envelope(&clauses) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// Refuse a LIST of tag keys a request REMOVES, under the two rules
/// [`check_modified_tag_keys`] applies to a named one and in the same order.
///
/// An element has no member name of its own, so a shape refusal names the list ONCE
/// however many of its elements violate the pattern, and states the whole constraint set
/// the list's elements carry — see [`element_clause`].
pub(crate) fn check_modified_tag_key_list(keys: &[String]) -> Result<(), TagKeyRefusal> {
    if !keys.iter().all(|key| admits_tag_key_characters(key)) {
        let clauses = vec![element_clause(&tag_keys_member(), &tag_key_constraints())];
        if let Some(message) = violation_envelope(&clauses) {
            return Err(TagKeyRefusal::Shape(message));
        }
    }
    if keys.iter().any(|key| names_a_system_tag(key)) {
        return Err(TagKeyRefusal::SystemTag);
    }
    Ok(())
}

/// Every constraint the elements of a list of tag keys carry, in the order a refusal
/// lists them: `TagKey`'s `@length(1..128)` and its `@pattern`
/// (`smithy_query(query="shape", shape="TagKey")`).
fn tag_key_constraints() -> Vec<String> {
    [
        ViolationKind::MaxLength(TAG_KEY_MAX_LENGTH),
        ViolationKind::MinLength(TAG_KEY_MIN_LENGTH),
        ViolationKind::Pattern(TAG_KEY_PATTERN.to_string()),
    ]
    .iter()
    .filter_map(constraint_requirement)
    .collect()
}

/// `TagKey`'s modelled `@length` bounds (`smithy_query(query="shape", shape="TagKey")`),
/// which the request boundary enforces and a refusal about an element of a list of tag
/// keys states alongside the pattern.
const TAG_KEY_MIN_LENGTH: i64 = 1;
const TAG_KEY_MAX_LENGTH: i64 = 128;

/// The member a list of tag keys is submitted at, in the casing a dotted path spells it
/// with. `UntagDeliveryStream`'s `TagKeys` is the one list of tag keys this service takes
/// (`smithy_query(query="operation", operation="UntagDeliveryStream")`).
fn tag_keys_member() -> String {
    ::aws_smithy_schema_server::constraints::path::member("TagKeys")
}

/// The list member `path` names an ELEMENT of, when it names an element of a list of tag
/// keys; `None` for every other path.
///
/// A list element's path is the list member, the element's 1-based position and the step
/// into it, and nothing below that — which is exactly what says the constrained value has
/// no member name of its own.
fn tag_key_list_member(path: &str) -> Option<&str> {
    let (head, step) = path.rsplit_once('.')?;
    let (member, position) = head.rsplit_once('.')?;
    let element = ::aws_smithy_schema_server::constraints::path::element(1);
    let (_, collection_step) = element.split_once('.')?;
    (step == collection_step
        && !position.is_empty()
        && position.chars().all(|digit| digit.is_ascii_digit())
        && member == tag_keys_member())
    .then_some(member)
}

/// The `@pattern` of `BucketARN`, the shape every member that takes an Amazon S3
/// destination bucket targets (`smithy_query(query="shape", shape="BucketARN")`). Held as
/// the model spells it, because that expression is what a refusal reports.
///
/// It says a destination bucket is named by an Amazon S3 bucket ARN and by nothing else:
/// an S3 bucket ARN carries neither Region nor account (`arn:<partition>:s3:::<name>`), so
/// a value that is not an ARN, or an ARN of another service, satisfies it nowhere.
const BUCKET_ARN_PATTERN: &str = r"^arn:.*:s3:::[\w\.\-]{1,255}$";

/// [`BUCKET_ARN_PATTERN`] with its one bounded repetition written as an UNBOUNDED one, so
/// the boundary's own engine can compile it, plus [`MAX_BUCKET_ARN_NAME_CHARS`] applied to
/// that run separately. The two together admit exactly the values the model's expression
/// does — the run's character class excludes `:`, so the run a match consumes can only be
/// the one after the last `:s3:::`.
const BUCKET_ARN_FORM: &str = r"^arn:.*:s3:::[\w\.\-]+$";

/// The upper bound [`BUCKET_ARN_PATTERN`]'s repetition states on the bucket-name run.
const MAX_BUCKET_ARN_NAME_CHARS: usize = 255;

/// [`BUCKET_ARN_FORM`] hung on a member schema, for the same reason [`TAG_KEY_TRAITS`] is:
/// the decision is then the framework's own constraint walk rather than a second reading
/// of the expression.
static BUCKET_ARN_TRAITS: LazyLock<TraitMap> = LazyLock::new(|| {
    let mut traits = TraitMap::new();
    traits.insert(Box::new(PatternTrait::new(BUCKET_ARN_FORM)));
    traits
});

/// The member schema carrying [`BUCKET_ARN_TRAITS`]. Its name never reaches a message —
/// see [`M_TAG_KEY`].
static M_BUCKET_ARN: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.firehose#CheckedMember$bucketARN",
        NAMESPACE,
        "bucketARN",
    ),
    ShapeType::String,
    "bucketARN",
    0,
)
.with_traits(&BUCKET_ARN_TRAITS);

/// Whether [`BUCKET_ARN_PATTERN`] admits `bucket_arn`: it has the form of an Amazon S3
/// bucket ARN and the bucket name it carries is within the bound that expression states.
///
/// The name is read by [`crate::state::delivery::bucket_name`], the one reading of a
/// `BucketARN`'s resource part in this crate. Both tests have to hold, so reading the name
/// as the part after the last colon is exact here: the form above admits no colon in the
/// run it bounds.
fn admits_bucket_arn(bucket_arn: &str) -> bool {
    let Some(name) = crate::state::delivery::bucket_name(bucket_arn) else {
        return false;
    };
    name.chars().count() <= MAX_BUCKET_ARN_NAME_CHARS
        && validate(
            &CheckedString {
                member: &M_BUCKET_ARN,
                value: bucket_arn,
            },
            &CHECKED_MEMBER_SCHEMA,
        )
        .is_empty()
}

/// Refuse every destination bucket ARN of a request that [`BUCKET_ARN_PATTERN`] does not
/// admit, each named at the dotted path it was submitted at; `Ok(())` when they all
/// satisfy it.
///
/// One refusal states every violating value, the way the boundary walk accumulates
/// violations rather than reporting only the first, and in the words that walk would have
/// used — this is a modelled constraint the request boundary could not apply, not a rule
/// of this service's own.
pub(crate) fn check_bucket_arns<'a, I>(bucket_arns: I) -> Result<(), String>
where
    I: IntoIterator<Item = (String, &'a str)>,
{
    let clauses: Vec<String> = bucket_arns
        .into_iter()
        .filter(|(_, bucket_arn)| !admits_bucket_arn(bucket_arn))
        .filter_map(|(path, _)| {
            constraint_clause(&Violation {
                path,
                kind: ViolationKind::Pattern(BUCKET_ARN_PATTERN.to_string()),
                // The rejected ARN is the value, and this service states no value in a
                // constraint refusal at all — see `constraint_clause`.
                value: None,
                sensitive: false,
            })
        })
        .collect();
    match violation_envelope(&clauses) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// The `@pattern` of the shapes that admit any value EXCEPT a blank one — `HttpEndpointName`
/// and `HttpEndpointAttributeName` (`smithy_query(query="shape",
/// shape="HttpEndpointName,HttpEndpointAttributeName")`). Held as the model spells it,
/// because that expression is what a refusal reports.
///
/// Its look-ahead says the value is not whitespace to its very end, which is the clause the
/// boundary's engine has no form for; the rest says the value is one or more characters of a
/// single line.
const NON_BLANK_PATTERN: &str = r"^(?!\s*$).+$";

/// [`NON_BLANK_PATTERN`] without that clause: one or more characters, none of them a line
/// break.
const NON_BLANK_CHARACTERS: &str = "^.+$";

/// [`NON_BLANK_CHARACTERS`] hung on a member schema, for the same reason
/// [`TAG_KEY_TRAITS`] is: the decision is then the framework's own constraint walk rather
/// than a second reading of the expression.
static NON_BLANK_TRAITS: LazyLock<TraitMap> = LazyLock::new(|| {
    let mut traits = TraitMap::new();
    traits.insert(Box::new(PatternTrait::new(NON_BLANK_CHARACTERS)));
    traits
});

/// The member schema carrying [`NON_BLANK_TRAITS`]. Its name never reaches a message — see
/// [`M_TAG_KEY`].
static M_NON_BLANK: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.firehose#CheckedMember$nonBlank",
        NAMESPACE,
        "nonBlank",
    ),
    ShapeType::String,
    "nonBlank",
    0,
)
.with_traits(&NON_BLANK_TRAITS);

/// Whether [`NON_BLANK_PATTERN`] admits `value`: it is not whitespace all the way to its
/// end — the clause the look-ahead states, a test over the whole value that needs no
/// backtracking engine — and its characters are what the rest of the expression admits.
fn admits_non_blank(value: &str) -> bool {
    !value.chars().all(char::is_whitespace)
        && validate(
            &CheckedString {
                member: &M_NON_BLANK,
                value,
            },
            &CHECKED_MEMBER_SCHEMA,
        )
        .is_empty()
}

/// Refuse every value of a request that [`NON_BLANK_PATTERN`] does not admit, each named at
/// the dotted path it was submitted at; `Ok(())` when they all satisfy it.
///
/// One refusal states every violating member, the way the boundary walk accumulates
/// violations rather than reporting only the first, and in the words that walk would have
/// used — this is a modelled constraint the request boundary could not apply, not a rule of
/// this service's own. Some of these members are `@sensitive` and the clause states the
/// value on none of them either way (see [`constraint_clause`]).
pub(crate) fn check_non_blank<'a, I>(values: I) -> Result<(), String>
where
    I: IntoIterator<Item = (String, &'a str)>,
{
    let clauses: Vec<String> = values
        .into_iter()
        .filter(|(_, value)| !admits_non_blank(value))
        .filter_map(|(path, _)| {
            constraint_clause(&Violation {
                path,
                kind: ViolationKind::Pattern(NON_BLANK_PATTERN.to_string()),
                value: None,
                sensitive: false,
            })
        })
        .collect();
    match violation_envelope(&clauses) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// Every member of an HTTP endpoint destination that targets a non-blank shape, each at the
/// dotted path it was submitted at: the endpoint's `Name` and the `AttributeName` of every
/// common attribute the request configuration carries
/// (`smithy_query(query="shape", shape="HttpEndpointConfiguration,\
/// HttpEndpointRequestConfiguration,HttpEndpointCommonAttribute")`).
///
/// Shared by the create and the update paths: the two carry the same
/// `HttpEndpointConfiguration` and `HttpEndpointRequestConfiguration` shapes under their
/// own destination member, which is what `destination_member` names.
pub(crate) fn non_blank_http_endpoint_members<'a>(
    destination_member: &str,
    endpoint: Option<&'a crate::ops::types::HttpEndpointConfiguration>,
    request_configuration: Option<&'a crate::ops::types::HttpEndpointRequestConfiguration>,
) -> Vec<(String, &'a str)> {
    use ::aws_smithy_schema_server::constraints::path;

    let mut found: Vec<(String, &str)> = Vec::new();
    if let Some(name) = endpoint.and_then(|endpoint| endpoint.name.as_deref()) {
        found.push((
            path::join(&[
                path::member(destination_member),
                path::member("EndpointConfiguration"),
                path::member("Name"),
            ]),
            name,
        ));
    }
    let attributes = request_configuration
        .and_then(|configuration| configuration.common_attributes.as_deref())
        .unwrap_or_default();
    for (index, attribute) in attributes.iter().enumerate() {
        found.push((
            path::join(&[
                path::member(destination_member),
                path::member("RequestConfiguration"),
                path::member("CommonAttributes"),
                path::element(index + 1),
                path::member("AttributeName"),
            ]),
            attribute.attribute_name.as_str(),
        ));
    }
    found
}

// ---------------------------------------------------------------------------------
// Refusal wording for the rules the HANDLERS enforce
//
// Firehose scopes these sentences to the ACCOUNT whose namespace the stream names live
// in: a stream name is unique per account per Region, so the account is what tells two
// collisions apart, and a lookup that found nothing reports only the account it was
// scoped to — never the name that was probed, which would answer "does this stream
// exist?" for a caller enumerating names. The account is the emulator's canonical one
// (`engine::emulator_account_id`), never a literal.
//
// They live beside the refusal rendering above so every operation words the same
// refusal the same way; a handler calls them rather than spelling a sentence itself.
// ---------------------------------------------------------------------------------

/// A create whose name is already taken: the requested name and the account whose
/// namespace it is taken in.
pub fn already_exists_message(name: &str) -> String {
    format!(
        "Firehose {name} under accountId {} already exists",
        ::engine::emulator_account_id()
    )
}

/// A control-plane read, delete, tag change or destination update that resolved no
/// stream: the account the lookup was scoped to, and not the name it was given.
pub fn not_found_message() -> String {
    format!(
        "Firehose under account {} not found.",
        ::engine::emulator_account_id()
    )
}

/// An ENCRYPTION CHANGE that resolved no stream. The same fact as
/// [`not_found_message`] and the same scoping rule — the account, never the probed name —
/// in the wording the encryption path states it in: the noun is `accountId` and the
/// sentence carries no terminal period.
///
/// Firehose does not word this fact identically across its surface, so each path keeps
/// its own template ([`not_found_message`] for the control-plane reads, delete, tag
/// changes and destination update, [`put_not_found_message`] for the data plane) and
/// none of them stands in for another.
pub fn encryption_not_found_message() -> String {
    format!(
        "Firehose under accountId {} not found",
        ::engine::emulator_account_id()
    )
}

/// Which way an encryption change goes, so the two refusals below are stated for the call
/// that was made rather than for one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionChange {
    /// `StartDeliveryStreamEncryption`.
    Enable,
    /// `StopDeliveryStreamEncryption`.
    Disable,
}

impl EncryptionChange {
    /// The verb the refusal names this change with.
    fn verb(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

/// An encryption change the STREAM's own state blocks: the stream and the status it is in,
/// stated for whatever that status is.
pub fn encryption_blocked_by_stream_message(
    change: EncryptionChange,
    name: &str,
    status: &str,
) -> String {
    format!(
        "Cannot {} encryption on firehose: {name} when it is in {status} status",
        change.verb()
    )
}

/// An encryption change the stream's ENCRYPTION state blocks: the stream and the
/// encryption status that blocks it.
pub fn encryption_blocked_by_encryption_message(
    change: EncryptionChange,
    name: &str,
    status: &str,
) -> String {
    format!(
        "Cannot {} encryption on firehose: {name} when encryption is in {status} status",
        change.verb()
    )
}

/// A disable with nothing to disable: the stream, and the state its encryption is ALREADY
/// in, which is the state the call asks for.
///
/// Stated as a fact about the stream rather than as a status this call is waiting on
/// ([`encryption_blocked_by_encryption_message`]), because no change is in flight: the
/// stream simply has no encryption in force to turn off, whichever way it came to have
/// none.
pub fn encryption_already_disabled_message(name: &str) -> String {
    format!("Cannot disable encryption on firehose: {name} when encryption is already disabled")
}

/// An update the stream's current state blocks: the stream and the status it is in.
pub fn update_blocked_message(name: &str, status: &str) -> String {
    format!("Cannot update firehose: {name} when it is in {status} status")
}

/// A conditional update whose `CurrentDeliveryStreamVersionId` is not the stream's: both
/// version ids, so a client can see which revision it was working from.
pub fn version_mismatch_message(name: &str, current: &str, specified: &str) -> String {
    format!(
        "Cannot update firehose: {name} since the current version id: {current} and \
         specified version id: {specified} do not match"
    )
}

/// An update naming a destination id the stream does not have: the id the request named.
pub fn destination_not_found_message(destination_id: &str) -> String {
    format!("Destination Id {destination_id} not found")
}

/// A switch into or out of the Amazon OpenSearch Service destination types, which Firehose
/// does not support ("Switching between Amazon OpenSearch Service and other services is not
/// supported" — `UpdateDestination`).
pub const UNSUPPORTED_SEARCH_TYPE_CHANGE: &str =
    "Changing the destination type to/from Elasticsearch/Amazonopensearchservice/\
     AmazonOpenSearchServerless from/to other destinations is not supported at this time.";

/// A switch into or out of the HTTP endpoint destination type, which Firehose does not
/// support either — however many of the new type's parameters the update carries.
pub const UNSUPPORTED_HTTP_ENDPOINT_TYPE_CHANGE: &str =
    "Changing the destination type to or from HttpEndpoint is not supported at this time.";

/// A create of a stream type whose source configuration is absent: the configuration that
/// type is fed by, and the type that requires it.
pub fn mandatory_source_configuration_message(configuration: &str, stream_type: &str) -> String {
    format!("{configuration} is mandatory for {stream_type} stream type.")
}

/// A data processor that omits a parameter its type cannot run without: the parameter, and
/// the processor type that requires it.
///
/// Stated as a requirement of the processor TYPE the request chose, because that is what
/// makes a parameter the model leaves optional in general mandatory here.
pub fn mandatory_processor_parameter_message(parameter: &str, processor_type: &str) -> String {
    format!("{parameter} is required when {processor_type} processor is used.")
}

/// A put that resolved no stream. Same rule as [`not_found_message`] — the account, not
/// the probed name — in the wording the data plane uses.
pub fn put_not_found_message() -> String {
    format!(
        "Firehose not found under account {}.",
        ::engine::emulator_account_id()
    )
}

/// A put refused because the stream takes data from CloudWatch Logs only: the account the
/// put was scoped to and the stream it named.
///
/// This one DOES name the stream, unlike [`put_not_found_message`]: the refusal is a
/// property of a stream the caller already named successfully, so the sentence answers
/// "does this stream exist?" for nobody who did not know already.
pub fn invalid_source_message(name: &str) -> String {
    format!(
        "Put to Firehose failed for AccountId: {}, FirehoseName: {name} \
         because the request is not originating from CloudWatch.",
        ::engine::emulator_account_id()
    )
}

/// A put refused because the stream's data comes from a configured source rather than
/// from direct puts: the `DeliveryStreamType` that says so.
///
/// The sentence names the stream's TYPE and not its name, so it is stated for whichever
/// non-direct-put type the stream carries rather than asserting one of them, and it
/// discloses no name in this account's namespace.
pub fn not_direct_put_message(stream_type: &str) -> String {
    format!("This operation is not permitted on {stream_type} delivery stream type.")
}

/// A batch put whose records together exceed the per-request ceiling: the ceiling, in the
/// megabytes the sentence states it in.
///
/// The fault is the aggregate SIZE of the records and not any one of them — each may be
/// legal on its own — so the sentence names neither a record nor a count, and the whole
/// request is refused.
pub fn records_too_large_message(limit_bytes: usize) -> String {
    /// A megabyte, as both the limit and the modelled per-record bound count it.
    const MEGABYTE: usize = 1024 * 1024;
    format!("Records size exceeds {} MB limit", limit_bytes / MEGABYTE)
}

/// A put refused because the stream it named is not accepting data: the stream the
/// request named and the account the put was scoped to.
///
/// Like [`invalid_source_message`], this one DOES name the stream: the refusal is about a
/// stream the caller already named successfully, so the sentence discloses nothing a
/// caller enumerating names could not learn from the lookup itself. The state the stream
/// is in is not part of the sentence — the data plane states that the stream is not
/// active, whichever non-`ACTIVE` state that is.
pub fn not_active_message(name: &str) -> String {
    format!(
        "Firehose {name} is not active in {} account.",
        ::engine::emulator_account_id()
    )
}

/// A delete the stream's current state blocks: the name, the account and the state that
/// blocks it, stated for whatever that state is rather than asserting a precondition of
/// its own.
pub fn blocked_delete_message(name: &str, state: &str) -> String {
    format!(
        "Firehose {name} under account {} cannot be deleted in {state} state",
        ::engine::emulator_account_id()
    )
}

/// A request that does not name EXACTLY ONE destination configuration — none, or several
/// that exclude each other.
///
/// "A Firehose stream is configured with a single destination", and "You must specify only
/// one of the following destination configuration parameters", so both faults are the one
/// arity rule and the refusal states it as one fixed sentence. It names no member either
/// way: the sentence is the same whichever destination the caller meant to configure and
/// whichever ones it supplied, so a client matching the refusal matches one text for the
/// rule rather than one per combination.
pub fn sole_destination_message() -> String {
    "Exactly one destination configuration is supported for a Firehose".to_string()
}

/// Whether a boundary refusal is ABOUT a role ARN: one of its violations is a `@pattern`
/// violation of the `RoleARN` expression.
///
/// Keyed to the expression the model constrains every `RoleARN` member with, so the rule
/// holds for whichever member of whichever operation carried the value — the destination
/// role of a create, the source role, a VPC or secret role, the role of an update — and
/// never to a path spelled out here. A refusal that ALSO carries other violations is
/// still answered as the role fault: Firehose validates the role itself, so that is the
/// fault it reports.
fn refused_role_arn(violations: &[Violation]) -> bool {
    violations.iter().any(|violation| {
        matches!(&violation.kind, ViolationKind::Pattern(pattern) if pattern == ROLE_ARN_PATTERN)
    })
}

/// A role Firehose could not assume: the ARN the request named, and what to do about it.
///
/// The role ARN is the caller's own value and the sentence is built from it, which is why
/// it is stated — unlike a stream name, a role ARN is not a name in this account's
/// namespace that a refusal could disclose by echoing.
pub fn unassumable_role_message(role_arn: &str) -> String {
    format!("Firehose is unable to assume role {role_arn}. Please check the role provided.")
}

/// An Amazon S3 bucket a destination configuration names that Amazon S3 does not hold: the
/// bucket the `BucketARN` named, stated as the refusal states it.
///
/// Beside [`unassumable_role_message`] for the same reason: the sentence a destination's
/// unresolvable resource is refused with belongs with this service's other refusal wording,
/// not inside one operation. The bucket NAME is the part the sentence carries — the name is
/// the caller's own value, and a bucket name is not a name in this account's namespace a
/// refusal could disclose by echoing.
pub fn absent_bucket_message(bucket: &str) -> String {
    format!("S3 Bucket {bucket} does not exist.")
}

/// A destination table of an Apache Iceberg destination that Firehose could not read out of
/// the Data Catalog: the role it would have read it with, stated as the refusal states it.
///
/// Beside [`absent_bucket_message`] for the same reason, and the same shape of sentence: the
/// resource a destination names is resolved before the configuration is written, and the
/// refusal names the call Firehose makes for it. Firehose reads the table with the
/// configuration's role and states one refusal for both halves of that answer — a role that
/// may not make the call and a table the catalog does not hold — because the caller learns the
/// same thing from either. The ROLE is the only variable part: it is the caller's own value,
/// and the table is not named at all, so the sentence discloses nothing about what this
/// account's catalog does hold.
pub fn unreadable_glue_table_message(role_arn: &str) -> String {
    format!(
        "Role {role_arn} is not authorized to perform: glue:GetTable for the given table or \
         the table does not exist."
    )
}

/// The `com-amazonaws-firehose-firehose-20150804` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(engine),
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry.
        &self.entry
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // Claim a request whose resolved route key is this service's signing
        // name. Edit this for anything the model does not express — a signing
        // name shared by several services, or access-key-id-based routing.
        match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) => key == self.entry.identity().signing_name(),
            None => false,
        }
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // Return `Some(op)` to serve a request the model's routing key cannot name
        // correctly; `None` (the default) lets the generated routing table resolve
        // it. `req` is the whole request, so a decision that depends on the body —
        // the `Action` form field on a query protocol, say — is reachable here.
        let _ = req;
        None
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this service
        // reword it.
        //
        // Firehose states a constraint violation WITHOUT the value that violated it: the
        // message names the member path and the constraint only, and reports a `@pattern`
        // as the expression a value must satisfy rather than as the model's anchored
        // string. The code stays the `ValidationException` (HTTP 400) the framework would
        // have answered; only the wording changes.
        //
        // A role ARN is the exception: Firehose validates a `RoleARN` member itself, so a
        // value that is not shaped like an IAM role ARN is answered under the service's
        // own parameter-value code and sentence, and the generic constraint walk is not
        // the terminal responder for it ([`refused_role_arn`]).
        //
        // An authentication refusal keeps the framework's wording: it is not an
        // input-constraint failure.
        match refusal {
            Refusal::Invalid(violations) if refused_role_arn(violations) => Some(Refused {
                code: INVALID_PARAMETER_VALUE.to_string(),
                message: INVALID_ROLE_ARN.to_string(),
            }),
            Refusal::Invalid(violations) => Some(Refused {
                code: VALIDATION_EXCEPTION.to_string(),
                message: constraint_envelope(violations)?,
            }),
            _ => None,
        }
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501). Return `Some(resp)` to
        // word that routing failure in your own envelope instead.
        let _ = req;
        None
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. Returning `None` keeps the entry's protocol-correct
        // rendering. Return `Some(resp)` to reword that failure in your own
        // envelope. The error is borrowed, so the default renderer can still
        // consume it on `None`.
        let _ = error;
        None
    }
}
