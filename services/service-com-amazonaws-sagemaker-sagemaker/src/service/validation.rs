// SPDX-License-Identifier: CC0-1.0

//! How this service STATES a request that breaks a constraint.
//!
//! The framework finds the violations (the protocol's deserializer for an absent
//! required member, `constraints::validate` for a value outside a declared bound) and
//! words them in its own conventional envelope — `1 validation error detected: Value
//! '<value>' at '<member path>' failed to satisfy constraint: <requirement>`. SageMaker
//! words the same facts differently in six ways, each a property of the shape that was
//! violated — or of the layer that collected the findings — rather than of one request, so
//! each belongs here rather than in a handler:
//!
//! 1. **A `@pattern` is stated WITHOUT the `^`/`$` anchors.** Smithy's `@pattern` is an
//!    unanchored find, so a model writes `^…$` to demand a whole-value match; the
//!    requirement a caller is told about is the expression itself. `CreateTrialComponent`
//!    `DisplayName` (`smithy_query(query="shape",shape="ExperimentEntityName")`:
//!    `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,119}$`) is reported as
//!    `[a-zA-Z0-9](-*[a-zA-Z0-9]){0,119}`, and `S3Uri`
//!    (`smithy_query(query="shape",shape="S3Uri")`: `^(https|s3)://([^/]+)/?(.*)$`) as
//!    `(https|s3)://([^/]+)/?(.*)`. The pair is dropped from the RENDERING only — the
//!    check still runs the model's own expression ([`unanchored`]).
//! 2. **An enum value set is listed in the order the SERVICE's own table of admissible
//!    values is WALKED** — ascending bucket of each value's own string hash, a bucket
//!    holding two values against the order they were put into it ([`stated_order`]) — which
//!    is neither the order the public model declares them in nor a sort of them; and it is
//!    listed over the values the SERVICE holds, which for two of this model's enums is more
//!    than the members the public model declares ([`STATED_VALUE_SETS`]).
//! 3. **A value outside the TRAINING instance types is told the CONDITION it failed**
//!    rather than handed that value set
//!    ([`SUPPORTED_INSTANCE_TYPE_REQUIREMENT`]).
//! 4. **A violation of a MAP member's values is stated against the map**, echoing the
//!    whole map and listing the value shape's complete constraint set
//!    ([`map_value_message`]); that one is reached from the handler that enforces such a
//!    constraint, because the framework's own walk reports the offending entry instead.
//! 5. **The clauses of one refusal are stated in the order that same findings table is
//!    walked**, keyed on the NAME of the member each clause is about
//!    ([`in_stated_clause_order`]) — not in the order the model declares those members and
//!    not in the order the request wrote them.
//! 6. **A constraint broken by a LIST's ELEMENTS is stated once at the LIST MEMBER**,
//!    quoting the whole list that member held and nesting the element shape's complete
//!    declared constraint set ([`element_clause`]), where the framework reports one clause
//!    per offending element at `<list>.<position>.member` naming only the constraint that
//!    element broke. The list a clause quotes is the request's own value for that member,
//!    which no violation carries, so that half is collected per operation from the request
//!    ([`super::list_element_refusals`]) and handed in here.
//!
//! One LIMIT of rendering 2, stated rather than hidden: a bucket holding two values is
//! ordered by the order the service's table was FILLED, and codegen hands a violation the
//! admissible values sorted by their MEMBER NAME, so for an enum [`STATED_VALUE_SETS`] holds
//! no row for, that order is the sorted one rather than the model's declaration order. It
//! shows only in a set big enough to put two values in one bucket, and every such set whose
//! listing a recording of this service covers has a row; one that gains a recording needs its
//! declaration order stated there too.
//!
//! Status (400) and error code ([`VALIDATION_EXCEPTION`]) are the framework's own choice
//! for a constraint failure and are left alone; only the constructed sentence changes.

use std::collections::{BTreeMap, HashMap, HashSet};

use aws_smithy_schema::Schema;
use aws_smithy_schema_server::constraints::ViolationKind;
use service_common::submitted_collections::map_entry_bucket;
use service_common::validation::{list_of_element, unanchored};
use service_common::validation_message::{
    element_constraint_set, element_set_requirement, hashed_enum_order, stated_enum_value_set,
    valueless_clause, StatedEnumValueSets,
};
use smithy_server_runtime::{Refused, Violation};

/// The error code a constraint failure is named by, unchanged from the one the
/// framework's refusal channel picks for this `awsJson1_1` service: a chosen [`Refused`]
/// replaces the code and the message TOGETHER, so the code is restated here rather than
/// left to a default that a reworded message would otherwise silently drop.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The text separating a clause's subject from the requirement it states, as the
/// framework's own [`Violation::message`] writes it.
const CONSTRAINT_SEPARATOR: &str = " failed to satisfy constraint: ";

/// What a clause states when the value that broke a constraint is a MAP's value: the
/// whole map is the value, and the requirement is the value shape's whole constraint set
/// rather than the one bound that failed.
const MAP_VALUE_REQUIREMENT: &str = "Map value must satisfy constraint: ";

/// The value set the SEARCH surface's resource type is listed over, which is LARGER than
/// the public model's `ResourceType` enum.
///
/// `smithy_query(query="shape",shape="ResourceType")` declares 20 values; a recorded
/// refusal of a `Resource` outside the enum lists the 25 below, the model's 20 plus the
/// five Studio and hosting resources the search index also holds (`App`,
/// `InferenceComponent`, `Domain`, `EndpointConfig`, `UserProfile`). The set is therefore a
/// are therefore a property of the ENUM SHAPE as the SERVICE holds it — not of one request
/// and not of one operation: every member targeting `ResourceType` (`Search`'s and
/// `GetSearchSuggestions`' `Resource`) is told the same set, the same way. The ORDER written
/// here is the order the service's table of them was filled, which decides two of them that
/// share a bucket ([`stated_order`]).
///
/// FRAGILITY, stated rather than hidden: a runtime [`Violation`] carries the value set but
/// not the shape it came from, so the row below is recognised by the MEMBERS the model
/// declares for it — read from the generated `RESOURCE_TYPE_VALUES`, so the recognition
/// follows the model — and a `ResourceType` that gains or loses a member stops being
/// recognised until this list is updated with it. What the service ACCEPTS is the model's
/// own enum, which is what the request walk checks; only the listing a refusal carries is
/// the service's own.
/// The value set the SEARCH surface's resource type is listed over, which is LARGER than
/// the public model's `ResourceType` enum, in the order the service's table of it was
/// FILLED.
///
/// `smithy_query(query="shape",shape="ResourceType")` declares 20 values; a recorded
/// refusal of a `Resource` outside the enum lists 25, the model's 20 plus the five Studio
/// and hosting resources the search index also holds (`App`, `InferenceComponent`, `Domain`,
/// `EndpointConfig`, `UserProfile`). The set is therefore a property of the ENUM SHAPE as
/// the SERVICE holds it — not of one request and not of one operation: every member
/// targeting `ResourceType` (`Search`'s and `GetSearchSuggestions`' `Resource`) is told the
/// same set, the same way.
///
/// The ORDER below is the FILL order, which is what decides two values that share a bucket
/// ([`stated_order`]): the model's own declaration order for the 20 it declares, with the
/// five the service also holds placed where the recorded listing puts them. Every bucket of
/// that listing holding more than one value carries them in the REVERSE of this order, which
/// is what identified the chain direction — `Domain` therefore sits between
/// `ExperimentTrialComponent` and `HubContent`, and `InferenceComponent` after `HubContent`,
/// while the other three share a bucket with nothing and their place here is free.
///
/// FRAGILITY, stated rather than hidden: a runtime [`Violation`] carries the value set but
/// not the shape it came from, so the row below is recognised by the MEMBERS the model
/// declares for it — read from the generated `RESOURCE_TYPE_VALUES`, so the recognition
/// follows the model — and a `ResourceType` that gains or loses a member stops being
/// recognised until this list is updated with it. What the service ACCEPTS is the model's
/// own enum, which is what the request walk checks; only the listing a refusal carries is
/// the service's own.
const SEARCH_RESOURCE_TYPES: [&str; 25] = [
    "TrainingJob",
    "Experiment",
    "ExperimentTrial",
    "ExperimentTrialComponent",
    "Domain",
    "Endpoint",
    "EndpointConfig",
    "Model",
    "ModelPackage",
    "ModelPackageGroup",
    "Pipeline",
    "PipelineExecution",
    "FeatureGroup",
    "FeatureMetadata",
    "Image",
    "ImageVersion",
    "Project",
    "HyperParameterTuningJob",
    "ModelCard",
    "PipelineVersion",
    "Job",
    "HubContent",
    "InferenceComponent",
    "App",
    "UserProfile",
];

