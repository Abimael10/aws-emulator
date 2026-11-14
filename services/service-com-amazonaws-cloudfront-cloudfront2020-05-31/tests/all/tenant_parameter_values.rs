// SPDX-License-Identifier: CC0-1.0

//! What a distribution tenant's PARAMETER VALUES are admitted by, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rule under test, read off the model
//! (`smithy_query(query="shape",shape="TenantConfig,ParameterDefinition,ParameterDefinitionSchema,StringSchemaConfig,Parameter")`):
//! "A valid parameter value must exist for any parameter that is marked as required in the
//! multi-tenant distribution" — so a tenant request that leaves a parameter the distribution
//! marks `Required` without a value (none submitted, and no `DefaultValue` on the definition
//! either) is refused `InvalidArgument` (HTTP 400) with the
//! service's FIXED sentence for a missing parameter, which names neither the parameter nor
//! the distribution; a parameter list that describes no list of values at all — one name
//! twice, or a name the distribution defines nothing for — keeps the refusal that states the
//! constraint the request broke after the error shape's own sentence.
//!
//! The rows span what the rule generalises over: which required definition is left
//! unsatisfied (the one with no default, and the one whose `DefaultValue` satisfies it), how
//! the value is withheld (no `Parameters` element at all, a list naming only other
//! parameters), how many values the list carries (none, one, two, three), and the OPERATION
//! that supplies them — `CreateDistributionTenant` and `UpdateDistributionTenant` both
//! measure a tenant's values against the same definitions. A value submitted EMPTY is no row
//! here: `Parameter$Value` carries `length min 1`, so the request boundary refuses it before
//! any definition is read.
//!
//! The multi-tenant distribution, the wire harness and the update call are the sibling
//! suites', reused rather than copied: that distribution's `TenantConfig` declares
//! `tenantName` required with no default, `stage` required with the `DefaultValue` `prod`,
//! and `region-label` optional.

use crate::distribution_lifecycle::{emulator, Answer, API_VERSION};
use crate::distribution_tenant_lifecycle::{
    create_distribution_tenant, escape, managed_certificate_request, multi_tenant_distribution,
};
use crate::distribution_tenant_update::{update_tenant, update_xml};

/// The sentence CloudFront refuses a tenant request that leaves a required parameter without
/// a value with: fixed text, which names no parameter and no distribution.
const MISSING_PARAMETER_MESSAGE: &str =
    "A parameter referenced in the multi-tenant distribution configuration is missing. \
     Specify the parameter and try again.";

/// The code the refusal travels under, and the status the model gives it
/// (`smithy_query(query="operation",operation="CreateDistributionTenant,UpdateDistributionTenant")`
/// -> `InvalidArgument`, httpError 400).
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The required definition with no `DefaultValue`: nothing but the request can satisfy it.
const REQUIRED_WITHOUT_DEFAULT: &str = "tenantName";

/// The required definition whose `DefaultValue` satisfies it when the request does not.
const REQUIRED_WITH_DEFAULT: &str = "stage";

/// A definition the distribution declares but does not require.
const OPTIONAL: &str = "region-label";

/// The `Parameters` element naming `values`, or no element at all for a request that states
/// none. `ParameterList`'s member carries no `xmlName`, so each entry travels in the restXml
/// default `<member>` element.
fn parameters_element(values: &[(&str, &str)]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let mut out = "<Parameters>".to_string();
    for (name, value) in values {
        out.push_str(&format!(
            "<member><Name>{}</Name><Value>{}</Value></member>",
            escape(name),
            escape(value)
        ));
    }
    out.push_str("</Parameters>");
    out
}

/// The `CreateDistributionTenantRequest` one row submits: a tenant of its own name and
/// domain under `distribution_id`, carrying `values` as its parameter list.
fn create_xml(distribution_id: &str, name: &str, values: &[(&str, &str)]) -> String {
    format!(
        "<CreateDistributionTenantRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <DistributionId>{}</DistributionId><Name>{}</Name>\
         <Domains><member><Domain>{}.example.com</Domain></member></Domains>{}{}\
         </CreateDistributionTenantRequest>",
        escape(distribution_id),
        escape(name),
        escape(name),
        parameters_element(values),
        managed_certificate_request(),
    )
}

/// One row: the parameter list a tenant request supplies, and what the service answers it
/// with.
struct Row {
    case: &'static str,
    /// The `Parameters` the request carries, as `(Name, Value)` pairs; empty for a request
    /// that states no `Parameters` element at all.
    values: &'static [(&'static str, &'static str)],
    /// What the service must answer: the fixed missing-parameter sentence, the sentence for
    /// a list that describes no values, or the served tenant.
    expected: Expected,
}

