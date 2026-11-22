// SPDX-License-Identifier: CC0-1.0

//! WHICH constraints a clause of this service is stated over, and HOW the expression a
//! `@pattern` requirement quotes is rendered.
//!
//! [`super`] owns the envelope and the seams a refusal reaches it through; this module owns
//! the two things about a clause that are neither the framework's nor readable off this
//! checkout's model.
//!
//! ## 1. The expression a `@pattern` clause quotes
//!
//! A clause states the expression the service PUBLISHES for the member's shape. A Smithy
//! `@pattern` is a FIND, so a model expresses a whole-value match by writing the published
//! expression inside a `^`…`$` pair, and recovering what is published is the textual inverse
//! of that wrapping — `service_common::validation::unanchored`, the tree's one reading of
//! that pair. For the expressions this service publishes exactly as the model declares them
//! ([`PUBLISHED_AS_DECLARED`]) the model's text is quoted verbatim: which of the two forms a
//! shape is published with cannot be read off the text, because both are spelled `^…$` in
//! the model, so it is reference data about the SHAPE, keyed by the expression — what is
//! published belongs to the shape, so one row holds for every member targeting it, in any
//! operation, and inside a constraint set stated at a collection of them. The CHECK always
//! uses the model's pattern as written, here as in the framework's own walk; only the
//! rendering differs.
//!
//! ## 2. The set a member's clauses are stated over
//!
//! The live service validates a value against the constraint set ITS model declares for the
//! member's shape, and states one clause per constraint of that set the value broke, in the
//! order that set declares them. For the shapes in [`STATED_SHAPES`] that set is larger than
//! this checkout's model declares — an id this model bounds only by its length is a UUID to
//! the service, and a node id this model bounds only by its expression has a length band too
//! — so a request this emulator refused for one constraint was refused for fewer reasons
//! than the service states. The rows carry that set, and [`restating_the_stated_sets`]
//! states it over the values the request actually carried, so a value breaking a constraint
//! this model does not declare is refused as well.
//!
//! A row is keyed by the constraints the model DECLARES for the shape ([`Declared`]), read
//! off the member's own schema, so it holds for every member of every operation that targets
//! that shape and for no member that targets another.
//!
//! ## 3. Where a constraint broken by a collection's elements is stated
//!
//! The framework reports such a violation at the offending ELEMENT, naming only the
//! constraint that failed; AWS states it once at the COLLECTION MEMBER with the element
//! shape's whole set nested inside `Member must satisfy constraint: [ … ]`
//! (`service_common::validation_message::element_set_requirement`). [`element_requirement`]
//! is that set: a row's where one holds for that collection ([`STATED_ELEMENTS`], keyed by
//! the member path with its element steps folded away, [`folded`]), else the set the MODEL
//! declares for that member's elements, read off the operation's own input schema
//! (`service_common::validation_message::element_constraint_set`) — so the grouping holds
//! for every collection of constrained scalars this service models and not only for the
//! members a row names.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use ::aws_smithy_schema::{Schema, ShapeId, TraitMap};
use ::aws_smithy_schema_server::constraints::{
    path, LengthTrait, PatternTrait, RangeTrait, Violation, ViolationKind,
};
use ::service_common::validation::{quoted_echo, unanchored};
use ::service_common::validation_message::{
    element_constraint_set, element_set_requirement, requirement,
};

/// `com.amazonaws.ssm#OpsItemId`'s expression, which `OpsItemEventFilterValue` carries too
/// (`smithy_query(query="shape",shape="OpsItemId,OpsItemEventFilterValues")`).
const OPS_ITEM_ID_PATTERN: &str = "^(oi)-[0-9a-f]{12}$";

/// The UUID a `CommandId` is. The shape this checkout's model declares carries a `@length`
/// of exactly 36 and no `@pattern` at all
/// (`smithy_query(query="shape",shape="CommandId")`), while the live service also requires
/// the hyphenated hexadecimal form — which is the form this emulator itself mints
/// (`crate::state::ComAmazonawsSsmAmazonssmState::mint_command_id`), so no id it hands a
/// caller is refused by it.
const COMMAND_ID_PATTERN: &str =
    "^[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}$";

/// `com.amazonaws.ssm#InstanceId`'s expression as the model declares it
/// (`smithy_query(query="shape",shape="InstanceId")`). What a clause QUOTES for it is the
/// alternation inside the pair the model wraps it in, which this crate states once as
/// `crate::state::MANAGED_NODE_ID_PATTERN` — the expression a node id read out of a filter is
/// refused with too — and which [`quoted_pattern`] renders from the text below.
const INSTANCE_ID_PATTERN: &str = r"^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$";

/// `com.amazonaws.ssm#DocumentVersion`'s expression as the model declares it
/// (`smithy_query(query="shape",shape="DocumentVersion")`).
const DOCUMENT_VERSION_PATTERN: &str = r"^([$]LATEST|[$]DEFAULT|^[1-9][0-9]*$)$";

/// `com.amazonaws.ssm#InventoryItemTypeName`'s expression, which this service publishes as
/// the model declares it (`smithy_query(query="shape",shape="InventoryItemTypeName")`).
const INVENTORY_ITEM_TYPE_NAME_PATTERN: &str = "^(AWS|Custom):.*$";

/// The name a document is addressed by, as the live service publishes it for the execution
/// preview: wider than the shape this checkout's model declares
/// (`smithy_query(query="shape",shape="DocumentName")` is `^[a-zA-Z0-9_\-.]{3,128}$`), since
/// the service admits the ARN punctuation and a longer name, and published as declared.
const PREVIEW_DOCUMENT_NAME_PATTERN: &str = r"^[a-zA-Z0-9_\-.:/]{3,200}$";

/// `com.amazonaws.ssm#DocumentName`'s expression as the model declares it
/// (`smithy_query(query="shape",shape="DocumentName")`).
const DOCUMENT_NAME_PATTERN: &str = r"^[a-zA-Z0-9_\-.]{3,128}$";

/// The run of characters a NAME a caller chooses is held to, as the model declares it
/// (`smithy_query(query="shape",shape="PatchGroup")`; `ActivationDefaultInstanceName`
/// carries the same text), and as this service publishes it: a refusal of a patch group
/// outside it quotes the expression with the pair, so the sentence states the whole-value
/// match it was refused for rather than a run a rejected value contains.
const CHOSEN_NAME_PATTERN: &str = r"^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$";