/// The value sets of this service whose enums the SERVICE holds values the public model
/// does not declare for, matched by the model's own set for each
/// ([`stated_enum_value_set`]).
///
/// A runtime [`Violation`] carries the values a member admits and not the shape they came
/// from, so a row is recognised by that SET: every member targeting one enum — in any
/// operation, at any depth — is therefore listed alike. A row holds its values in the order
/// the service's table of them was FILLED, which is what decides two values that share a
/// bucket ([`stated_order`]); WHICH values a member admits is never decided here: that is
/// the model's enum, which the framework's own walk checks.
const STATED_VALUE_SETS: &StatedEnumValueSets<'static> = &[
    (crate::ops::types::RESOURCE_TYPE_VALUES, &SEARCH_RESOURCE_TYPES),
    (
        crate::ops::types::TRACKING_SERVER_SIZE_VALUES,
        &TRACKING_SERVER_SIZES,
    ),
    (
        crate::ops::types::OPTIMIZATION_JOB_DEPLOYMENT_INSTANCE_TYPE_VALUES,
        &OPTIMIZATION_JOB_DEPLOYMENT_INSTANCE_TYPES,
    ),
];

/// The value set an MLflow tracking server's size is listed over, which is LARGER than the
/// public model's `TrackingServerSize` enum.
///
/// `smithy_query(query="shape",shape="TrackingServerSize")` declares three values
/// (`Small`, `Medium`, `Large`); a recorded `CreateMlflowTrackingServer` refusal of a size
/// outside the enum lists eight — those three beside the five `mm1.*` sizes the service
/// itself holds for the member. The set belongs to the ENUM SHAPE as the service holds it,
/// so every member targeting it (`CreateMlflowTrackingServer`'s and
/// `UpdateMlflowTrackingServer`'s `TrackingServerSize`) is told the same eight. No two of
/// these eight share a bucket, so the order written here decides nothing; and what a member
/// ACCEPTS stays the model's three: the walk checks the model's enum, and only the listing a
/// refusal carries is the service's own.
const TRACKING_SERVER_SIZES: [&str; 8] = [
    "Small",
    "Medium",
    "Large",
    "mm1.small",
    "mm1.medium",
    "mm1.large",
    "mm1.xlarge",
    "mm1.2xlarge",
];

/// The deployment instance types of an optimization job, in the order the MODEL DECLARES
/// them — which is the order the service's table of them was filled, and so what decides two
/// of them that share a bucket ([`stated_order`]).
///
/// Why it is written out at all, when the set is exactly the model's: codegen hands a
/// violation the admissible values sorted by their MEMBER NAME
/// (`vendor/smithy-macros/src/codegen.rs`'s `enum_shape_values`, over a JSON object), so the
/// declaration order is lost before a clause is worded — and this set is the only one of
/// this service whose recorded listing holds MORE values than the table has buckets, which
/// is what makes that order observable. Source, value for value and in this order:
/// `smithy_query(query="shape",shape="OptimizationJobDeploymentInstanceType")`. A larger set
/// of this model whose listing a recording covers needs its declaration order stated here
/// too; one no bucket of which holds two values does not.
const OPTIMIZATION_JOB_DEPLOYMENT_INSTANCE_TYPES: [&str; 55] = [
    "ml.p4d.24xlarge",
    "ml.p4de.24xlarge",
    "ml.p5.48xlarge",
    "ml.p5e.48xlarge",
    "ml.p5en.48xlarge",
    "ml.g4dn.xlarge",
    "ml.g4dn.2xlarge",
    "ml.g4dn.4xlarge",
    "ml.g4dn.8xlarge",
    "ml.g4dn.12xlarge",
    "ml.g4dn.16xlarge",
    "ml.g5.xlarge",
    "ml.g5.2xlarge",
    "ml.g5.4xlarge",
    "ml.g5.8xlarge",
    "ml.g5.12xlarge",
    "ml.g5.16xlarge",
    "ml.g5.24xlarge",
    "ml.g5.48xlarge",
    "ml.g6.xlarge",
    "ml.g6.2xlarge",
    "ml.g6.4xlarge",
    "ml.g6.8xlarge",
    "ml.g6.12xlarge",
    "ml.g6.16xlarge",
    "ml.g6.24xlarge",
    "ml.g6.48xlarge",
    "ml.g6e.xlarge",
    "ml.g6e.2xlarge",
    "ml.g6e.4xlarge",
    "ml.g6e.8xlarge",
    "ml.g6e.12xlarge",
    "ml.g6e.16xlarge",
    "ml.g6e.24xlarge",
    "ml.g6e.48xlarge",
    "ml.inf2.xlarge",
    "ml.inf2.8xlarge",
    "ml.inf2.24xlarge",
    "ml.inf2.48xlarge",
    "ml.trn1.2xlarge",
    "ml.trn1.32xlarge",
    "ml.trn1n.32xlarge",
    "ml.p6-b200.48xlarge",
    "ml.g7e.2xlarge",
    "ml.g7e.4xlarge",
    "ml.g7e.8xlarge",
    "ml.g7e.12xlarge",
    "ml.g7e.24xlarge",
    "ml.g7e.48xlarge",
    "ml.g7.2xlarge",
    "ml.g7.4xlarge",
    "ml.g7.8xlarge",
    "ml.g7.12xlarge",
    "ml.g7.24xlarge",
    "ml.g7.48xlarge",
];

/// What a member whose value must be one of the TRAINING instance types is told it failed:
/// the CONDITION the value did not meet, in place of the value set that would meet it.
///
/// A property of that value SET — not of one member, one operation or one request — so
/// every member targeting it states this, wherever it sits in a request:
/// `ResourceConfig$InstanceType` and its `InstanceGroups` entries, the tuning resource
/// configuration's instance configurations, and an algorithm's supported training instance
/// types. The listing of [`as_rendered`] stands for every OTHER enum of this service,
/// the hosting instance types (`ProductionVariantInstanceType`) among them, whose recorded
/// refusal does list its set.
///
/// The value set is recognised by its MEMBERS, from the generated
/// `TRAINING_INSTANCE_TYPE_VALUES` — the model's own values, so the recognition follows the
/// model rather than a list written out here — on the same terms, and with the same
/// fragility, as [`MODEL_APPROVAL_STATUS`]: a runtime [`Violation`] carries the set and not
/// the shape it came from.
const SUPPORTED_INSTANCE_TYPE_REQUIREMENT: &str = "Member must be a supported instance type";

