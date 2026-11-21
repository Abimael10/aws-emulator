// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a request its front door refused before any handler ran.
//!
//! The framework checks a request against the constraints the model declares and hands the
//! service the refusal STRUCTURALLY — one [`Violation`] per offending position, carrying the
//! member path it sits at, the constraint class it broke and the value it held — and leaves
//! the words to the service, because no model records them. [`refused`] is those words,
//! reached from [`crate::service`]'s `override_refusal`, the one seam that sees a refusal
//! taken before dispatch.
//!
//! Four rules distinguish this service's wording from the framework's default. Rules 1, 2 and 4
//! are keyed to nothing at all, so each holds for every constrained member of every operation;
//! rule 3 is keyed to the enumerated TYPE, because what a requirement says a type admits is a
//! property of that type and not of a member, an operation or a depth:
//!
//! 1. a clause names the offending MEMBER PATH and the CONSTRAINT it broke, and NOTHING of the
//!    value the request carried — not the submitted bytes, and not the `null` the framework
//!    writes where a member was absent. The requirement states what every value of that member
//!    has to satisfy, which the one value that missed it does not help a caller with, so the
//!    sentence is the same whatever was submitted and a client matching it matches for every bad
//!    input rather than for one;
//! 2. a constraint the model declares on the ELEMENTS of a collection member is stated at that
//!    MEMBER — no element position and no `member` step — with every requirement the element
//!    shape declares nested inside `Member must satisfy constraint: [ … ]`. The whole set, not
//!    just the constraint that happened to fail: a caller correcting the request is told what
//!    each element of that member has to satisfy. A constraint the COLLECTION itself broke (its
//!    own `@length`), and one on a member inside a structure element, keep their own path and
//!    their own requirement;
//! 3. an enumerated type's value set is listed in the order this front door's own table of it
//!    is walked, which is neither the model's declaration order nor sorted, over the values the
//!    service PUBLISHES for that type ([`PUBLISHED_ENUM_VALUE_SETS`] holds the three whose
//!    published membership is not the shipped model's);
//! 4. the clauses of one message are ordered by the MEMBER PATH each is stated at, not by the
//!    order the validating walk found them in, so a request breaking two members' constraints
//!    is refused with the same sentence whichever order it listed those members in.
//!
//! Rules 1, 2 and 4 are AWS's own wording rather than this service's, and all three are
//! implemented once for the fleet in [`service_common::validation_message`], which this module
//! DELEGATES to rather than rewording: the envelope and its error count, the member paths, the
//! element sets, the clause order and every requirement sentence stay the framework's and that
//! module's, as does the order rule 3 lists a set in and the `@pattern` rendering (the
//! expression with the whole-value anchors the model wraps it in removed — the CHECK always
//! uses the model's pattern as written). What this service states, and all it states, is WHICH
//! values each of those three types admits, because that is a fact about this service.
//!
//! An element set is read off the schemas codegen derives from the model, and a member schema
//! carries its own traits and not the members of the shape it targets — so a collection member
//! of an operation INPUT is looked up by its own name on that input, and one nested inside a
//! structure or a union variant by its own name on the shape that DECLARES it
//! ([`NESTED_COLLECTION_SHAPES`]). Rule 2 is about the member a constraint is declared at and
//! not about where that member sits, so it holds at every depth.
//!
//! And one limit of rule 3: a type with no row of its own is listed from the values its model
//! declares, which is right for a type nothing is known to publish differently. A recorded
//! refusal that lists a value the shipped enum does not declare, or omits one it does, is what
//! settles a row — not a reading of the model, which by then is the half that is behind.

use ::aws_smithy_schema::Schema;
use ::aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use ::service_common::{validation, validation_message};
use ::smithy_server_runtime::{Refusal, Refused};