/// The expressions this service publishes exactly as the model declares them, the outer
/// anchors included — every other expression is published without the whole-value pair the
/// model wrote around it ([`quoted_pattern`]).
///
/// Keyed by the EXPRESSION rather than by a member, because what a service publishes belongs
/// to the shape: the same text is quoted the same way wherever that shape is carried.
const PUBLISHED_AS_DECLARED: &[&str] = &[
    OPS_ITEM_ID_PATTERN,
    COMMAND_ID_PATTERN,
    INVENTORY_ITEM_TYPE_NAME_PATTERN,
    PREVIEW_DOCUMENT_NAME_PATTERN,
    CHOSEN_NAME_PATTERN,
];

/// One constraint of a shape's stated set.
///
/// The classes the framework's own walk reports, so a clause built here carries the same
/// requirement sentence as one the walk reported
/// (`service_common::validation_message::requirement` over [`ViolationKind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Constraint {
    /// The greatest length a value may have.
    MaxLength(i64),
    /// The least length a value may have.
    MinLength(i64),
    /// The greatest value a number may have.
    MaxValue(i64),
    /// The least value a number may have.
    MinValue(i64),
    /// The expression a value has to satisfy, as the model writes it where the model
    /// declares one and as the service publishes it where it does not.
    Pattern(&'static str),
    /// The value has to be there at all — what a set stated at a collection says about an
    /// element of a list that is not `@sparse`.
    Present,
}

impl Constraint {
    /// The violation this constraint is reported as, with a `@pattern` already quoted the
    /// way this service publishes it.
    fn kind(self) -> ViolationKind {
        match self {
            Self::MaxLength(max) => ViolationKind::MaxLength(max),
            Self::MinLength(min) => ViolationKind::MinLength(min),
            Self::MaxValue(max) => ViolationKind::MaxValue(max.to_string()),
            Self::MinValue(min) => ViolationKind::MinValue(min.to_string()),
            Self::Pattern(regex) => ViolationKind::Pattern(quoted_pattern(regex)),
            Self::Present => ViolationKind::MissingRequiredMember,
        }
    }

    /// Whether `value` breaks this constraint.
    ///
    /// A `@length` counts CHARACTERS and a `@pattern` is applied as a find, the way the
    /// framework's own walk applies both, so a constraint this module shares with the model
    /// is decided identically whichever layer sees it. A `@range` bound is read off the
    /// value's own digits, and a value that is not a number breaks no numeric bound: its
    /// member is one the codec already refused. A value the request carried is PRESENT by
    /// construction, so that constraint is never broken here.
    fn broken_by(self, value: &str) -> bool {
        match self {
            Self::MaxLength(max) => characters(value) > max,
            Self::MinLength(min) => characters(value) < min,
            Self::MaxValue(max) => value.parse::<i64>().is_ok_and(|number| number > max),
            Self::MinValue(min) => value.parse::<i64>().is_ok_and(|number| number < min),
            Self::Pattern(regex) => !satisfies(regex, value),
            Self::Present => false,
        }
    }
}

/// How many characters `value` holds, as a `@length` counts one.
fn characters(value: &str) -> i64 {
    i64::try_from(value.chars().count()).unwrap_or(i64::MAX)
}

/// The constraints this checkout's MODEL declares for a shape: the SIGNATURE a member
/// targeting it is recognised by.
///
/// A row is keyed by it rather than by a member name because the set belongs to the SHAPE: a
/// member schema carries the constraints of the shape it targets (that is what the
/// framework's own walk validates against) and nothing of that shape's name, so the
/// constraints are what identifies it — and one row then holds for every member targeting
/// that shape. A member name would not: this model spells `instanceId` over two different
/// shapes, one of which is a hybrid-node id with a length band and an expression of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Declared {
    /// The `@pattern` the model declares, if it declares one.
    pattern: Option<&'static str>,
    /// `@length`'s minimum, if the model declares one.
    min_length: Option<i64>,
    /// `@length`'s maximum, if the model declares one.
    max_length: Option<i64>,
    /// `@range`'s minimum, if the model declares one.
    min_value: Option<i64>,
    /// `@range`'s maximum, if the model declares one.
    max_value: Option<i64>,
}

/// One SHAPE whose values the live service constrains beyond what this checkout's model
/// declares, with the constraints in the order its clauses state them.
#[derive(Debug)]
struct StatedShape {
    /// The shapes the constraints belong to — what the row is ABOUT, and what the tests
    /// below read the model for. More than one where this model declares the SAME id under
    /// two names, in which case the signature is the same for both and so is the set the
    /// service enforces.
    ///
    /// Read by the tests that hold the rows to the model rather than by the lookup, which
    /// recognises a member by what the model declares for it: the shape's NAME is nowhere in
    /// the runtime schemas, and a row that named no shape could not be checked against the
    /// model at all.
    #[cfg_attr(not(test), allow(dead_code))]
    shapes: &'static [&'static str],
    /// What this checkout's model declares for them, which a member carrying one is
    /// recognised by.
    declared: Declared,
    /// The member names this row holds at, or EMPTY when the signature alone names the
    /// shapes. Named only where OTHER shapes of this model declare the same constraints
    /// without being the same id — three shapes are bounded to exactly 36 characters and
    /// nothing else — in which case the pair (member name, signature) is what identifies
    /// the shape, and the test below pins that it does.
    members: &'static [&'static str],
    /// The operations this row holds for, or EMPTY for every operation carrying such a
    /// member. Named where what the service enforces is a property of the PARAMETER rather
    /// than of the shape alone — an API reference publishes a `Pattern` and a `Valid Range`
    /// per operation parameter, and one operation's can be wider or narrower than the shape
    /// this checkout's model gives every member targeting it — in which case every other
    /// operation keeps the model's own constraints.
    operations: &'static [&'static str],
    /// The constraints the live service enforces, in the order its clauses state them.
    constraints: &'static [Constraint],
}

/// One COLLECTION member whose ELEMENTS' constraint set this service states itself, rather
/// than leaving it to be read off the model.
///
/// Keyed by the member PATH, because the elements of a collection nested inside a structure
/// are reachable from no schema a refusal has in hand (a member schema carries nothing of
/// its target shape's members), and a path is the one thing every violation carries.
#[derive(Debug)]
struct StatedElements {
    /// The operations this row holds for, or EMPTY for every operation carrying the member —
    /// a row names them only where another operation spells the same path over another
    /// element shape.
    operations: &'static [&'static str],
    /// The collection member as a violation path names it, with every step into an element
    /// folded away ([`folded`]).
    collection: &'static str,
    /// The element shape the constraints belong to, read by the test that holds the rows to
    /// the model (see [`StatedShape::shapes`]).
    #[cfg_attr(not(test), allow(dead_code))]
    shape: &'static str,
    /// The element shape's whole set, in the order the nested list states it.
    constraints: &'static [Constraint],
}