/// This service's wording for the violations the framework found on one request, or
/// `None` when there are none to word.
///
/// `input_schemas` are the operation input shapes a clause stated at a COLLECTION member
/// reads the element shape's constraint set off ([`element_clause`]), and
/// `submitted_lists` the lists the request carried, keyed by the member path the walk
/// reports each at — the value such a clause quotes, which no violation carries. Both are
/// EMPTY on the centralized refusal seam, which is handed the violations alone: a clause
/// this service cannot state at the collection keeps the framework's own per-element one,
/// which is the same answer that seam gave before this grouping existed.
pub(super) fn refusal(
    violations: &[Violation],
    input_schemas: &[&'static Schema],
    submitted_lists: &HashMap<String, String>,
) -> Option<Refused> {
    let mut clauses: Vec<String> = Vec::new();
    // An identical clause is stated ONCE: a collection is named once however many of its
    // elements the walk reported. Recognised through a set rather than by rescanning the
    // clauses already collected, so the cost stays linear in the violation count — which
    // the framework's own walk bounds.
    let mut stated: HashSet<String> = HashSet::new();
    for violation in in_stated_clause_order(violations) {
        let clause = element_clause(violation, input_schemas, submitted_lists)
            .unwrap_or_else(|| stated_clause(violation));
        if stated.insert(clause.clone()) {
            clauses.push(clause);
        }
    }
    let message = stated_message(&clauses)?;
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message,
    })
}

/// How wide the table holding one refusal's findings is: a CONSTANT of the layer holding
/// them rather than a width sized by how many findings one request produced.
///
/// What shows the order is a walk of such a table and not the model's member order: a
/// recorded `AttachClusterNodeVolume` refusal whose three members ALL break their declared
/// expressions states them `clusterArn`, `volumeId`, `nodeId`, while the model declares
/// them `ClusterArn`, `NodeId`, `VolumeId` — and that is their order by bucket at this
/// width. (It is their order at twice this width too, and not at four times it, which is
/// why the width is stated here rather than derived from a finding count.)
const FINDINGS_TABLE_BUCKETS: usize = 16;

/// `violations` in the order this service states their clauses: by the bucket the NAME of
/// the member each clause is about sits in, and within one bucket by that name.
///
/// The bucket is the shared one every order this layer exposes is read from
/// ([`map_entry_bucket`], the 31-polynomial over the name's UTF-16 code units with its high
/// half folded into its low half), at [`FINDINGS_TABLE_BUCKETS`]. The KEY is the member's
/// own name — the last step of the path a clause states it by, so a member of a structure
/// the request nests is keyed by its name there — and for a violation the framework found
/// at a list ELEMENT it is the LIST member this service states the clause at, so the clause
/// sits where the member it is about does.
///
/// STABLE and over that name alone, so two requirements of ONE member keep the order the
/// framework's walk collected them in and only the members move. Nothing about a clause's
/// text is decided here. Each key is computed once, in a single pass rather than inside the
/// comparator, and the walk hands over at most its own ceiling of violations, so the work is
/// bounded by that ceiling rather than by one request.
fn in_stated_clause_order(violations: &[Violation]) -> Vec<&Violation> {
    let mut keyed: Vec<((usize, &str), &Violation)> = violations
        .iter()
        .map(|violation| {
            let member = member_name(violation);
            (
                (map_entry_bucket(member, FINDINGS_TABLE_BUCKETS), member),
                violation,
            )
        })
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, violation)| violation).collect()
}

/// The NAME of the member `violation`'s clause is about; see [`in_stated_clause_order`].
fn member_name(violation: &Violation) -> &str {
    let stated_at = list_of_element(&violation.path).unwrap_or(&violation.path);
    stated_at.rsplit('.').next().unwrap_or(stated_at)
}

/// The clause a constraint broken by a LIST's ELEMENTS is stated as — at the LIST MEMBER,
/// quoting the whole list that member held and nesting the element shape's COMPLETE declared
/// constraint set — or `None` for a violation that is not a list element's and for a
/// collection whose element set `input_schemas` does not answer for.
///
/// The framework reports such a violation at the element it found it on
/// (`<list>.<1-based position>.member`, quoting that element and naming only the constraint
/// it broke); AWS reports it at the list, because the member the caller submitted is the
/// list. So one list is ONE validation error however many of its elements offend, and the
/// requirement is everything an element has to satisfy rather than the one bound that
/// happened to fail: the element shape's declared constraints, read off the model
/// ([`element_constraint_set`] — the refusal carries only what failed), each worded as this
/// service words a requirement ([`requirement_text`]), and last that the element be PRESENT
/// at all, which every list of this model requires of its elements (none is `@sparse`).
///
/// The value half is the framework's own spelling of one, read off a clause it frames for
/// the collection member itself, so a grouped clause reads like a clause about a member the
/// walk reported; a collection the request's lists do not answer for — one the collecting
/// walk could not render in full, or one whose elements the refusal may not disclose —
/// states the member and the requirement with no value at all ([`valueless_clause`]).
fn element_clause(
    violation: &Violation,
    input_schemas: &[&'static Schema],
    submitted_lists: &HashMap<String, String>,
) -> Option<String> {
    let collection = list_of_element(&violation.path)?;
    let declared = element_constraint_set(input_schemas, collection)?;
    let mut requirements: Vec<String> = declared.iter().map(requirement_text).collect();
    requirements.push(requirement_text(&ViolationKind::MissingRequiredMember));
    let stated_at = Violation {
        path: collection.to_string(),
        // Any class frames the clause: only the half before the separator is read, and the
        // requirement is the nested set below.
        kind: ViolationKind::MissingRequiredMember,
        value: submitted_lists.get(collection).cloned(),
        sensitive: false,
    };
    let framed = match stated_at.value {
        Some(_) => stated_at.message(),
        None => valueless_clause(&stated_at)?,
    };
    let (subject, _) = framed.rsplit_once(CONSTRAINT_SEPARATOR)?;
    Some(format!(
        "{subject}{CONSTRAINT_SEPARATOR}{}",
        element_set_requirement(&requirements)
    ))
}

/// One violation as this service states it.
///
/// The whole sentence is the FRAMEWORK's — the echoed value, the member path, the frame
/// around them and the requirement itself — reached through the same [`Violation`] the
/// walk produced, so a constraint class this service does not reword cannot drift from the
/// framework's own clause. Where the service states the CONDITION a value failed instead
/// of the framework's rendering of the constraint ([`restated_requirement`]), only the
/// requirement half is replaced, so the clause's subject stays the framework's.
fn stated_clause(violation: &Violation) -> String {
    let clause = as_stated(violation).message();
    let Some(requirement) = restated_requirement(&violation.kind) else {
        return clause;
    };
    match clause.rsplit_once(CONSTRAINT_SEPARATOR) {
        Some((subject, _)) => format!("{subject}{CONSTRAINT_SEPARATOR}{requirement}"),
        None => clause,
    }
}

/// The requirement this service words ITSELF, for a constraint whose failure it states as
/// a condition rather than as the framework's rendering of the constraint — or `None`,
/// which is every other class, where the framework's requirement stands.
///
/// [`ViolationKind`] carries the constraint that was broken, so a sentence that is not a
/// rendering OF one cannot travel on it: it is chosen here, from the violated constraint,
/// and nowhere else.
fn restated_requirement(kind: &ViolationKind) -> Option<&'static str> {
    match kind {
        ViolationKind::EnumValue(values)
            if is_set_of(values, crate::ops::types::TRAINING_INSTANCE_TYPE_VALUES) =>
        {
            Some(SUPPORTED_INSTANCE_TYPE_REQUIREMENT)
        }
        _ => None,
    }
}

/// The `ValidationException` message around the clauses this service stated.
///
/// Spelled exactly as [`aws_smithy_schema_server::envelope`] spells it, and not reached
/// through it, because a clause this module states is not always one the framework can
/// build from a [`Violation`] (see [`restated_requirement`] and [`map_value_message`]).
fn stated_message(clauses: &[String]) -> Option<String> {
    match clauses {
        [] => None,
        [one] => Some(envelope_of(one)),
        many => Some(format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        )),
    }
}

/// One violation as this service states it: the framework's path and echoed value, with
/// the violated constraint rendered the way this service renders it.
fn as_stated(violation: &Violation) -> Violation {
    Violation {
        path: violation.path.clone(),
        kind: as_rendered(&violation.kind),
        value: violation.value.clone(),
        sensitive: violation.sensitive,
    }
}