/// `AggregationType`'s admissible values as this service PUBLISHES them: the shipped model
/// declares one more (`smithy_query(query="shape", shape="AggregationType")` lists
/// `CODE_REPOSITORY` beside these fourteen), which the refusal of a value outside the set does
/// not advertise.
///
/// The set a refusal LISTS is not stated here: it is the order the front door walks its own
/// table of these values ([`published`]). What the values' order here DOES decide is the one
/// thing that walk cannot: two values the table puts in the same place are read from the one
/// pushed in last, and a value is pushed in when the type DECLARES it. So the row is written
/// in the model's declaration order, which is the order the generated member SCHEMA carries an
/// enumerated type's values in — the order the validating walk reports, and therefore the order
/// a type with no row here is listed from. It is NOT the order of the convenience constant
/// beside it (`crate::ops::types::AGGREGATION_TYPE_VALUES`, which codegen alphabetises and
/// which is used here only to match the row to its type, as a SET).
const PUBLISHED_AGGREGATION_TYPES: &[&str] = &[
    "FINDING_TYPE",
    "PACKAGE",
    "TITLE",
    "REPOSITORY",
    "AMI",
    "AWS_EC2_INSTANCE",
    "AWS_ECR_CONTAINER",
    "IMAGE_LAYER",
    "ACCOUNT",
    "AWS_LAMBDA_FUNCTION",
    "LAMBDA_LAYER",
    "VM_INSTANCE",
    "CONTAINER_IMAGE",
    "SERVERLESS_FUNCTION",
];

/// `GroupKey`'s admissible values as this service PUBLISHES them: the shipped model's nine
/// (`smithy_query(query="shape", shape="GroupKey")`) plus the scan-type grouping, which the
/// refusal of a value outside the set advertises and the shipped enum does not declare.
///
/// Which values a member ADMITS is not decided here — that is the enum the member targets, so
/// the boundary still refuses a value the shipped model does not declare — and this only
/// decides what the refusal SAYS the service groups by.
///
/// In the model's declaration order, for the reason [`PUBLISHED_AGGREGATION_TYPES`] states,
/// with the value the shipped enum does not declare last.
const PUBLISHED_GROUP_KEYS: &[&str] = &[
    "SCAN_STATUS_CODE",
    "SCAN_STATUS_REASON",
    "ACCOUNT_ID",
    "RESOURCE_TYPE",
    "ECR_REPOSITORY_NAME",
    "PROVIDER",
    "PROVIDER_ACCOUNT_ID",
    "PROVIDER_REGION",
    "PROVIDER_ORG_ID",
    "SCAN_TYPE",
];

/// `SortField`'s admissible values as this service PUBLISHES them: the shipped model's
/// seventeen (`smithy_query(query="shape", shape="SortField")`) plus the two finding detail
/// fields the findings surface also sorts by — how recently a finding's package was last in
/// use, and how many times it is in use — which the refusal of a value outside the set
/// advertises and the shipped enum does not declare.
///
/// Which values a member ADMITS is not decided here — that is the enum the member targets, so
/// the boundary still refuses a value the shipped model does not declare — and this only
/// decides what the refusal SAYS the findings surface sorts by.
///
/// In the model's declaration order, for the reason [`PUBLISHED_AGGREGATION_TYPES`] states,
/// with the two values the shipped enum does not declare last, which is where a release that
/// adds a value to a type declares it.
const PUBLISHED_SORT_FIELDS: &[&str] = &[
    "AWS_ACCOUNT_ID",
    "FINDING_TYPE",
    "SEVERITY",
    "FIRST_OBSERVED_AT",
    "LAST_OBSERVED_AT",
    "FINDING_STATUS",
    "RESOURCE_TYPE",
    "ECR_IMAGE_PUSHED_AT",
    "ECR_IMAGE_REPOSITORY_NAME",
    "ECR_IMAGE_REGISTRY",
    "NETWORK_PROTOCOL",
    "COMPONENT_TYPE",
    "VULNERABILITY_ID",
    "VULNERABILITY_SOURCE",
    "INSPECTOR_SCORE",
    "VENDOR_SEVERITY",
    "EPSS_SCORE",
    "LAST_IN_USE_AT",
    "IN_USE_COUNT",
];