/// What a row's answer must be.
enum Expected {
    /// Refused with the fixed missing-parameter sentence.
    MissingParameter,
    /// Refused with the error shape's sentence followed by the constraint the list broke.
    Constraint(&'static str),
    /// Admitted: the values satisfy every required definition.
    Served,
}

/// Every parameter list the rule is asserted over.
const ROWS: &[Row] = &[
    Row {
        case: "no Parameters element at all",
        values: &[],
        expected: Expected::MissingParameter,
    },
    Row {
        case: "only the required parameter that carries a default",
        values: &[(REQUIRED_WITH_DEFAULT, "beta")],
        expected: Expected::MissingParameter,
    },
    Row {
        case: "only the optional parameter",
        values: &[(OPTIONAL, "eu")],
        expected: Expected::MissingParameter,
    },
    Row {
        case: "the defaulted and the optional parameter together",
        values: &[(REQUIRED_WITH_DEFAULT, "beta"), (OPTIONAL, "eu")],
        expected: Expected::MissingParameter,
    },
    Row {
        case: "the required parameter with no default supplied, the other left to its default",
        values: &[(REQUIRED_WITHOUT_DEFAULT, "acme")],
        expected: Expected::Served,
    },
    Row {
        case: "every declared parameter supplied",
        values: &[
            (REQUIRED_WITHOUT_DEFAULT, "acme"),
            (REQUIRED_WITH_DEFAULT, "beta"),
            (OPTIONAL, "eu"),
        ],
        expected: Expected::Served,
    },
    Row {
        case: "the required parameter named twice",
        values: &[
            (REQUIRED_WITHOUT_DEFAULT, "acme"),
            (REQUIRED_WITHOUT_DEFAULT, "acme-again"),
        ],
        expected: Expected::Constraint(
            "An argument is invalid: the parameter tenantName is named more than once",
        ),
    },
    Row {
        case: "a name the distribution defines nothing for",
        values: &[(REQUIRED_WITHOUT_DEFAULT, "acme"), ("tier", "gold")],
        expected: Expected::Constraint(
            "An argument is invalid: the distribution defines no parameter named tier",
        ),
    },
];

/// The `Message` of one answer.
fn message(answer: &Answer) -> String {
    answer
        .root()
        .find_text("Message")
        .unwrap_or_default()
        .to_string()
}

/// Assert one answer against what the row expects, naming the operation under test.
fn assert_row(row: &Row, answer: &Answer, context: &str) {
    let case = row.case;
    match row.expected {
        Expected::Served => {
            assert!(
                (200..300).contains(&answer.status),
                "case {case:?} ({context}): expected the values to be admitted, got {} with {}",
                answer.status,
                answer.raw
            );
        }
        Expected::MissingParameter | Expected::Constraint(_) => {
            assert_eq!(
                answer.status, 400,
                "case {case:?} ({context}): expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                INVALID_ARGUMENT,
                "case {case:?} ({context}): expected {INVALID_ARGUMENT}, got {:?} in {}",
                answer.error_code(),
                answer.raw
            );
            let actual = message(answer);
            let expected = match row.expected {
                Expected::Constraint(sentence) => sentence.to_string(),
                _ => MISSING_PARAMETER_MESSAGE.to_string(),
            };
            assert_eq!(
                actual, expected,
                "case {case:?} ({context}): expected the message {expected:?}, got {actual:?}"
            );
            if matches!(row.expected, Expected::MissingParameter) {
                // The fixed sentence states the CONDITION: no definition's name, and
                // nothing else of the request, belongs in it.
                for name in [REQUIRED_WITHOUT_DEFAULT, REQUIRED_WITH_DEFAULT, OPTIONAL] {
                    assert!(
                        !actual.contains(name),
                        "case {case:?} ({context}): the sentence must not name {name:?}, got \
                         {actual:?}"
                    );
                }
            }
        }
    }
}

/// A tenant name of this row, unique per row and admissible under
/// `DistributionTenant$Name`'s pattern.
fn tenant_name(prefix: &str, index: usize) -> String {
    format!("{prefix}-param-row-{index}")
}

/// `CreateDistributionTenant` measures the values the request supplies against the
/// definitions the multi-tenant distribution declares.
#[tokio::test]
async fn a_create_admits_only_a_parameter_list_that_satisfies_every_required_definition() {
    let router = emulator();
    let distribution = multi_tenant_distribution(&router, "ref-tenant-parameter-values").await;

    for (index, row) in ROWS.iter().enumerate() {
        let name = tenant_name("create", index);
        let answer =
            create_distribution_tenant(&router, &create_xml(&distribution, &name, row.values))
                .await;
        assert_row(row, &answer, "CreateDistributionTenant");
    }
}

/// `UpdateDistributionTenant` applies the SAME rule to the list the update supplies: the
/// values the tenant ends up with have to satisfy the same definitions, so the refusal — and
/// its sentence — is the create's.
#[tokio::test]
async fn an_update_admits_only_a_parameter_list_that_satisfies_every_required_definition() {
    let router = emulator();
    let distribution =
        multi_tenant_distribution(&router, "ref-tenant-parameter-values-update").await;

    for (index, row) in ROWS.iter().enumerate() {
        // Each row updates a tenant of its own, created with the values every definition
        // requires, so what the update is measured by is the list IT states.
        let name = tenant_name("update", index);
        let created = create_distribution_tenant(
            &router,
            &create_xml(
                &distribution,
                &name,
                &[(REQUIRED_WITHOUT_DEFAULT, "acme"), (OPTIONAL, "eu")],
            ),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected the tenant to be created, got {} with {}",
            row.case, created.status, created.raw
        );
        let id = created
            .root()
            .text_at("Id")
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the create carries no Id in {}",
                    row.case, created.raw
                )
            })
            .to_string();
        let etag = created.header("etag").to_string();

        // An update that states no `Parameters` element leaves the stored values exactly as
        // they were, so the row with no element states an EMPTY list instead: the list the
        // tenant ends up with is then the one under test.
        let members = match row.values {
            [] => "<Parameters></Parameters>".to_string(),
            values => parameters_element(values),
        };
        let answer = update_tenant(&router, &id, &update_xml(&members), Some(&etag)).await;
        assert_row(row, &answer, "UpdateDistributionTenant");
    }
}