/// A violated constraint as this service renders it: a `@pattern` without its
/// whole-value anchors, an enum value set as the service's own table lists it, and every
/// other class exactly as the framework renders it.
fn as_rendered(kind: &ViolationKind) -> ViolationKind {
    match kind {
        ViolationKind::Pattern(expression) => ViolationKind::Pattern(unanchored(expression)),
        ViolationKind::EnumValue(values) => ViolationKind::EnumValue(stated_order(values)),
        other => other.clone(),
    }
}

/// The requirement half of a clause: what the value had to satisfy, with none of the
/// clause's subject.
///
/// Read off the FRAMEWORK's own rendering of `kind` — with an empty path and no value, so
/// every word of the sentence is the framework's or the model's — and then cut at the
/// clause separator, at its LAST occurrence: the requirement is the tail, and cutting
/// there cannot be fooled by a separator-shaped run inside the rendering. A constraint
/// class the framework learns to check therefore cannot drift from this wording, and
/// [`ViolationKind`] being `#[non_exhaustive]` costs nothing here.
///
/// MIGRATION NOTE: `services/common/src/validation.rs` reserves this "requirement half"
/// rule for the shared module (its header lists it among the parts still to land there).
/// It has no shared helper yet, so this is the local reading until that lands — and is to
/// be deleted in favour of it rather than copied into a third service.
fn requirement_text(kind: &ViolationKind) -> String {
    if let Some(requirement) = restated_requirement(kind) {
        return requirement.to_string();
    }
    let rendered = Violation {
        path: String::new(),
        kind: as_rendered(kind),
        value: None,
        sensitive: false,
    }
    .message();
    match rendered.rsplit_once(CONSTRAINT_SEPARATOR) {
        Some((_, requirement)) => requirement.to_string(),
        None => rendered,
    }
}

/// The largest number of map ENTRIES a clause renders, and the largest number of bytes
/// the rendered map may carry.
///
/// The clause echoes a value the CALLER chose, so its size has to be bounded here rather
/// than inherited from the member's modelled bounds — `ArtifactProperties` admits 30
/// entries of up to 4096 bytes each, which is a ~120 KB response built out of one
/// request. Both bounds are this module's own choice, in the spirit of the framework's
/// own echo cap (`MAX_ECHOED_VALUE`, 256 bytes per clause) and of
/// `services/common/src/validation.rs`'s restored-echo ceilings: generous enough that
/// every refusal a modelled map produces today renders in full (30 entries of a name and
/// a short value), small enough that one refusal cannot carry request-sized content.
/// Past either bound the rendering is cut and the elision is MARKED, so a reader can tell
/// a cut map from a short one.
const MAX_RENDERED_ENTRIES: usize = 30;

/// See [`MAX_RENDERED_ENTRIES`].
const MAX_RENDERED_BYTES: usize = 4096;

/// What a cut rendering ends with, in place of the entries it does not carry — after the
/// entries it does carry ([`ELISION_AFTER_ENTRIES`]), or on its own when not even the
/// first entry fits ([`ELISION_ALONE`]).
const ELISION_AFTER_ENTRIES: &str = ", ...";

/// See [`ELISION_AFTER_ENTRIES`].
const ELISION_ALONE: &str = "...";

/// The whole `ValidationException` message for a MAP member whose VALUES must satisfy
/// `requirements`: the map itself is the value the clause echoes, `path` is the map
/// member, and the requirement is the value shape's COMPLETE constraint set.
///
/// SageMaker reports a map-value constraint against the map, not against the entry that
/// broke it: a caller is told which map it sent and what every value in it has to
/// satisfy, so the whole map is rendered (`{key=value, …}`) and `requirements` is listed
/// in full. Entries are listed in key order, so one request renders one way however the
/// map arrived, and the rendering is bounded by [`MAX_RENDERED_ENTRIES`] and
/// [`MAX_RENDERED_BYTES`] so a refusal is never sized by the request.
pub(crate) fn map_value_message<'entries>(
    path: &str,
    entries: impl IntoIterator<Item = (&'entries str, &'entries str)>,
    requirements: &[ViolationKind],
) -> String {
    let sorted: BTreeMap<&str, &str> = entries.into_iter().collect();
    let mut rendered = String::new();
    let mut cut = sorted.len() > MAX_RENDERED_ENTRIES;
    for (key, value) in sorted.iter().take(MAX_RENDERED_ENTRIES) {
        let entry = format!("{key}={value}");
        let separator = usize::from(!rendered.is_empty()) * 2;
        if rendered.len() + separator + entry.len() > MAX_RENDERED_BYTES {
            cut = true;
            break;
        }
        if separator != 0 {
            rendered.push_str(", ");
        }
        rendered.push_str(&entry);
    }
    if cut {
        rendered.push_str(if rendered.is_empty() {
            ELISION_ALONE
        } else {
            ELISION_AFTER_ENTRIES
        });
    }
    let stated: Vec<String> = requirements.iter().map(requirement_text).collect();
    envelope_of(&format!(
        "Value '{{{rendered}}}' at '{path}'{CONSTRAINT_SEPARATOR}{MAP_VALUE_REQUIREMENT}[{}]",
        stated.join(", ")
    ))
}

/// The validation path of the `Properties` member, as the framework's own path spelling
/// (lowerCamel) writes it — the map a property-value refusal is stated against.
const PROPERTIES_PATH: &str = "properties";

/// The lower bound a value of the `ArtifactProperties` map must satisfy. The shape its
/// values target (`smithy_query(query="shape",shape="ArtifactPropertyValue")`) states
/// `@length 0..4096`, so an empty value is inside the modelled bound and outside the one
/// the service enforces; this is that enforced minimum.
const PROPERTY_VALUE_MIN_LENGTH: i64 = 1;

/// The whole constraint set a value of the `ArtifactProperties` map must satisfy, in the
/// order the service lists it in a refusal: the modelled maximum, the enforced minimum,
/// then the modelled expression. Source for the two modelled entries:
/// `smithy_query(query="shape",shape="ArtifactPropertyValue")` — `@length 0..4096`,
/// `@pattern .*`.
fn property_value_constraints() -> [ViolationKind; 3] {
    [
        ViolationKind::MaxLength(4096),
        ViolationKind::MinLength(PROPERTY_VALUE_MIN_LENGTH),
        ViolationKind::Pattern(".*".to_string()),
    ]
}

/// How this service states an `ArtifactProperties` map a request may not send, or `None`
/// when every value in it is acceptable.
///
/// The enforced minimum is a property of the VALUE SHAPE, not of one operation, so every
/// operation carrying a map of `ArtifactPropertyValue`s — `CreateArtifact` and
/// `UpdateArtifact` — refuses the same map here rather than wording it twice. The clause
/// is stated against the MAP ([`map_value_message`]), echoing every entry of it and
/// listing the whole constraint set a property value has to satisfy.
pub(crate) fn artifact_properties_refusal(
    properties: &std::collections::HashMap<String, String>,
) -> Option<String> {
    properties.values().any(String::is_empty).then(|| {
        map_value_message(
            PROPERTIES_PATH,
            properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
            &property_value_constraints(),
        )
    })
}

/// How this service states an enum-typed member carrying a value the model does not
/// declare, or `None` when `declared` holds the value.
///
/// An enum reaches a handler as a plain string, so a value outside the enum is refused by
/// the handler that reads it rather than by the walk — and the sentence is a property of
/// the ENUM-TYPED MEMBER, not of one operation, so every handler that checks one words it
/// here instead of each stating it again. `declared` is always a generated `*_VALUES` list
/// in `crate::ops::types`, i.e. the model's own value list.
pub(crate) fn enum_member_refusal(
    member: &str,
    value: &str,
    declared: &[&str],
) -> Option<String> {
    (!declared.contains(&value)).then(|| {
        format!(
            "{member}: '{value}' is not one of the accepted values; expected one of {}.",
            declared.join(", ")
        )
    })
}