/// The enumerated types whose admissible set this service publishes other than the shipped
/// model declares it: one row per type, the model's declared values beside the service's.
///
/// Matched as a SET, so every member targeting one of these types — in any operation, in
/// whatever binding carried the value, at any depth — is stated alike, and a type with no row
/// here keeps the values its model declares.
const PUBLISHED_ENUM_VALUE_SETS: &validation_message::StatedEnumValueSets<'static> = &[
    (
        crate::ops::types::AGGREGATION_TYPE_VALUES,
        PUBLISHED_AGGREGATION_TYPES,
    ),
    (crate::ops::types::GROUP_KEY_VALUES, PUBLISHED_GROUP_KEYS),
    (crate::ops::types::SORT_FIELD_VALUES, PUBLISHED_SORT_FIELDS),
];

/// Every operation INPUT shape of this service, as the schemas codegen derives from the
/// Smithy model.
///
/// What they are read for: rule 2 states the whole constraint set an element shape declares,
/// and that set is a property of the model rather than of one request — a refusal carries only
/// the constraints a value actually broke. Codegen attaches a collection member's element
/// constraints to that member's schema, which is where the shared rendering reads them, and
/// where the walk read them to refuse the request.
///
/// ALL of them, not the ones one refusal happens to name: the rule holds for every constrained
/// collection of every operation. `grep -h 'super::types::.*_INPUT_SCHEMA' src/ops/*.rs` names
/// exactly these, one per operation file; one missing from this list is not a wrong clause,
/// only a clause stating the constraint that failed rather than the element shape's whole set.
const INPUT_SCHEMAS: &[&Schema] = {
    use crate::ops::types;

    &[
        types::ASSOCIATE_MEMBER_INPUT_SCHEMA,
        types::BATCH_ASSOCIATE_CODE_SECURITY_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::BATCH_DISASSOCIATE_CODE_SECURITY_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::BATCH_GET_ACCOUNT_STATUS_INPUT_SCHEMA,
        types::BATCH_GET_CODE_SNIPPET_INPUT_SCHEMA,
        types::BATCH_GET_FINDING_DETAILS_INPUT_SCHEMA,
        types::BATCH_GET_FREE_TRIAL_INFO_INPUT_SCHEMA,
        types::BATCH_GET_MEMBER_EC2_DEEP_INSPECTION_STATUS_INPUT_SCHEMA,
        types::BATCH_UPDATE_MEMBER_EC2_DEEP_INSPECTION_STATUS_INPUT_SCHEMA,
        types::CANCEL_FINDINGS_REPORT_INPUT_SCHEMA,
        types::CANCEL_SBOM_EXPORT_INPUT_SCHEMA,
        types::CREATE_CIS_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::CREATE_CODE_SECURITY_INTEGRATION_INPUT_SCHEMA,
        types::CREATE_CODE_SECURITY_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::CREATE_CONNECTOR_INPUT_SCHEMA,
        types::CREATE_FILTER_INPUT_SCHEMA,
        types::CREATE_FINDINGS_REPORT_INPUT_SCHEMA,
        types::CREATE_SBOM_EXPORT_INPUT_SCHEMA,
        types::DELETE_CIS_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::DELETE_CODE_SECURITY_INTEGRATION_INPUT_SCHEMA,
        types::DELETE_CODE_SECURITY_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::DELETE_CONNECTOR_INPUT_SCHEMA,
        types::DELETE_FILTER_INPUT_SCHEMA,
        types::DESCRIBE_ORGANIZATION_CONFIGURATION_INPUT_SCHEMA,
        types::DISABLE_DELEGATED_ADMIN_ACCOUNT_INPUT_SCHEMA,
        types::DISABLE_INPUT_SCHEMA,
        types::DISASSOCIATE_MEMBER_INPUT_SCHEMA,
        types::ENABLE_DELEGATED_ADMIN_ACCOUNT_INPUT_SCHEMA,
        types::ENABLE_INPUT_SCHEMA,
        types::GET_CIS_SCAN_REPORT_INPUT_SCHEMA,
        types::GET_CIS_SCAN_RESULT_DETAILS_INPUT_SCHEMA,
        types::GET_CLUSTERS_FOR_IMAGE_INPUT_SCHEMA,
        types::GET_CODE_SECURITY_INTEGRATION_INPUT_SCHEMA,
        types::GET_CODE_SECURITY_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::GET_CODE_SECURITY_SCAN_INPUT_SCHEMA,
        types::GET_CONFIGURATION_INPUT_SCHEMA,
        types::GET_DELEGATED_ADMIN_ACCOUNT_INPUT_SCHEMA,
        types::GET_EC2_DEEP_INSPECTION_CONFIGURATION_INPUT_SCHEMA,
        types::GET_ENCRYPTION_KEY_INPUT_SCHEMA,
        types::GET_FINDINGS_REPORT_STATUS_INPUT_SCHEMA,
        types::GET_MEMBER_INPUT_SCHEMA,
        types::GET_SBOM_EXPORT_INPUT_SCHEMA,
        types::LIST_ACCOUNT_PERMISSIONS_INPUT_SCHEMA,
        types::LIST_CIS_SCAN_CONFIGURATIONS_INPUT_SCHEMA,
        types::LIST_CIS_SCAN_RESULTS_AGGREGATED_BY_CHECKS_INPUT_SCHEMA,
        types::LIST_CIS_SCAN_RESULTS_AGGREGATED_BY_TARGET_RESOURCE_INPUT_SCHEMA,
        types::LIST_CIS_SCANS_INPUT_SCHEMA,
        types::LIST_CODE_SECURITY_INTEGRATIONS_INPUT_SCHEMA,
        types::LIST_CODE_SECURITY_SCAN_CONFIGURATION_ASSOCIATIONS_INPUT_SCHEMA,
        types::LIST_CODE_SECURITY_SCAN_CONFIGURATIONS_INPUT_SCHEMA,
        types::LIST_CONNECTOR_SCAN_CONFIGURATIONS_INPUT_SCHEMA,
        types::LIST_CONNECTORS_INPUT_SCHEMA,
        types::LIST_COVERAGE_INPUT_SCHEMA,
        types::LIST_COVERAGE_STATISTICS_INPUT_SCHEMA,
        types::LIST_DELEGATED_ADMIN_ACCOUNTS_INPUT_SCHEMA,
        types::LIST_FILTERS_INPUT_SCHEMA,
        types::LIST_FINDING_AGGREGATIONS_INPUT_SCHEMA,
        types::LIST_FINDINGS_INPUT_SCHEMA,
        types::LIST_MEMBERS_INPUT_SCHEMA,
        types::LIST_TAGS_FOR_RESOURCE_INPUT_SCHEMA,
        types::LIST_USAGE_TOTALS_INPUT_SCHEMA,
        types::RESET_ENCRYPTION_KEY_INPUT_SCHEMA,
        types::SEARCH_VULNERABILITIES_INPUT_SCHEMA,
        types::SEND_CIS_SESSION_HEALTH_INPUT_SCHEMA,
        types::SEND_CIS_SESSION_TELEMETRY_INPUT_SCHEMA,
        types::START_CIS_SESSION_INPUT_SCHEMA,
        types::START_CODE_SECURITY_SCAN_INPUT_SCHEMA,
        types::STOP_CIS_SESSION_INPUT_SCHEMA,
        types::TAG_RESOURCE_INPUT_SCHEMA,
        types::UNTAG_RESOURCE_INPUT_SCHEMA,
        types::UPDATE_CIS_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::UPDATE_CODE_SECURITY_INTEGRATION_INPUT_SCHEMA,
        types::UPDATE_CODE_SECURITY_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::UPDATE_CONFIGURATION_INPUT_SCHEMA,
        types::UPDATE_CONNECTOR_INPUT_SCHEMA,
        types::UPDATE_CONNECTOR_SCAN_CONFIGURATION_INPUT_SCHEMA,
        types::UPDATE_EC2_DEEP_INSPECTION_CONFIGURATION_INPUT_SCHEMA,
        types::UPDATE_ENCRYPTION_KEY_INPUT_SCHEMA,
        types::UPDATE_FILTER_INPUT_SCHEMA,
        types::UPDATE_ORG_EC2_DEEP_INSPECTION_CONFIGURATION_INPUT_SCHEMA,
        types::UPDATE_ORGANIZATION_CONFIGURATION_INPUT_SCHEMA,
    ]
};