/// `com.amazonaws.ssm#CommandId`: the modelled exact length, and the UUID the live service
/// requires, stated before it (recorded behaviour — fixtures
/// `ssm-ecsm-run-command-on-an-unmanaged-instance-refusals` and
/// `ssm-vscan-traj-send-command-validation-order`, where a `commandId` that is neither
/// refuses the expression first and the length second).
const COMMAND_ID: &[Constraint] = &[
    Constraint::Pattern(COMMAND_ID_PATTERN),
    Constraint::MaxLength(36),
    Constraint::MinLength(36),
];

/// `com.amazonaws.ssm#InstanceId`: the length band the live service holds a node id to, then
/// the expression the model declares (recorded behaviour — fixtures
/// `ssm-vscan-traj-inventory-entries-type-and-id-mistakes` and
/// `ssm-vscan-traj-patch-and-compliance-read-mistakes`, where a short `instanceId` refuses
/// the length first and the expression second).
///
/// The band is this crate's one statement of it (`crate::state::MANAGED_NODE_ID_MIN_LENGTH` /
/// `MANAGED_NODE_ID_MAX_LENGTH`, where the same recordings are cited), so a node id read out
/// of a FILTER and one read at an input member are held to the same bounds. It admits every
/// id the expression does — `i-` with 8 or 17 word characters is 10 or 19, `mi-` with 17 is
/// 20 — so no id this service mints or accepts is refused by it.
const INSTANCE_ID: &[Constraint] = &[
    Constraint::MaxLength(crate::state::MANAGED_NODE_ID_MAX_LENGTH),
    Constraint::MinLength(crate::state::MANAGED_NODE_ID_MIN_LENGTH),
    Constraint::Pattern(INSTANCE_ID_PATTERN),
];

/// `com.amazonaws.ssm#InstanceId` as an ELEMENT of `InstanceIdList`: the shape's own set,
/// and the presence every element of that list has to have (recorded behaviour — fixtures
/// `ssm-ecsm-run-command-on-an-unmanaged-instance-refusals` and
/// `ssm-vscan-traj-patch-and-compliance-read-mistakes`, where the whole set is stated at the
/// `instanceIds` member).
const INSTANCE_ID_ELEMENT: &[Constraint] = &[
    Constraint::MaxLength(crate::state::MANAGED_NODE_ID_MAX_LENGTH),
    Constraint::MinLength(crate::state::MANAGED_NODE_ID_MIN_LENGTH),
    Constraint::Pattern(INSTANCE_ID_PATTERN),
    Constraint::Present,
];

/// `com.amazonaws.ssm#DocumentVersion`: the expression the model declares, then the length
/// the live service holds a version to (recorded behaviour — fixtures
/// `ssm-execution-preview-automation-workflow` and
/// `ssm-execution-preview-invalid-document-error`, where a `documentVersion` that is neither
/// `$LATEST`, `$DEFAULT` nor a number refuses the expression first and the length second).
///
/// The length admits every version the expression does but a number of nine digits or more.
const DOCUMENT_VERSION: &[Constraint] = &[
    Constraint::Pattern(DOCUMENT_VERSION_PATTERN),
    Constraint::MaxLength(8),
];

/// `com.amazonaws.ssm#OpsItemEventFilterValue`: the set the model declares for the elements
/// of an OpsItem event filter's values, stated at the values member (recorded behaviour —
/// fixtures `ssm-ops-item-events-error-paths` and `ssm-opsitem-read-apis-on-unknown-id`,
/// where the clause states no value at all).
const OPS_ITEM_EVENT_FILTER_VALUE: &[Constraint] = &[
    Constraint::MaxLength(15),
    Constraint::MinLength(1),
    Constraint::Pattern(OPS_ITEM_ID_PATTERN),
];

/// The page size a patch-compliance read serves at most, which the live service holds below
/// the maximum this checkout's model declares (recorded behaviour — fixture
/// `ssm-vscan-traj-patch-and-compliance-read-mistakes` step `task#4`, where
/// `DescribeInstancePatches` with a `MaxResults` of 101 is refused with a ceiling of 50).
/// The floor is the model's own, so it is stated exactly where the model would state it.
const PATCH_COMPLIANCE_PAGE_SIZE: &[Constraint] =
    &[Constraint::MaxValue(50), Constraint::MinValue(10)];

/// The document name the execution preview admits: the expression the live service
/// publishes for that parameter, which is wider than the shape this checkout's model gives
/// it (recorded behaviour — fixture `ssm-execution-preview-invalid-document-error` step
/// `task#1`).
const PREVIEW_DOCUMENT_NAME: &[Constraint] = &[Constraint::Pattern(PREVIEW_DOCUMENT_NAME_PATTERN)];

/// Every SHAPE of this service whose stated constraint set is its own.
const STATED_SHAPES: &[StatedShape] = &[
    StatedShape {
        shapes: &["com.amazonaws.ssm#CommandId"],
        declared: Declared {
            pattern: None,
            min_length: Some(36),
            max_length: Some(36),
            min_value: None,
            max_value: None,
        },
        members: &["commandId"],
        operations: &[],
        constraints: COMMAND_ID,
    },
    StatedShape {
        // `NodeId` is the same managed-node id under a second name: it declares the same
        // expression, so the band the service holds one to is the same band.
        shapes: &["com.amazonaws.ssm#InstanceId", "com.amazonaws.ssm#NodeId"],
        declared: Declared {
            pattern: Some(INSTANCE_ID_PATTERN),
            min_length: None,
            max_length: None,
            min_value: None,
            max_value: None,
        },
        members: &[],
        operations: &[],
        constraints: INSTANCE_ID,
    },
    StatedShape {
        shapes: &["com.amazonaws.ssm#DocumentVersion"],
        declared: Declared {
            pattern: Some(DOCUMENT_VERSION_PATTERN),
            min_length: None,
            max_length: None,
            min_value: None,
            max_value: None,
        },
        members: &[],
        operations: &[],
        constraints: DOCUMENT_VERSION,
    },
    StatedShape {
        shapes: &["com.amazonaws.ssm#DocumentName"],
        declared: Declared {
            pattern: Some(DOCUMENT_NAME_PATTERN),
            min_length: None,
            max_length: None,
            min_value: None,
            max_value: None,
        },
        members: &["documentName"],
        operations: &["StartExecutionPreview"],
        constraints: PREVIEW_DOCUMENT_NAME,
    },
    StatedShape {
        shapes: &["com.amazonaws.ssm#PatchComplianceMaxResults"],
        declared: Declared {
            pattern: None,
            min_length: None,
            max_length: None,
            min_value: Some(10),
            max_value: Some(100),
        },
        members: &["maxResults"],
        operations: &["DescribeInstancePatches"],
        constraints: PATCH_COMPLIANCE_PAGE_SIZE,
    },
];