/// The `ValidationException` envelope around one clause this service stated itself.
///
/// Spelled exactly as [`aws_smithy_schema_server::envelope`] spells a single-violation
/// message, so a stated clause is framed the way a walked one is; it cannot go
/// through that function because the clause this module states is not always one the
/// framework can build from a [`Violation`] (a map-value clause reports the map, not the
/// entry the walk found, and a restated requirement is no rendering of a
/// [`ViolationKind`]).
fn envelope_of(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The order the values of an enumerated type are LISTED in, and the set they are listed
/// over.
///
/// A refusal of a value outside an enumerated type names the values that type admits, and
/// this service lists them in the order the table it holds them in is WALKED rather than in
/// the order the model declares them:
///
/// * in ascending bucket, the bucket being the low bits of each value's own string hash —
///   the 31-polynomial over its UTF-16 code units;
/// * a bucket holding more than one value read from the head each value was pushed onto, so
///   the LATER of two values that share a bucket is listed first.
///
/// Both are this tree's one derivation of that order ([`hashed_enum_order`], whose own docs
/// carry the table's width and the same chain direction read off another service's
/// recording), applied here to the values in the order the service's table was FILLED — the
/// order a row of [`STATED_VALUE_SETS`] holds, which for a set the model declares is the
/// model's own declaration order.
///
/// What shows it: a recorded `UpdateModelPackage` refusal lists `ModelApprovalStatus`'s
/// three values `[PendingManualApproval, Approved, Rejected]` where the model declares them
/// `[Approved, Rejected, PendingManualApproval]`; a recorded `CreateComputeQuota` refusal
/// lists `ResourceSharingStrategy` as `[DontLend, Lend, LendAndBorrow]` where the model
/// declares `[Lend, DontLend, LendAndBorrow]`; a recorded `CreateMlflowTrackingServer`
/// refusal lists the eight tracking-server sizes in that same order; and the recorded
/// 25-value `Resource` listing — the only recorded set of this service with two values in
/// one bucket — carries every such run against the declaration order, which is what shows
/// the chain direction. The order is a function of the VALUES alone, so one enum is listed
/// the same way in every operation that carries it.
///
/// The SET is the model's own, except for the enums [`STATED_VALUE_SETS`] holds a row for.
fn stated_order(values: &[String]) -> Vec<String> {
    let held: Vec<&str> = match stated_enum_value_set(values, STATED_VALUE_SETS) {
        Some(service) => service.to_vec(),
        None => values.iter().map(String::as_str).collect(),
    };
    hashed_enum_order(&held)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Whether `values` holds exactly the values `expected` does, in any order.
fn is_set_of(values: &[String], expected: &[&str]) -> bool {
    values.len() == expected.len()
        && expected
            .iter()
            .all(|wanted| values.iter().any(|value| value == wanted))
}

#[cfg(test)]
mod tests {
    //! The renderings this module owns, each over a SET of inputs rather than one
    //! recorded case. The end-to-end wording is pinned on the wire in
    //! `tests/all/rejection_fidelity.rs`.

    use super::*;

    /// This service's wording for `violations` alone — what the centralized refusal seam
    /// asks for, which is handed no input shape and no submitted list, so no clause is
    /// grouped at a collection member.
    fn stated_refusal(violations: &[Violation]) -> Option<Refused> {
        refusal(violations, &[], &HashMap::new())
    }

    /// A `@pattern` loses the whole-value anchor PAIR and nothing else.
    #[test]
    fn a_pattern_is_rendered_without_its_whole_value_anchors() {
        // (case, modelled expression, rendered expression)
        let rows: [(&str, &str, &str); 12] = [
            // Source: `smithy_query(query="shape",shape="ExperimentEntityName")`, the
            // target of `CreateTrialComponent$DisplayName`.
            (
                "the modelled experiment entity name",
                r"^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,119}$",
                r"[a-zA-Z0-9](-*[a-zA-Z0-9]){0,119}",
            ),
            // Source: `smithy_query(query="shape",shape="S3Uri")`.
            (
                "the modelled S3 URI",
                r"^(https|s3)://([^/]+)/?(.*)$",
                r"(https|s3)://([^/]+)/?(.*)",
            ),
            // Source: `smithy_query(query="shape",shape="ImageUri")`, the target of
            // `ContainerDefinition$Image`.
            ("a one-class expression", r"^[\S]+$", r"[\S]+"),
            ("anchored at the start only", "^arn:aws:", "^arn:aws:"),
            ("anchored at the end only", "[0-9]+$", "[0-9]+$"),
            ("no anchor at all", r"\d{3}", r"\d{3}"),
            ("an escaped trailing dollar", r"^[a-z]+\$$", r"[a-z]+\$"),
            // The pair is dropped POSITIONALLY — see
            // `service_common::validation::unanchored`, which this module reads the rule
            // from: the published expression is the service's own with that pair written
            // around it, so an alternation does not keep it (a lambdacore recording shows
            // AWS dropping it from a top-level alternation).
            ("a fully anchored alternation", "^a$|^b$", "a$|^b"),
            ("a half-anchored alternation", "^a|b$", "a|b"),
            ("an alternation inside a group", "^(a|b)$", "(a|b)"),
            ("a negated character class", r"^[^/]+$", r"[^/]+"),
            ("a class holding a literal dollar", "^[a-z$]+$", "[a-z$]+"),
        ];

        for (case, modelled, expected) in rows {
            let actual = unanchored(modelled);
            assert_eq!(
                actual, expected,
                "case {case:?} ({modelled}): expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A value set is listed in the order the table the SERVICE holds it in is walked, over
    /// the values the service holds: the recorded listing for each enum a recording covers,
    /// and the same derivation over the model's own set for every other enum.
    #[test]
    fn an_enum_value_set_is_listed_in_the_order_its_table_is_walked() {
        // (case, the set as the model declares it, the set as the service lists it)
        //
        // Rows differ along what the rule generalises over: the number of values, whether
        // the service holds values the model does not, and the order the model declares
        // them in. A row says whether its listing is one a RECORDED refusal of this service
        // carries or one the same derivation PREDICTS for a set no recording covers.
        let rows: [(&str, &[&str], &[&str]); 7] = [
            // RECORDED. Source: `smithy_query(query="shape",shape="ResourceType")`, read
            // through the generated constant — the service holds five more values, and this
            // is the listing a recorded refusal of a `Resource` outside the enum carries.
            (
                "ResourceType, as the public model declares it",
                crate::ops::types::RESOURCE_TYPE_VALUES,
                &[
                    "App",
                    "Pipeline",
                    "ModelPackageGroup",
                    "TrainingJob",
                    "InferenceComponent",
                    "HubContent",
                    "Domain",
                    "ExperimentTrialComponent",
                    "FeatureMetadata",
                    "FeatureGroup",
                    "Model",
                    "HyperParameterTuningJob",
                    "Endpoint",
                    "PipelineVersion",
                    "PipelineExecution",
                    "EndpointConfig",
                    "ModelCard",
                    "Project",
                    "ExperimentTrial",
                    "Image",
                    "Job",
                    "ImageVersion",
                    "ModelPackage",
                    "Experiment",
                    "UserProfile",
                ],
            ),
            // RECORDED: `smithy_query(query="shape",shape="ModelApprovalStatus")` declares
            // these three in this order; a recorded `UpdateModelPackage` refusal lists them
            // the other way about.
            (
                "ModelApprovalStatus, in the model's declaration order",
                &["Approved", "Rejected", "PendingManualApproval"],
                &["PendingManualApproval", "Approved", "Rejected"],
            ),
            // ...and the same listing whatever order the set reaches the clause in: the
            // order is a function of the values alone.
            (
                "ModelApprovalStatus, already in the recorded order",
                &["PendingManualApproval", "Approved", "Rejected"],
                &["PendingManualApproval", "Approved", "Rejected"],
            ),
            // RECORDED: a `CreateComputeQuota` refusal of a resource-sharing strategy
            // outside the enum (`smithy_query(query="shape",
            // shape="ResourceSharingStrategy")` declares `Lend, DontLend, LendAndBorrow`).
            (
                "ResourceSharingStrategy",
                &["Lend", "DontLend", "LendAndBorrow"],
                &["DontLend", "Lend", "LendAndBorrow"],
            ),
            // RECORDED: a `CreateMlflowTrackingServer` refusal of a size outside the enum
            // lists all eight the service holds, in this order
            // (`smithy_query(query="shape",shape="TrackingServerSize")` declares three).
            (
                "TrackingServerSize, over the eight sizes the service holds",
                crate::ops::types::TRACKING_SERVER_SIZE_VALUES,
                &[
                    "mm1.xlarge",
                    "Small",
                    "mm1.small",
                    "mm1.medium",
                    "Medium",
                    "mm1.2xlarge",
                    "Large",
                    "mm1.large",
                ],
            ),
            // PREDICTED. Source: `smithy_query(query="shape",shape="SkipModelValidation")`.
            ("SkipModelValidation", &["All", "None"], &["All", "None"]),
            // PREDICTED: one value is one listing, whatever its text.
            ("a single-value set", &["Deleting"], &["Deleting"]),
        ];

        for (case, declared, expected) in rows {
            let given: Vec<String> = declared.iter().map(|value| (*value).to_string()).collect();
            let actual = stated_order(&given);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A listing adds and drops nothing: it is the set the service holds for that enum,
    /// reordered — the model's own set for every enum the service holds no more values for.
    #[test]
    fn a_listing_holds_exactly_the_values_the_service_holds() {
        // Rows span the model's value sets by size: two values, three, twenty, and the two
        // the service holds more values for than the model declares.
        let rows: [(&str, &[&str], &[&str]); 5] = [
            (
                "SkipModelValidation",
                &["All", "None"],
                &["All", "None"],
            ),
            (
                "ModelPackageStatus, three of its members",
                &["Completed", "Failed", "Pending"],
                &["Completed", "Failed", "Pending"],
            ),
            (
                "the hosting instance types",
                crate::ops::types::PRODUCTION_VARIANT_INSTANCE_TYPE_VALUES,
                crate::ops::types::PRODUCTION_VARIANT_INSTANCE_TYPE_VALUES,
            ),
            (
                "ResourceType, over the set the service holds",
                crate::ops::types::RESOURCE_TYPE_VALUES,
                &SEARCH_RESOURCE_TYPES,
            ),
            (
                "TrackingServerSize, over the set the service holds",
                crate::ops::types::TRACKING_SERVER_SIZE_VALUES,
                &TRACKING_SERVER_SIZES,
            ),
        ];

        for (case, declared, held) in rows {
            let given: Vec<String> = declared.iter().map(|value| (*value).to_string()).collect();
            let mut listed = stated_order(&given);
            let mut expected: Vec<String> =
                held.iter().map(|value| (*value).to_string()).collect();
            listed.sort();
            expected.sort();
            assert_eq!(
                listed, expected,
                "case {case:?}: expected the listing to hold {expected:?}, got {listed:?}"
            );
        }
    }

    /// A value outside the TRAINING instance types is told the condition it failed, and no
    /// other value set is: the generic listing stands for the rest, the hosting instance
    /// types included.
    #[test]
    fn only_the_training_instance_types_are_stated_as_a_condition() {
        // (case, the value set the walk reported, the requirement this service states)
        let listed = |values: &[&str]| {
            let given: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
            format!(
                "Member must satisfy enum value set: [{}]",
                stated_order(&given).join(", ")
            )
        };
        let training: Vec<&str> = crate::ops::types::TRAINING_INSTANCE_TYPE_VALUES.to_vec();
        // Source: `smithy_query(query="shape",shape="ProductionVariantInstanceType")` and
        // the other value sets of this model, through their generated constants.
        let hosting: Vec<&str> = crate::ops::types::PRODUCTION_VARIANT_INSTANCE_TYPE_VALUES.to_vec();
        let transform: Vec<&str> = crate::ops::types::TRANSFORM_INSTANCE_TYPE_VALUES.to_vec();
        let processing: Vec<&str> = crate::ops::types::PROCESSING_INSTANCE_TYPE_VALUES.to_vec();
        // The recognition is by MEMBERS, so a set that merely overlaps the training one —
        // here every training type but the first — is not it.
        let short_of_the_set: Vec<&str> = training.iter().skip(1).copied().collect();

        let rows: [(&str, &[&str], String); 6] = [
            (
                "the training instance types",
                &training,
                SUPPORTED_INSTANCE_TYPE_REQUIREMENT.to_string(),
            ),
            ("the hosting instance types", &hosting, listed(&hosting)),
            (
                "the transform instance types",
                &transform,
                listed(&transform),
            ),
            (
                "the processing instance types",
                &processing,
                listed(&processing),
            ),
            (
                "one short of the training set",
                &short_of_the_set,
                listed(&short_of_the_set),
            ),
            (
                "a two-value set of another enum",
                &["All", "None"],
                listed(&["All", "None"]),
            ),
        ];

        for (case, values, expected) in rows {
            let found = Violation {
                path: "resourceConfig.instanceType".to_string(),
                kind: ViolationKind::EnumValue(
                    values.iter().map(|value| (*value).to_string()).collect(),
                ),
                value: Some("ml.totally.bogus".to_string()),
                sensitive: false,
            };
            let stated = stated_refusal(std::slice::from_ref(&found))
                .unwrap_or_else(|| panic!("case {case:?}: expected a reworded refusal"));
            let whole = format!(
                "1 validation error detected: Value 'ml.totally.bogus' at \
                 'resourceConfig.instanceType'{CONSTRAINT_SEPARATOR}{expected}"
            );
            assert_eq!(
                stated.message, whole,
                "case {case:?}: expected {whole:?}, got {:?}",
                stated.message
            );
        }
    }

    /// The requirement is replaced for EVERY member carrying the set, whatever the clause's
    /// subject, and the envelope still counts and joins the clauses the framework's way.
    #[test]
    fn a_restated_requirement_leaves_the_clauses_subject_and_the_envelope_alone() {
        let instance_type = |path: &str, value: &str| Violation {
            path: path.to_string(),
            kind: ViolationKind::EnumValue(
                crate::ops::types::TRAINING_INSTANCE_TYPE_VALUES
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect(),
            ),
            value: Some(value.to_string()),
            sensitive: false,
        };

        // Rows differ along the member's depth in the request, the echoed value's shape
        // (including one that spells the clause separator itself) and the clause count.
        let rows: [(&str, Vec<Violation>, String); 4] = [
            (
                "a top-level resource configuration",
                vec![instance_type("resourceConfig.instanceType", "ml.t2.medium")],
                format!(
                    "1 validation error detected: Value 'ml.t2.medium' at \
                     'resourceConfig.instanceType'{CONSTRAINT_SEPARATOR}\
                     {SUPPORTED_INSTANCE_TYPE_REQUIREMENT}"
                ),
            ),
            (
                "an instance group inside a list",
                vec![instance_type(
                    "resourceConfig.instanceGroups.1.member.instanceType",
                    "",
                )],
                format!(
                    "1 validation error detected: Value '' at \
                     'resourceConfig.instanceGroups.1.member.instanceType'\
                     {CONSTRAINT_SEPARATOR}{SUPPORTED_INSTANCE_TYPE_REQUIREMENT}"
                ),
            ),
            (
                "a value spelling the clause separator",
                vec![instance_type(
                    "resourceConfig.instanceType",
                    "x failed to satisfy constraint: y",
                )],
                format!(
                    "1 validation error detected: Value 'x failed to satisfy constraint: y' \
                     at 'resourceConfig.instanceType'{CONSTRAINT_SEPARATOR}\
                     {SUPPORTED_INSTANCE_TYPE_REQUIREMENT}"
                ),
            ),
            (
                "two members of one request",
                vec![
                    instance_type("resourceConfig.instanceType", "ml.bogus.one"),
                    instance_type(
                        "resourceConfig.instanceGroups.1.member.instanceType",
                        "ml.bogus.two",
                    ),
                ],
                format!(
                    "2 validation errors detected: Value 'ml.bogus.one' at \
                     'resourceConfig.instanceType'{CONSTRAINT_SEPARATOR}\
                     {SUPPORTED_INSTANCE_TYPE_REQUIREMENT}; Value 'ml.bogus.two' at \
                     'resourceConfig.instanceGroups.1.member.instanceType'\
                     {CONSTRAINT_SEPARATOR}{SUPPORTED_INSTANCE_TYPE_REQUIREMENT}"
                ),
            ),
        ];

        for (case, found, expected) in rows {
            let stated = stated_refusal(&found)
                .unwrap_or_else(|| panic!("case {case:?}: expected a reworded refusal"));
            assert_eq!(
                stated.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }
    }

    /// Every constraint class the framework can report keeps the framework's own clause,
    /// and only the two renderings above change.
    #[test]
    fn every_constraint_class_keeps_the_frameworks_clause() {
        // (case, the violation the framework found, the message this service states)
        let rows: [(&str, Violation, &str); 4] = [
            (
                "an enum value set",
                Violation {
                    path: "modelApprovalStatus".to_string(),
                    kind: ViolationKind::EnumValue(
                        ["Approved", "Rejected", "PendingManualApproval"]
                            .iter()
                            .map(|value| (*value).to_string())
                            .collect(),
                    ),
                    value: Some("Maybe".to_string()),
                    sensitive: false,
                },
                "1 validation error detected: Value 'Maybe' at 'modelApprovalStatus' \
                 failed to satisfy constraint: Member must satisfy enum value set: \
                 [PendingManualApproval, Approved, Rejected]",
            ),
            (
                "a pattern",
                Violation {
                    path: "transformInput.dataSource.s3DataSource.s3Uri".to_string(),
                    kind: ViolationKind::Pattern(r"^(https|s3)://([^/]+)/?(.*)$".to_string()),
                    value: Some("not-a-valid-s3-uri".to_string()),
                    sensitive: false,
                },
                "1 validation error detected: Value 'not-a-valid-s3-uri' at \
                 'transformInput.dataSource.s3DataSource.s3Uri' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: \
                 (https|s3)://([^/]+)/?(.*)",
            ),
            (
                "an absent required member",
                Violation {
                    path: "modelPackageArn".to_string(),
                    kind: ViolationKind::MissingRequiredMember,
                    value: None,
                    sensitive: false,
                },
                "1 validation error detected: Value null at 'modelPackageArn' failed to \
                 satisfy constraint: Member must not be null",
            ),
            (
                "a length bound",
                Violation {
                    path: "displayName".to_string(),
                    kind: ViolationKind::MaxLength(120),
                    value: Some("x".to_string()),
                    sensitive: false,
                },
                "1 validation error detected: Value 'x' at 'displayName' failed to \
                 satisfy constraint: Member must have length less than or equal to 120",
            ),
        ];

        for (case, found, expected) in rows {
            let stated = stated_refusal(std::slice::from_ref(&found))
                .unwrap_or_else(|| panic!("case {case:?}: expected a reworded refusal"));
            assert_eq!(
                stated.code, VALIDATION_EXCEPTION,
                "case {case:?}: expected the code {VALIDATION_EXCEPTION}, got {:?}",
                stated.code
            );
            assert_eq!(
                stated.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }
    }

    /// Nothing to word is no refusal: a request that breaks no constraint is not answered
    /// here.
    #[test]
    fn no_violations_is_no_refusal() {
        let stated = stated_refusal(&[]);
        assert!(
            stated.is_none(),
            "case no violations: expected None, got {stated:?}"
        );
    }

    /// A map-value clause echoes the WHOLE map, in key order, and lists every
    /// requirement it was given.
    #[test]
    fn a_map_value_clause_echoes_the_whole_map_and_every_requirement() {
        let requirements = [
            ViolationKind::MaxLength(4096),
            ViolationKind::MinLength(1),
            ViolationKind::Pattern(".*".to_string()),
        ];
        // Rows differ along entry count, the order the entries arrive in, and which of
        // the values is the empty one.
        type Row = (
            &'static str,
            &'static [(&'static str, &'static str)],
            &'static str,
        );
        let rows: [Row; 5] = [
            ("one entry", &[("only", "")], "{only=}"),
            (
                "two entries, the empty one first",
                &[("empty_prop", ""), ("present_prop", "some-value")],
                "{empty_prop=, present_prop=some-value}",
            ),
            (
                "the same two entries, sent in the other order",
                &[("present_prop", "some-value"), ("empty_prop", "")],
                "{empty_prop=, present_prop=some-value}",
            ),
            (
                "three entries",
                &[("a", "1"), ("c", ""), ("b", "2")],
                "{a=1, b=2, c=}",
            ),
            ("no entries at all", &[], "{}"),
        ];

        for (case, entries, rendered) in rows {
            let expected = format!(
                "1 validation error detected: Value '{rendered}' at 'properties' failed \
                 to satisfy constraint: Map value must satisfy constraint: [Member must \
                 have length less than or equal to 4096, Member must have length greater \
                 than or equal to 1, Member must satisfy regular expression pattern: .*]"
            );
            let actual = map_value_message("properties", entries.iter().copied(), &requirements);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The rendered map is BOUNDED: a map at either cap renders in full, and one past a
    /// cap is cut with the elision marked, so a refusal is never sized by the request.
    #[test]
    fn a_map_value_clause_is_bounded_by_its_own_caps() {
        let requirements = [ViolationKind::MinLength(1)];
        // Keys and values sized so the byte cap is reached by entry COUNT in one row and
        // by entry SIZE in another: "k<nn>=<v…>" entries of a known width.
        let entry_text = |index: usize, value_len: usize| {
            (format!("k{index:03}"), "v".repeat(value_len))
        };

        // (case, entries, whether the rendering must be cut)
        let at_the_entry_cap: Vec<(String, String)> =
            (0..MAX_RENDERED_ENTRIES).map(|i| entry_text(i, 1)).collect();
        let past_the_entry_cap: Vec<(String, String)> = (0..MAX_RENDERED_ENTRIES + 1)
            .map(|i| entry_text(i, 1))
            .collect();
        // Two entries whose renderings together exceed the byte cap, the first alone
        // fitting: the rendering carries the first and marks the elision after it.
        let past_the_byte_cap: Vec<(String, String)> = (0..2)
            .map(|i| entry_text(i, MAX_RENDERED_BYTES / 2))
            .collect();
        // One entry that alone exceeds the byte cap: nothing fits, so the rendering is
        // the elision on its own rather than a leading separator.
        let first_entry_past_the_cap: Vec<(String, String)> =
            vec![entry_text(0, MAX_RENDERED_BYTES + 1)];

        type BoundRow<'r> = (&'r str, &'r Vec<(String, String)>, bool, &'r str);
        let rows: [BoundRow<'_>; 4] = [
            ("at the entry cap", &at_the_entry_cap, false, ""),
            (
                "one entry past the cap",
                &past_the_entry_cap,
                true,
                ELISION_AFTER_ENTRIES,
            ),
            (
                "past the byte cap",
                &past_the_byte_cap,
                true,
                ELISION_AFTER_ENTRIES,
            ),
            (
                "the first entry alone past the byte cap",
                &first_entry_past_the_cap,
                true,
                ELISION_ALONE,
            ),
        ];

        for (case, entries, must_be_cut, marker) in rows {
            let borrowed: Vec<(&str, &str)> = entries
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let actual = map_value_message("properties", borrowed, &requirements);
            assert_eq!(
                actual.contains(ELISION_ALONE),
                must_be_cut,
                "case {case:?}: expected cut={must_be_cut}, got {actual:?}"
            );
            if must_be_cut {
                assert!(
                    actual.contains(&format!("{marker}}}")),
                    "case {case:?}: expected the rendering to end in {marker:?}, \
                     got {actual:?}"
                );
            }
            // Whatever the request, the clause stays within the byte cap plus the frame
            // this module writes round it.
            assert!(
                actual.len() < MAX_RENDERED_BYTES * 2,
                "case {case:?}: the clause must stay bounded, got {} bytes",
                actual.len()
            );
        }
    }

    /// The clauses of one refusal come out in the order the findings table is walked — a
    /// function of the member names alone, so the order does not depend on the order the
    /// walk found them or on the order the model declares them.
    #[test]
    fn the_clauses_come_out_in_the_findings_tables_own_order() {
        let at = |path: &str| Violation {
            path: path.to_string(),
            kind: ViolationKind::Pattern("^.*$".to_string()),
            value: Some("x".to_string()),
            sensitive: false,
        };

        // (case, the paths as the walk found them, the paths as the refusal states them).
        // Rows differ along the member set, the order the walk found them in, the number of
        // members, and a path with a nested member or a list element in it.
        let rows: [(&str, &[&str], &[&str]); 6] = [
            // RECORDED: an `AttachClusterNodeVolume` whose three members all break their
            // expressions. The walk reports them in the input shape's own member order
            // (`smithy_query(query="operation",operation="AttachClusterNodeVolume")`:
            // `ClusterArn`, `NodeId`, `VolumeId`).
            (
                "the three members of a volume attach",
                &["clusterArn", "nodeId", "volumeId"],
                &["clusterArn", "volumeId", "nodeId"],
            ),
            // ...and the same message whatever order they reached the envelope in.
            (
                "the same three members, found the other way round",
                &["volumeId", "nodeId", "clusterArn"],
                &["clusterArn", "volumeId", "nodeId"],
            ),
            ("one member alone", &["nodeId"], &["nodeId"]),
            // PREDICTED: two members of one batch node call.
            (
                "a cluster name beside a node list",
                &["clusterName", "nodeIds.1.member"],
                &["nodeIds.1.member", "clusterName"],
            ),
            // PREDICTED: a clause about a list ELEMENT sits where the LIST member does, so
            // the element step does not decide its place, and two elements of one list keep
            // the order the walk found them in.
            (
                "two elements of one list and another member",
                &["nodeIds.1.member", "nodeIds.2.member", "clusterName"],
                &["nodeIds.1.member", "nodeIds.2.member", "clusterName"],
            ),
            // PREDICTED: a member of a nested structure is keyed by its own name there.
            (
                "a nested member beside a plain one",
                &["volumeId", "resourceConfig.instanceType"],
                &["resourceConfig.instanceType", "volumeId"],
            ),
        ];

        for (case, found, expected) in rows {
            let violations: Vec<Violation> = found.iter().map(|path| at(path)).collect();
            let stated: Vec<&str> = in_stated_clause_order(&violations)
                .into_iter()
                .map(|violation| violation.path.as_str())
                .collect();
            assert_eq!(
                stated, expected,
                "case {case:?}: expected the clauses in {expected:?}, got {stated:?}"
            );
        }
    }

    /// A constraint broken by a list's ELEMENTS is stated once at the LIST member, quoting
    /// the list the request sent and nesting the element shape's whole declared set — and
    /// two offending elements of one list are ONE validation error.
    #[test]
    fn a_broken_element_constraint_is_stated_at_the_list_member() {
        // The element shape's whole set, as the model declares it
        // (`smithy_query(query="shape",shape="ClusterNodeIds")`: a list of `ClusterNodeId`,
        // `@length 1..256`, `@pattern ^i-[a-f0-9]{8}(?:[a-f0-9]{9})?$`), closing with the
        // requirement that an element be present at all.
        let node_id_set = "Member must satisfy constraint: [Member must have length less than \
                           or equal to 256, Member must have length greater than or equal to 1, \
                           Member must satisfy regular expression pattern: \
                           i-[a-f0-9]{8}(?:[a-f0-9]{9})?, Member must not be null]";
        let schemas: [&'static Schema; 1] =
            [crate::ops::types::BATCH_DELETE_CLUSTER_NODES_INPUT_SCHEMA];
        let element = |position: usize, value: &str| Violation {
            path: format!("nodeIds.{position}.member"),
            kind: ViolationKind::Pattern(r"^i-[a-f0-9]{8}(?:[a-f0-9]{9})?$".to_string()),
            value: Some(value.to_string()),
            sensitive: false,
        };

        // (case, the violations the walk found, the list the request carried, the message).
        // Rows differ along how many elements offend, how many the list holds, and whether
        // the request's list reached the renderer at all.
        type Row = (&'static str, Vec<Violation>, Vec<(&'static str, &'static str)>, String);
        let rows: [Row; 4] = [
            (
                "one element of a one-element list",
                vec![element(1, "not-an-instance")],
                vec![("nodeIds", "[not-an-instance]")],
                format!(
                    "1 validation error detected: Value '[not-an-instance]' at \
                     'nodeIds'{CONSTRAINT_SEPARATOR}{node_id_set}"
                ),
            ),
            (
                "two offending elements of one list, which is one validation error",
                vec![element(1, "first-bad"), element(2, "second-bad")],
                vec![("nodeIds", "[first-bad, second-bad]")],
                format!(
                    "1 validation error detected: Value '[first-bad, second-bad]' at \
                     'nodeIds'{CONSTRAINT_SEPARATOR}{node_id_set}"
                ),
            ),
            (
                "one offending element beside a well-formed one: the whole list is quoted",
                vec![element(2, "bad-one")],
                vec![("nodeIds", "[i-0123456789abcdef0, bad-one]")],
                format!(
                    "1 validation error detected: Value '[i-0123456789abcdef0, bad-one]' at \
                     'nodeIds'{CONSTRAINT_SEPARATOR}{node_id_set}"
                ),
            ),
            (
                "no list to quote: the member and the requirement, with no value",
                vec![element(1, "not-an-instance")],
                vec![],
                format!(
                    "1 validation error detected: Value at 'nodeIds'\
                     {CONSTRAINT_SEPARATOR}{node_id_set}"
                ),
            ),
        ];

        for (case, violations, lists, expected) in rows {
            let submitted: HashMap<String, String> = lists
                .into_iter()
                .map(|(member, rendered)| (member.to_string(), rendered.to_string()))
                .collect();
            let stated = refusal(&violations, &schemas, &submitted)
                .unwrap_or_else(|| panic!("case {case:?}: expected a stated refusal"));
            assert_eq!(
                stated.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }
    }

    /// Without an input shape to read the element set off — the centralized seam, which is
    /// handed the violations alone — a list element keeps the framework's own per-element
    /// clause.
    #[test]
    fn an_element_clause_the_set_is_unknown_for_stays_the_frameworks() {
        let found = Violation {
            path: "nodeIds.1.member".to_string(),
            kind: ViolationKind::Pattern(r"^i-[a-f0-9]{8}(?:[a-f0-9]{9})?$".to_string()),
            value: Some("not-an-instance".to_string()),
            sensitive: false,
        };
        let stated = stated_refusal(std::slice::from_ref(&found))
            .expect("a violation is always stated somehow");
        let expected = "1 validation error detected: Value 'not-an-instance' at \
                        'nodeIds.1.member' failed to satisfy constraint: Member must satisfy \
                        regular expression pattern: i-[a-f0-9]{8}(?:[a-f0-9]{9})?";
        assert_eq!(
            stated.message, expected,
            "expected the framework's own element clause, got {:?}",
            stated.message
        );
    }
}