/// The shapes this service's operation inputs NEST that declare a collection member of
/// constrained SCALARS — the members a clause of rule 2's class can be stated at below the
/// top level.
///
/// What they are read for: the element shape's constraint set is looked up by the member a
/// clause is stated at, and codegen attaches a collection member's element constraints to that
/// member's own schema — which, for a member of a nested structure or a union variant, lives
/// on THAT shape's schema rather than on the operation input's. So a nested member is looked
/// up by its OWN name here, which is also why a shape whose collection members hold
/// STRUCTURES (every one of this service's filter criteria) is absent: a structure element
/// declares no constraints of its own, and its own members keep their own paths.
///
/// A member several of these declare with the same element set reads the same either way, and
/// one they declare differently resolves to nothing, which leaves the framework's own clause.
/// `grep -n 'Vec<String>' src/ops/types/*.rs` over the shapes an operation INPUT reaches names
/// exactly these; one missing from this list is not a wrong clause, only a clause stated at the
/// offending element rather than at its member.
const NESTED_COLLECTION_SHAPES: &[&Schema] = {
    use crate::ops::types;

    &[
        // A code-security configuration's rule-set categories and continuous-integration
        // events.
        types::CODE_SECURITY_SCAN_CONFIGURATION_SCHEMA,
        types::CONTINUOUS_INTEGRATION_SCAN_CONFIGURATION_SCHEMA,
        // A connector's Azure regions and the subscription scope each scanning type covers.
        types::AZURE_PROVIDER_DETAIL_CREATE_SCHEMA,
        types::AZURE_PROVIDER_DETAIL_UPDATE_SCHEMA,
        types::SCOPE_CONFIGURATION_INPUT_SCHEMA,
        // A CIS scan configuration's target accounts and the days of its weekly schedule.
        types::CREATE_CIS_TARGETS_SCHEMA,
        types::UPDATE_CIS_TARGETS_SCHEMA,
        types::WEEKLY_SCHEDULE_SCHEMA,
        // The vulnerability ids a vulnerability search names.
        types::SEARCH_VULNERABILITIES_FILTER_CRITERIA_SCHEMA,
    ]
};