/// Every COLLECTION member of this service whose element set a clause states itself.
const STATED_ELEMENTS: &[StatedElements] = &[
    StatedElements {
        operations: &[],
        collection: "instanceIds",
        shape: "com.amazonaws.ssm#InstanceId",
        constraints: INSTANCE_ID_ELEMENT,
    },
    StatedElements {
        // `OpsItemEventFilter$Values`. The sibling OpsItem reads spell `values` under a
        // `filters` member of their own over ANOTHER filter shape, so this row names the
        // operation whose filters carry these elements.
        operations: &["ListOpsItemEvents"],
        collection: "filters.member.values",
        shape: "com.amazonaws.ssm#OpsItemEventFilterValue",
        constraints: OPS_ITEM_EVENT_FILTER_VALUE,
    },
];

/// Every expression a row states, compiled once.
///
/// The expressions are MODEL (and service) CONSTANTS, never built from a request, so the set
/// is known at startup and one compile serves every request. An expression this engine
/// cannot compile is absent, which leaves it SATISFIED — the framework's own treatment of
/// one (`vendor/smithy-schema-server/src/constraints.rs`), so no value is refused for an
/// expression nothing evaluated.
static COMPILED: LazyLock<HashMap<&'static str, ::regex::Regex>> = LazyLock::new(|| {
    let stated = STATED_SHAPES
        .iter()
        .flat_map(|row| row.constraints.iter())
        .chain(STATED_ELEMENTS.iter().flat_map(|row| row.constraints.iter()));
    stated
        .filter_map(|constraint| match constraint {
            Constraint::Pattern(regex) => Some(*regex),
            _ => None,
        })
        .filter_map(|regex| ::regex::Regex::new(regex).ok().map(|re| (regex, re)))
        .collect()
});

/// Whether `value` satisfies `regex` as a Smithy `@pattern` is applied: a find rather than a
/// whole-value match, which is what the framework's own walk makes of one.
fn satisfies(regex: &str, value: &str) -> bool {
    COMPILED.get(regex).is_none_or(|re| re.is_match(value))
}

/// The expression a `@pattern` clause of this service quotes for `regex`: the model's text
/// without the whole-value pair it wraps the published expression in, or that text verbatim
/// for the expressions this service publishes as declared.
///
/// The DROP is the shared one (`service_common::validation::unanchored`), called rather than
/// restated, so this service cannot read the pair differently from any other.
pub(super) fn quoted_pattern(regex: &str) -> String {
    if PUBLISHED_AS_DECLARED.contains(&regex) {
        return regex.to_string();
    }
    unanchored(regex)
}

/// `path` with every step into a collection ELEMENT folded away: the 1-based position a walk
/// spells such a step with is dropped, so one row covers the member at every position
/// (`instanceIds.1.member` and `instanceIds.7.member` both fold to `instanceIds.member`).
///
/// A MAP entry whose key is all digits folds the same way, which is the ambiguity every
/// dotted path carries: the key a caller chose is not distinguishable from a position.
fn folded(path: &str) -> String {
    let kept: Vec<&str> = path
        .split('.')
        .filter(|segment| segment.is_empty() || !segment.bytes().all(|byte| byte.is_ascii_digit()))
        .collect();
    kept.join(".")
}

/// The `@pattern`, `@length` and `@range` this checkout's model declares for `input`'s
/// TOP-LEVEL member `member_path`, read off the member's own schema.
///
/// Codegen attaches the constraints of the shape a member TARGETS to that member's schema,
/// which is what the framework's walk validates against, so this is the shape's signature as
/// the running service holds it. TOP-LEVEL only, and that is the frozen schema crate's
/// limit rather than a choice: a member schema carries nothing of its target shape's
/// members, so a nested member's declared constraints are reachable from no schema here —
/// such a member keeps the framework's own clause, which is the fail-safe direction.
fn declared_of(input: &Schema, member_path: &str) -> Option<Declared> {
    let member = input.members().iter().find(|member| {
        member
            .member_name()
            .is_some_and(|name| path::member(name) == member_path)
    })?;
    let traits = member.traits()?;
    let length = constraint_trait::<LengthTrait>(traits, &LengthTrait::TRAIT_ID);
    let range = constraint_trait::<RangeTrait>(traits, &RangeTrait::TRAIT_ID);
    Some(Declared {
        pattern: constraint_trait::<PatternTrait>(traits, &PatternTrait::TRAIT_ID)
            .map(|pattern| pattern.regex),
        min_length: length.and_then(|length| length.min),
        max_length: length.and_then(|length| length.max),
        min_value: range.and_then(|range| integral(range.min)),
        max_value: range.and_then(|range| integral(range.max)),
    })
}

/// A `@range` bound as a row states one: the whole number it is. `None` for a bound the
/// model writes fractionally, which no row states — a page size is a whole number.
fn integral(bound: Option<f64>) -> Option<i64> {
    let bound = bound?;
    (bound.fract() == 0.0 && bound.is_finite()).then_some(bound as i64)
}

/// One constraint trait off a trait map, downcast to the type carrying its bounds.
fn constraint_trait<'map, T: 'static>(traits: &'map TraitMap, id: &ShapeId) -> Option<&'map T> {
    traits.get(id)?.as_any().downcast_ref::<T>()
}

/// The shape row that holds for `input`'s top-level member `member_path` under `operation`,
/// or `None` when this service states no set of its own for the shape that member targets.
fn shape_for(
    operation: Option<&str>,
    input: Option<&Schema>,
    member_path: &str,
) -> Option<&'static StatedShape> {
    let declared = declared_of(input?, member_path)?;
    STATED_SHAPES.iter().find(|row| {
        row.declared == declared
            && (row.members.is_empty() || row.members.contains(&member_path))
            && (row.operations.is_empty()
                || operation.is_some_and(|operation| row.operations.contains(&operation)))
    })
}