/// The requirement a clause stated at the collection member `collection` nests (rule 2): the
/// element shape's whole declared constraint set, inside
/// `Member must satisfy constraint: [ … ]`, or `None` where no handed-over shape declares a
/// constraint on that member's elements — which leaves the clause the framework's own.
///
/// The constraints are the shared reading of the schemas
/// ([`validation_message::element_constraint_set`]), the model's constraint for constraint, and
/// the nesting is that module's too; what this adds is the two renderings this service states a
/// requirement with wherever it states one — a `@pattern` without the whole-value anchors (the
/// module header above), and an enumerated set as rule 3 lists it — so a requirement nested in
/// a collection's clause and the same requirement on a member of its own read alike.
///
/// A member of an operation INPUT is looked up by its whole path, and one nested inside a
/// structure or a union variant by its own member name on the shapes this service nests
/// ([`NESTED_COLLECTION_SHAPES`]), since that is the schema codegen attached its element
/// constraints to.
fn stated_element_set(collection: &str) -> Option<String> {
    let declared =
        validation_message::element_constraint_set(INPUT_SCHEMAS, collection).or_else(|| {
            let (_, nested) = collection.rsplit_once('.')?;
            validation_message::element_constraint_set(NESTED_COLLECTION_SHAPES, nested)
        })?;
    let requirements: Vec<String> = declared.into_iter().map(stated_requirement).collect();
    Some(validation_message::element_set_requirement(&requirements))
}

/// One constraint of an element shape as this service states that requirement: the framework's
/// own sentence ([`validation_message::requirement`]) over the two renderings this service
/// chooses — the expression without the model's whole-value anchors, and an enumerated set in
/// the order rule 3 lists it.
fn stated_requirement(kind: ViolationKind) -> String {
    let stated = match kind {
        // The CHECK always used the model's expression as written; only the quoting is this
        // service's.
        ViolationKind::Pattern(pattern) => ViolationKind::Pattern(validation::unanchored(&pattern)),
        ViolationKind::EnumValue(values) => ViolationKind::EnumValue(stated_enum_values(&values)),
        other => other,
    };
    validation_message::requirement(stated)
}