/// The element row that holds for the collection member `collection` under `operation`, or
/// `None` when this service states no set of its own for that collection's elements.
fn elements_for(operation: Option<&str>, collection: &str) -> Option<&'static StatedElements> {
    let folded_path = folded(collection);
    STATED_ELEMENTS.iter().find(|row| {
        row.collection == folded_path
            && (row.operations.is_empty()
                || operation.is_some_and(|operation| row.operations.contains(&operation)))
    })
}

/// The shape row a VIOLATION identifies on its own, with no schema in hand: one whose
/// declared constraints are what the violation reports.
///
/// The same keying as [`shape_for`] read from the other side. A violation carries the
/// constraint it broke, which for a `@pattern` or a `@length` bound the model declares IS the
/// signature of the shape that declares it, so the member is recognised without the schema —
/// which is what the refusal path that has no modelled operation to read the request against
/// has to work from.
///
/// A violation inside a COLLECTION identifies nothing: a clause stated at a collection member
/// aggregates its elements' whole set ([`element_requirement`]) rather than stating the
/// constraints of one element one by one.
fn shape_of_violation(
    operation: Option<&str>,
    violation: &Violation,
) -> Option<&'static StatedShape> {
    if violation.path.contains('.') {
        return None;
    }
    STATED_SHAPES.iter().find(|row| {
        let reports_the_signature = match &violation.kind {
            ViolationKind::Pattern(pattern) => row.declared.pattern == Some(pattern.as_str()),
            ViolationKind::MinLength(min) => row.declared.min_length == Some(*min),
            ViolationKind::MaxLength(max) => row.declared.max_length == Some(*max),
            ViolationKind::MinValue(min) => {
                row.declared.min_value.map(|min| min.to_string()).as_deref() == Some(min.as_str())
            }
            ViolationKind::MaxValue(max) => {
                row.declared.max_value.map(|max| max.to_string()).as_deref() == Some(max.as_str())
            }
            _ => false,
        };
        reports_the_signature
            && (row.members.is_empty() || row.members.contains(&violation.path.as_str()))
            && (row.operations.is_empty()
                || operation.is_some_and(|operation| row.operations.contains(&operation)))
    })
}

/// `violations` with the clauses about a member whose set this service states replaced by
/// that whole set — one clause per constraint of it the submitted value broke, in the order
/// the row states them.
///
/// `values` are the values the request carried, by the member path a violation names them
/// (`service_common::validation::submitted_renderings`), which is why this is reached with
/// the REQUEST in hand: a value that breaks only a constraint the model does not declare
/// carries no violation at all, and the service refuses it all the same. `input` is the
/// operation's own input schema, which the member's shape is recognised by ([`Declared`]).
/// Where neither is available — the refusal path with no modelled operation — a member is
/// recognised from the violation itself ([`shape_of_violation`]) and its set is stated over
/// the value that violation carries.
///
/// A member the framework also reported keeps the value half of the clause the framework
/// wrote for it, so a long value is quoted back under exactly the ceiling every other clause
/// of this service carries; a member only this set refused quotes the value the request
/// carried. Every violation about any OTHER member is kept exactly as it arrived, in the
/// order the walk found them.
pub(super) fn restating_the_stated_sets(
    operation: Option<&str>,
    violations: Vec<Violation>,
    values: &[(String, String)],
    input: Option<&'static Schema>,
) -> Vec<Violation> {
    let mut stated: Vec<(String, Vec<Violation>)> = values
        .iter()
        .filter_map(|(path, value)| {
            let row = shape_for(operation, input, path)?;
            let reported = violations.iter().find(|violation| violation.path == *path);
            let broken = broken_by(row, path, value, reported);
            (!broken.is_empty()).then_some((path.clone(), broken))
        })
        .collect();
    // The members the request's own values did not answer for, recognised from the violation
    // the walk reported about them.
    for violation in &violations {
        if stated.iter().any(|(path, _)| *path == violation.path) {
            continue;
        }
        let Some(row) = shape_of_violation(operation, violation) else {
            continue;
        };
        let Some(value) = violation.value.as_deref() else {
            continue;
        };
        let broken = broken_by(row, &violation.path, value, Some(violation));
        if !broken.is_empty() {
            stated.push((violation.path.clone(), broken));
        }
    }
    if stated.is_empty() {
        return violations;
    }

    let mut restated: Vec<Violation> = Vec::new();
    let mut written: HashSet<&str> = HashSet::new();
    for violation in &violations {
        match stated.iter().find(|(path, _)| *path == violation.path) {
            // The member's whole set stands in place of the first clause the walk reported
            // about it, and the walk's other clauses about it are already in that set.
            Some((path, broken)) => {
                if written.insert(path.as_str()) {
                    restated.extend(broken.iter().cloned());
                }
            }
            None => restated.push(violation.clone()),
        }
    }
    for (path, broken) in &stated {
        if !written.contains(path.as_str()) {
            restated.extend(broken.iter().cloned());
        }
    }
    restated
}

/// One clause per constraint of `row` that `value` breaks, stated at `path`, in the order the
/// row states them.
///
/// `reported` is the clause the framework's walk wrote about that member, when it wrote one:
/// its value half is kept, so a value is quoted back under exactly the ceiling every other
/// clause of this service carries, and a value that walk WITHHELD stays withheld.
fn broken_by(
    row: &'static StatedShape,
    path: &str,
    value: &str,
    reported: Option<&Violation>,
) -> Vec<Violation> {
    row.constraints
        .iter()
        .copied()
        .filter(|constraint| constraint.broken_by(value))
        .map(|constraint| Violation {
            path: path.to_string(),
            kind: constraint.kind(),
            value: match reported {
                Some(reported) => reported.value.clone(),
                None => Some(quoted_echo(value).into_owned()),
            },
            sensitive: reported.is_some_and(|reported| reported.sensitive),
        })
        .collect()
}