/// `declared` as this service lists an enumerated type's value set (rule 3), for a caller
/// holding the values rather than a [`Violation`].
///
/// Routed through [`published`] rather than restating rule 3, so a set nested inside a
/// collection's requirement and a set stated at a member of its own cannot drift apart.
fn stated_enum_values(declared: &[String]) -> Vec<String> {
    let listed = published(&Violation {
        path: String::new(),
        kind: ViolationKind::EnumValue(declared.to_vec()),
        value: None,
        sensitive: false,
    });
    match listed.kind {
        ViolationKind::EnumValue(values) => values,
        _ => declared.to_vec(),
    }
}

/// `violation` with its enumerated value set as this service states one (rule 3): the values
/// the service publishes, in the order its front door's own table of them is walked.
///
/// Only the values move. The member path, the value half and the requirement's wording keep
/// coming from the one place that writes them, and a violation of any other class comes back
/// untouched.
fn published(violation: &Violation) -> Violation {
    let published = validation_message::in_stated_enum_order(violation, PUBLISHED_ENUM_VALUE_SETS);
    validation_message::in_hashed_enum_order(&published)
}

/// How this service states `refusal`, or `None` to keep the framework's wording.
///
/// Only an input-constraint refusal is reworded: an authentication refusal is a different
/// failure with its own status and code, and nothing about it is this service's to word.
///
/// Rules 3 and 4 are applied to the violations BEFORE the clauses are rendered, so the clause
/// itself, its member path and the words around the value set stay the framework's; rule 1 and
/// the element set rule 2 nests are the choices the shared entry point takes
/// ([`stated_element_set`] for the second).
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    let stated: Vec<Violation> = violations.iter().map(published).collect();
    // Rule 4: the clauses of one message are ordered by the member path they are stated at.
    // The order is the shared module's, so it cannot drift from the services that state it.
    let ordered: Vec<Violation> = validation_message::in_member_path_order(&stated)
        .into_iter()
        .cloned()
        .collect();
    validation_message::aws_validation_refusal_over_elements_stating_withholding(
        &Refusal::Invalid(ordered),
        // This service quotes a `@pattern` requirement without the whole-value anchors the
        // model wraps the expression in.
        validation::unanchored,
        // Rule 1: no clause of this service's refusals states a value, whichever member it is
        // about, so the decision is the same for every member path.
        |_| true,
        // Rule 2: the element shape's whole set, at whatever depth the collection member sits.
        stated_element_set,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        published, refused, PUBLISHED_AGGREGATION_TYPES, PUBLISHED_GROUP_KEYS,
        PUBLISHED_SORT_FIELDS,
    };
    use ::aws_smithy_schema_server::constraints::{Violation, ViolationKind};
    use ::smithy_server_runtime::Refusal;

    /// One violation of `kind` at `path`, holding `value`.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_owned(),
            kind,
            value: value.map(str::to_owned),
            sensitive: false,
        }
    }

    /// An `EnumValue` violation at `path` over the values `declared`.
    fn enum_violation(path: &str, declared: &[&str], value: &str) -> Violation {
        violation(
            path,
            ViolationKind::EnumValue(declared.iter().map(|v| (*v).to_owned()).collect()),
            Some(value),
        )
    }

    /// RULE 3: an enumerated type's set is listed in the order this front door's own table of
    /// it is walked, over the values the service publishes — asserted for every type one of
    /// this service's refusals is recorded for, including the two whose published membership
    /// is not the shipped model's.
    #[test]
    fn an_enumerated_set_is_listed_as_the_service_lists_it() {
        // (case, the values the walk carried, the set as the service states it)
        let rows: &[(&str, &[&str], &str)] = &[
            (
                "the aggregation types, which the service publishes narrower",
                crate::ops::types::AGGREGATION_TYPE_VALUES,
                "[AWS_EC2_INSTANCE, PACKAGE, REPOSITORY, ACCOUNT, IMAGE_LAYER, \
                 AWS_LAMBDA_FUNCTION, FINDING_TYPE, AWS_ECR_CONTAINER, TITLE, LAMBDA_LAYER, \
                 SERVERLESS_FUNCTION, CONTAINER_IMAGE, VM_INSTANCE, AMI]",
            ),
            (
                "the coverage group keys, which the service publishes wider",
                crate::ops::types::GROUP_KEY_VALUES,
                "[PROVIDER_REGION, PROVIDER_ORG_ID, RESOURCE_TYPE, ACCOUNT_ID, \
                 SCAN_STATUS_REASON, PROVIDER, ECR_REPOSITORY_NAME, SCAN_STATUS_CODE, \
                 PROVIDER_ACCOUNT_ID, SCAN_TYPE]",
            ),
            (
                "the findings sort fields, which the service publishes wider",
                crate::ops::types::SORT_FIELD_VALUES,
                "[LAST_OBSERVED_AT, FINDING_STATUS, NETWORK_PROTOCOL, INSPECTOR_SCORE, \
                 ECR_IMAGE_PUSHED_AT, RESOURCE_TYPE, ECR_IMAGE_REGISTRY, AWS_ACCOUNT_ID, \
                 FINDING_TYPE, ECR_IMAGE_REPOSITORY_NAME, VENDOR_SEVERITY, FIRST_OBSERVED_AT, \
                 LAST_IN_USE_AT, COMPONENT_TYPE, IN_USE_COUNT, SEVERITY, EPSS_SCORE, \
                 VULNERABILITY_SOURCE, VULNERABILITY_ID]",
            ),
            (
                "the findings filter comparisons",
                crate::ops::types::STRING_COMPARISON_VALUES,
                "[NOT_EQUALS, PREFIX, EQUALS]",
            ),
            (
                "the coverage filter comparisons, which admit no prefix match",
                crate::ops::types::COVERAGE_STRING_COMPARISON_VALUES,
                "[NOT_EQUALS, EQUALS]",
            ),
        ];

        for (case, declared, expected) in rows {
            let stated = published(&enum_violation("member", declared, "nope")).message();
            assert!(
                stated.ends_with(*expected),
                "case {case:?}: expected the set stated as {expected}, got {stated:?}"
            );
            assert!(
                stated.starts_with("Value 'nope' at 'member' failed to satisfy constraint: "),
                "case {case:?}: only the value set moves, got {stated:?}"
            );
        }
    }

    /// RULE 3: the published membership is the service's, not the model's — one type drops a
    /// value the model declares and one gains a value the model does not.
    #[test]
    fn the_published_membership_is_the_services_own() {
        assert!(
            !PUBLISHED_AGGREGATION_TYPES.contains(&"CODE_REPOSITORY")
                && PUBLISHED_AGGREGATION_TYPES.len()
                    == crate::ops::types::AGGREGATION_TYPE_VALUES.len() - 1,
            "the published aggregation types are the model's without the repository one, got \
             {PUBLISHED_AGGREGATION_TYPES:?}"
        );
        assert!(
            PUBLISHED_GROUP_KEYS.contains(&"SCAN_TYPE")
                && PUBLISHED_GROUP_KEYS.len() == crate::ops::types::GROUP_KEY_VALUES.len() + 1,
            "the published group keys are the model's plus the scan-type one, got \
             {PUBLISHED_GROUP_KEYS:?}"
        );
        assert!(
            PUBLISHED_SORT_FIELDS.contains(&"LAST_IN_USE_AT")
                && PUBLISHED_SORT_FIELDS.contains(&"IN_USE_COUNT")
                && PUBLISHED_SORT_FIELDS.len()
                    == crate::ops::types::SORT_FIELD_VALUES.len() + 2,
            "the published sort fields are the model's plus the two in-use ones, got \
             {PUBLISHED_SORT_FIELDS:?}"
        );
    }

    /// RULE 1: a clause states no value at all — neither the bytes the request submitted nor
    /// the `null` the walk reports an absent member with — whatever class it is about.
    #[test]
    fn a_clause_states_no_value() {
        // (case, the violations one refusal collected, the message stated)
        let rows: &[(&str, &[Violation], &str)] = &[
            (
                "an expression the value does not match",
                &[violation(
                    "reportId",
                    ViolationKind::Pattern(r"^[a-f0-9]{8}$".to_owned()),
                    Some("not-a-report"),
                )],
                "1 validation error detected: Value at 'reportId' failed to satisfy constraint: \
                 Member must satisfy regular expression pattern: [a-f0-9]{8}",
            ),
            (
                "a value outside its enumerated set",
                &[enum_violation(
                    "groupBy",
                    crate::ops::types::GROUP_KEY_VALUES,
                    "resourceType",
                )],
                "1 validation error detected: Value at 'groupBy' failed to satisfy constraint: \
                 Member must satisfy enum value set: [PROVIDER_REGION, PROVIDER_ORG_ID, \
                 RESOURCE_TYPE, ACCOUNT_ID, SCAN_STATUS_REASON, PROVIDER, ECR_REPOSITORY_NAME, \
                 SCAN_STATUS_CODE, PROVIDER_ACCOUNT_ID, SCAN_TYPE]",
            ),
            (
                "a value past its length bound",
                &[violation("arn", ViolationKind::MaxLength(128), Some("xxx"))],
                "1 validation error detected: Value at 'arn' failed to satisfy constraint: \
                 Member must have length less than or equal to 128",
            ),
            (
                "a value outside its numeric range",
                &[violation(
                    "maxResults",
                    ViolationKind::MaxValue("100".to_owned()),
                    Some("999"),
                )],
                "1 validation error detected: Value at 'maxResults' failed to satisfy \
                 constraint: Member must have value less than or equal to 100",
            ),
            (
                "a member the request did not carry at all",
                &[violation("kmsKeyId", ViolationKind::MissingRequiredMember, None)],
                "1 validation error detected: Value at 'kmsKeyId' failed to satisfy constraint: \
                 Member must not be null",
            ),
            (
                "two members at once, ordered by their paths and not by the walk",
                &[
                    violation("kmsKeyId", ViolationKind::MissingRequiredMember, None),
                    violation(
                        "accountId",
                        ViolationKind::Pattern(r"^\d{12}$".to_owned()),
                        Some("notanaccount"),
                    ),
                ],
                "2 validation errors detected: Value at 'accountId' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: \\d{12}; Value at \
                 'kmsKeyId' failed to satisfy constraint: Member must not be null",
            ),
            (
                "the same two members in the other order, which is the same message",
                &[
                    violation(
                        "accountId",
                        ViolationKind::Pattern(r"^\d{12}$".to_owned()),
                        Some("notanaccount"),
                    ),
                    violation("kmsKeyId", ViolationKind::MissingRequiredMember, None),
                ],
                "2 validation errors detected: Value at 'accountId' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: \\d{12}; Value at \
                 'kmsKeyId' failed to satisfy constraint: Member must not be null",
            ),
            (
                "two requirements of ONE member, which keep the order the walk found them in",
                &[
                    violation("arn", ViolationKind::MaxLength(128), Some("xxx")),
                    violation(
                        "arn",
                        ViolationKind::Pattern(r"^arn:[a-z]+$".to_owned()),
                        Some("xxx"),
                    ),
                ],
                "2 validation errors detected: Value at 'arn' failed to satisfy constraint: \
                 Member must have length less than or equal to 128; Value at 'arn' failed to \
                 satisfy constraint: Member must satisfy regular expression pattern: arn:[a-z]+",
            ),
        ];

        for (case, violations, expected) in rows {
            let stated = refused(&Refusal::Invalid(violations.to_vec()))
                .unwrap_or_else(|| panic!("case {case:?}: expected a stated refusal, got none"));
            assert_eq!(
                stated.code, "ValidationException",
                "case {case:?}: unexpected code, got {:?}",
                stated.code
            );
            assert_eq!(
                stated.message, *expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }
    }

    /// RULE: a refusal that is not a constraint refusal keeps the framework's own code and
    /// wording — an unauthenticated caller is answered 403, not 400.
    #[test]
    fn only_a_constraint_refusal_is_reworded() {
        for (case, refusal) in [
            ("an unauthenticated caller", Refusal::Unauthenticated),
            ("no violation at all", Refusal::Invalid(Vec::new())),
        ] {
            let stated = refused(&refusal);
            assert!(
                stated.is_none(),
                "case {case:?}: expected the framework's own wording, got {stated:?}"
            );
        }
    }
}