/// The requirement a clause states at the COLLECTION member `collection` for a constraint
/// its ELEMENTS broke: the element shape's whole set, nested the way AWS nests it.
///
/// A row's set where one holds for those elements, else the set the MODEL declares for them,
/// read off `input` — the operation's own input schema — so every collection of constrained
/// scalars this service models is stated at its member and not at the element the walk
/// happened to report. `None` leaves the framework's own per-element clause, which is what a
/// collection whose element set is not known here keeps.
pub(super) fn element_requirement(
    operation: Option<&str>,
    collection: &str,
    input: Option<&'static Schema>,
) -> Option<String> {
    if let Some(row) = elements_for(operation, collection) {
        let requirements: Vec<String> = row
            .constraints
            .iter()
            .map(|constraint| requirement(constraint.kind()))
            .collect();
        return Some(element_set_requirement(&requirements));
    }
    let declared = element_constraint_set(&[input?], collection)?;
    let requirements: Vec<String> = declared
        .into_iter()
        .map(|kind| match kind {
            // The CHECK used the model's pattern as written; only the rendering of the
            // expression is this service's.
            ViolationKind::Pattern(pattern) => {
                requirement(ViolationKind::Pattern(quoted_pattern(&pattern)))
            }
            other => requirement(other),
        })
        .collect();
    Some(element_set_requirement(&requirements))
}

#[cfg(test)]
mod tests {
    //! The rows against the model they are about, and the two renderings they feed.

    use super::*;

    /// This service's Smithy model, as it ships beside this crate. Read at test time rather
    /// than embedded: it is megabytes of JSON that no non-test build needs.
    const MODEL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-ssm-amazonssm.json"
    );

    /// The model as JSON.
    fn model() -> ::serde_json::Value {
        let read = std::fs::read_to_string(MODEL_PATH)
            .expect("the model that generated this crate ships beside it");
        ::serde_json::from_str(&read).expect("the model that generated this crate is JSON")
    }

    /// The expression a row states for its shape, if it states one.
    fn row_pattern(constraints: &'static [Constraint]) -> Option<&'static str> {
        constraints.iter().find_map(|constraint| match constraint {
            Constraint::Pattern(regex) => Some(*regex),
            _ => None,
        })
    }

    /// Whether a row carries a length bound of `bound`.
    fn carries_length(constraints: &'static [Constraint], bound: Constraint) -> bool {
        constraints.contains(&bound)
    }

    /// What the model declares for one shape, as a row's signature states it: its
    /// `@pattern`, then the bounds of its `@length` and of its `@range`.
    type Declaration<'model> = (
        Option<&'model str>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );

    /// What the model declares for the shape `shape`: its `@pattern`, and the bounds of its
    /// `@length` and `@range`. A shape the model does not declare, or declares with no
    /// constraint at all, reads as all `None`s.
    fn declared_in<'model>(
        model: &'model ::serde_json::Value,
        shape: &str,
    ) -> Declaration<'model> {
        let traits = &model["shapes"][shape]["traits"];
        (
            traits["smithy.api#pattern"].as_str(),
            traits["smithy.api#length"]["min"].as_i64(),
            traits["smithy.api#length"]["max"].as_i64(),
            traits["smithy.api#range"]["min"].as_i64(),
            traits["smithy.api#range"]["max"].as_i64(),
        )
    }

    /// Whether the model declares for `shape` exactly the signature `declared` states.
    fn declares(model: &::serde_json::Value, shape: &str, declared: Declared) -> bool {
        declared_in(model, shape)
            == (
                declared.pattern,
                declared.min_length,
                declared.max_length,
                declared.min_value,
                declared.max_value,
            )
    }

    /// RULE: a row states the MODEL's own constraints for its shape and adds to them — every
    /// length bound the model declares is in the row, and the expression it quotes is the one
    /// the model declares, except for a row scoped to the OPERATIONS whose parameter the
    /// service publishes its own expression for (an API reference publishes a `Pattern` per
    /// operation parameter, which can be wider than the shape every other member shares).
    ///
    /// So a shape-wide row can only widen what a caller is refused for, and a model revision
    /// that changes one of these shapes fails here instead of silently leaving a row behind.
    #[test]
    fn a_row_states_the_models_constraints_too() {
        let model = model();
        // (the shape a row is about, the constraints it states, the operations the row is
        // scoped to)
        let rows: Vec<(&str, &'static [Constraint], &'static [&'static str])> = STATED_SHAPES
            .iter()
            .flat_map(|row| {
                row.shapes
                    .iter()
                    .map(move |shape| (*shape, row.constraints, row.operations))
            })
            .chain(
                STATED_ELEMENTS
                    .iter()
                    .map(|row| (row.shape, row.constraints, row.operations)),
            )
            .collect();
        for (shape, constraints, operations) in rows {
            assert!(
                !model["shapes"][shape].is_null(),
                "case {shape}: the model declares the shape a row is about"
            );
            let (pattern, min_length, max_length, _, _) = declared_in(&model, shape);
            match (pattern, row_pattern(constraints)) {
                (Some(declared), Some(stated)) => assert!(
                    stated == declared || !operations.is_empty(),
                    "case {shape}: a shape-wide row quotes the expression the model declares; \
                     this one states {stated:?} for {declared:?}"
                ),
                (Some(declared), None) => panic!(
                    "case {shape}: the model declares {declared:?} and the row states no \
                     expression"
                ),
                // A `@pattern` the model does not declare at all is the service's own.
                (None, _) => {}
            }
            for (bound, declared) in [
                (Constraint::MinLength as fn(i64) -> Constraint, min_length),
                (Constraint::MaxLength as fn(i64) -> Constraint, max_length),
            ] {
                if let Some(declared) = declared {
                    assert!(
                        carries_length(constraints, bound(declared)),
                        "case {shape}: the model declares a length bound of {declared} and \
                         the row states no such bound"
                    );
                }
            }
        }
    }

    /// RULE: inside the SCOPE a shape row holds for — every operation, or the ones it names
    /// — every member the lookup would recognise targets a shape the row is ABOUT.
    ///
    /// That is the whole soundness of the keying: a member is recognised by the constraints
    /// the model declares for it, narrowed by the member names and the operations a row
    /// states, and no other member in that scope carries those constraints. (The hybrid-node
    /// id this model spells at some of the same member names declares a band and an
    /// expression of its own, so it is recognised as the other shape it is; three shapes are
    /// bounded to exactly 36 characters and nothing else, and two page sizes share one range,
    /// so the rows about those name their members — and the page size also names its
    /// operation, whose own parameter the ceiling belongs to.)
    #[test]
    fn a_row_is_keyed_to_the_shapes_it_is_about() {
        let model = model();
        let shapes = model["shapes"]
            .as_object()
            .expect("a Smithy model declares shapes");

        /// The input shape of one operation of this service.
        fn input_of<'model>(
            model: &'model ::serde_json::Value,
            operation: &str,
        ) -> &'model ::serde_json::Value {
            let declared = &model["shapes"][format!("com.amazonaws.ssm#{operation}")];
            let input = declared["input"]["target"]
                .as_str()
                .unwrap_or_else(|| panic!("case {operation}: the model declares the operation"));
            &model["shapes"][input]
        }

        for row in STATED_SHAPES {
            for shape in row.shapes {
                assert!(
                    declares(&model, shape, row.declared),
                    "case {shape}: the row's signature is not what the model declares, which \
                     is {:?}",
                    declared_in(&model, shape)
                );
            }
            // The structures a request inside this row's scope carries values at: every
            // structure of the model, or the inputs of the operations the row names.
            let scanned: Vec<(String, &::serde_json::Value)> = if row.operations.is_empty() {
                shapes
                    .iter()
                    .filter(|(_, shape)| shape["type"] == "structure")
                    .map(|(id, shape)| (id.clone(), shape))
                    .collect()
            } else {
                row.operations
                    .iter()
                    .map(|operation| ((*operation).to_string(), input_of(&model, operation)))
                    .collect()
            };
            for (shape_id, structure) in scanned {
                let Some(members) = structure["members"].as_object() else {
                    continue;
                };
                for (member, declared) in members {
                    let path = path::member(member);
                    // Exactly what the lookup asks: the member's name where the row states
                    // names, and the constraints the model declares for it.
                    if !row.members.is_empty() && !row.members.contains(&path.as_str()) {
                        continue;
                    }
                    let target = declared["target"]
                        .as_str()
                        .expect("a member names the shape it targets");
                    if !declares(&model, target, row.declared) {
                        continue;
                    }
                    assert!(
                        row.shapes.contains(&target),
                        "case {shape_id}${member}: a member this row would be applied to \
                         targets {target}, which the row is not about"
                    );
                }
            }
        }
    }

    /// RULE: an element row keyed by a one-segment collection name is keyed by a name that
    /// names ONE list in this model, whose elements are the shape the row is about.
    #[test]
    fn an_element_rows_collection_names_one_element_shape() {
        let model = model();
        let shapes = model["shapes"]
            .as_object()
            .expect("a Smithy model declares shapes");
        for row in STATED_ELEMENTS {
            if !row.operations.is_empty() || row.collection.contains('.') {
                continue;
            }
            for (shape_id, shape) in shapes {
                // STRUCTURE members only: an enumerated type's values are members of a kind
                // no request carries a value at, so they name no member path.
                if shape["type"] != "structure" {
                    continue;
                }
                let Some(members) = shape["members"].as_object() else {
                    continue;
                };
                for (member, declared) in members {
                    if path::member(member) != row.collection {
                        continue;
                    }
                    let list = declared["target"]
                        .as_str()
                        .expect("a member names the shape it targets");
                    let element = model["shapes"][list]["member"]["target"].as_str();
                    assert_eq!(
                        element,
                        Some(row.shape),
                        "case {shape_id}${member}: a collection this row's name keys holds \
                         another shape, so the row needs the operations it holds for"
                    );
                }
            }
        }
    }

    /// RULE 1: the expression a clause quotes — the model's text without the whole-value
    /// pair it wraps the published expression in, and verbatim for the expressions this
    /// service publishes as declared.
    #[test]
    fn an_expression_is_quoted_as_the_service_publishes_it() {
        // (case, the model's text, the expression a clause quotes)
        let cases: &[(&str, &str, &str)] = &[
            (
                "a UUID member: the pair the model wrote is dropped",
                "^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$",
                "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}",
            ),
            (
                "an ARN member",
                r"^arn:(aws[a-zA-Z-]*)?:ssm:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:opsmetadata\/([a-zA-Z0-9-_\.\/]*)$",
                r"arn:(aws[a-zA-Z-]*)?:ssm:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:opsmetadata\/([a-zA-Z0-9-_\.\/]*)",
            ),
            (
                "a node id, whose own branches keep their anchors",
                INSTANCE_ID_PATTERN,
                r"(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)",
            ),
            (
                "a document version, anchored inside its alternation",
                DOCUMENT_VERSION_PATTERN,
                r"([$]LATEST|[$]DEFAULT|^[1-9][0-9]*$)",
            ),
            (
                "an OpsItem id, which this service publishes as the model declares it",
                OPS_ITEM_ID_PATTERN,
                OPS_ITEM_ID_PATTERN,
            ),
            (
                "the command id the service itself requires",
                COMMAND_ID_PATTERN,
                COMMAND_ID_PATTERN,
            ),
            (
                "a name a caller chooses, which this service publishes as the model declares it",
                CHOSEN_NAME_PATTERN,
                CHOSEN_NAME_PATTERN,
            ),
            (
                "an expression with no pair to drop",
                r"[a-z]+",
                r"[a-z]+",
            ),
        ];
        for (case, declared, expected) in cases {
            let quoted = quoted_pattern(declared);
            assert_eq!(
                &quoted, expected,
                "case {case:?}: expected {expected:?}, got {quoted:?}"
            );
        }
    }

    /// RULE 2: the constraints a value breaks, over the classes a row carries and over
    /// values on either side of each bound.
    /// The input schema of one of this service's operations, which a member's shape is
    /// recognised by.
    fn input_schema<O: ::smithy_server_runtime::Operation>() -> &'static Schema {
        O::INPUT_SCHEMA
    }

    /// RULE 2: the constraints a value breaks, over the classes a row carries and over
    /// values on either side of each bound — driven through the operations' OWN generated
    /// input schemas, which is what the running service recognises a member's shape by.
    #[test]
    fn a_stated_set_reports_every_constraint_the_value_breaks() {
        let command =
            input_schema::<crate::ops::get_command_invocation::GetCommandInvocation<()>>();
        let inventory = input_schema::<crate::ops::list_inventory_entries::ListInventoryEntries<()>>();
        let preview =
            input_schema::<crate::ops::start_execution_preview::StartExecutionPreview<()>>();
        let parameter = input_schema::<crate::ops::put_parameter::PutParameter<()>>();

        // (case, the operation's input schema, the member path the request carried the value
        // at, the value, the requirements the clauses state)
        let cases: &[(&str, &'static Schema, &str, &str, &[&str])] = &[
            (
                "a command id that is neither a UUID nor long enough",
                command,
                "commandId",
                "not-a-command-id",
                &[
                    "Member must satisfy regular expression pattern: ^[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}$",
                    "Member must have length greater than or equal to 36",
                ],
            ),
            (
                "a command id of the modelled length that is not a UUID",
                command,
                "commandId",
                "not-a-command-id-but-36-characters!!",
                &[
                    "Member must satisfy regular expression pattern: ^[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}$",
                ],
            ),
            (
                "a command id this service mints",
                command,
                "commandId",
                "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
                &[],
            ),
            (
                "a node id too short for the band and outside the expression",
                inventory,
                "instanceId",
                "i-zzz",
                &[
                    "Member must have length greater than or equal to 10",
                    r"Member must satisfy regular expression pattern: (^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)",
                ],
            ),
            (
                "an EC2 node id of either width",
                inventory,
                "instanceId",
                "i-0abcdef0123456789",
                &[],
            ),
            (
                "the shorter EC2 node id, at the band's floor",
                inventory,
                "instanceId",
                "i-0abcdef0",
                &[],
            ),
            (
                "a hybrid node id, at the band's ceiling",
                inventory,
                "instanceId",
                "mi-0abcdef0123456789",
                &[],
            ),
            (
                "a document version that is neither a label nor a number",
                preview,
                "documentVersion",
                "not-a-version",
                &[
                    r"Member must satisfy regular expression pattern: ([$]LATEST|[$]DEFAULT|^[1-9][0-9]*$)",
                    "Member must have length less than or equal to 8",
                ],
            ),
            (
                "the default version label, at the length ceiling",
                preview,
                "documentVersion",
                "$DEFAULT",
                &[],
            ),
            (
                "a version number the length admits",
                preview,
                "documentVersion",
                "12345678",
                &[],
            ),
            (
                "a version number one digit past the length",
                preview,
                "documentVersion",
                "123456789",
                &["Member must have length less than or equal to 8"],
            ),
            (
                "a member whose shape no row is about",
                parameter,
                "name",
                "anything at all",
                &[],
            ),
        ];
        for (case, schema, path, value, expected) in cases {
            let leaves = vec![((*path).to_string(), (*value).to_string())];
            let restated =
                restating_the_stated_sets(Some("AnyOperation"), Vec::new(), &leaves, Some(schema));
            let stated: Vec<String> = restated
                .iter()
                .map(|violation| requirement(violation.kind.clone()))
                .collect();
            assert_eq!(
                stated, *expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
            for violation in &restated {
                assert_eq!(
                    violation.value.as_deref(),
                    Some(*value),
                    "case {case:?}: a clause quotes the value the request carried"
                );
            }
        }
    }

    /// RULE 2 at a position: the set replaces the clauses the walk reported about that
    /// member, and leaves every other clause where it was.
    #[test]
    fn a_stated_set_replaces_the_walks_clauses_about_that_member() {
        let reported = vec![
            Violation {
                path: "maxResults".to_string(),
                kind: ViolationKind::MaxValue("50".to_string()),
                value: Some("51".to_string()),
                sensitive: false,
            },
            Violation {
                path: "instanceId".to_string(),
                kind: ViolationKind::Pattern(INSTANCE_ID_PATTERN.to_string()),
                value: Some("i-zzz".to_string()),
                sensitive: false,
            },
        ];
        let leaves = vec![("instanceId".to_string(), "i-zzz".to_string())];
        let restated = restating_the_stated_sets(
            Some("ListInventoryEntries"),
            reported,
            &leaves,
            Some(input_schema::<
                crate::ops::list_inventory_entries::ListInventoryEntries<()>,
            >()),
        );
        let paths: Vec<&str> = restated
            .iter()
            .map(|violation| violation.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["maxResults", "instanceId", "instanceId"],
            "case a page size beside a node id: expected the page size, then the node id's \
             whole set, got {restated:?}"
        );
    }

    /// RULE 3: the set a clause nests at a collection member, for a row's collection and for
    /// one whose set is the model's.
    #[test]
    fn a_collections_elements_are_stated_at_the_member() {
        let instance_ids = element_requirement(Some("SendCommand"), "instanceIds", None)
            .expect("the node id list states a set of its own");
        assert_eq!(
            instance_ids,
            "Member must satisfy constraint: [Member must have length less than or equal to \
             20, Member must have length greater than or equal to 10, Member must satisfy \
             regular expression pattern: (^i-(\\w{8}|\\w{17})$)|(^mi-\\w{17}$), Member must \
             not be null]",
            "case instanceIds: got {instance_ids:?}"
        );

        let values = element_requirement(
            Some("ListOpsItemEvents"),
            "filters.1.member.values",
            None,
        )
        .expect("the OpsItem event filter values state a set of their own");
        assert_eq!(
            values,
            "Member must satisfy constraint: [Member must have length less than or equal to \
             15, Member must have length greater than or equal to 1, Member must satisfy \
             regular expression pattern: ^(oi)-[0-9a-f]{12}$]",
            "case filters.1.member.values: got {values:?}"
        );

        assert!(
            element_requirement(Some("DescribeOpsItems"), "filters.1.member.values", None)
                .is_none(),
            "case another operation's filter values: a row that names its operations holds \
             for no other"
        );
        assert!(
            element_requirement(Some("SendCommand"), "notACollection", None).is_none(),
            "case a member no row is about: the framework's own clause stands"
        );
    }

    /// Every step into an element folds away, whatever position it names.
    #[test]
    fn a_member_path_folds_its_element_steps() {
        // (case, the path a walk spelled, the folded path)
        let cases: &[(&str, &str, &str)] = &[
            ("a top-level member", "instanceId", "instanceId"),
            ("a list element", "instanceIds.1.member", "instanceIds.member"),
            (
                "a later list element",
                "instanceIds.17.member",
                "instanceIds.member",
            ),
            (
                "a member of a structure element",
                "filters.3.member.values.2.member",
                "filters.member.values.member",
            ),
            (
                "a structure member that is not an element",
                "target.targetAddress",
                "target.targetAddress",
            ),
        ];
        for (case, path, expected) in cases {
            let folded = folded(path);
            assert_eq!(
                &folded, expected,
                "case {case:?}: expected {expected:?}, got {folded:?}"
            );
        }
    }
}
