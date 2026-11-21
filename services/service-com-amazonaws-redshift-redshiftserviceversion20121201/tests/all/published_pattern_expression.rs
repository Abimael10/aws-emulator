// SPDX-License-Identifier: CC0-1.0

//! The expression a `@pattern` refusal of this service QUOTES, end to end over the awsQuery
//! wire.
//!
//! The rule under test: **a constraint refusal quotes the expression this API publishes for
//! the member's SHAPE.** A caller lifts that expression out of the message and compiles it,
//! so what the message carries is the expression the service advertises rather than the
//! model's spelling of it, and one API publishes both forms: for most shapes it is the body
//! inside the `^`/`$` pair the model writes around a whole-value match, and for a shape the
//! API publishes as declared it is the model's text verbatim, anchors included. Which of the
//! two a shape takes cannot be read off the text — both are spelled `^…$` in the model — so
//! it is reference data about the shape, which `crate::wire`-free rows below state per shape.
//! The CHECK is unchanged either way: the value is still matched against the model's pattern
//! as written, so every row here is refused.
//!
//! The envelope around it is the service's own and is asserted too: `400` under the one
//! validation code this service names a refused input with, over AWS's constraint envelope
//! (`1 validation error detected: Value '<value>' at '<member>' failed to satisfy
//! constraint: Member must satisfy regular expression pattern: <expression>`).
//!
//! The rows span what the rule generalises over: the operation (a create of an integration
//! and a create of a cluster), the member (a source ARN, a target ARN, an integration name, a
//! catalog name), the shape whose expression is quoted, which of the two published forms that
//! shape takes, and what makes the submitted value violate it (a service the expression
//! admits nowhere, a character outside the class, a leading digit, a value of another resource
//! type). No row is the recorded case; the recorded one is a single source ARN.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Emulator};

/// The error code this service names a refused request INPUT with, whichever member and
/// whichever operation it was — spelled out here rather than read from the crate, so a
/// changed code fails this suite.
const VALIDATION_ERROR: &str = "ValidationError";

/// One row: a request that breaks a member's `@pattern`, and the expression the refusal
/// must quote.
struct PatternRow {
    case: &'static str,
    action: &'static str,
    /// The request's parameters, the offending one included.
    params: &'static [(&'static str, &'static str)],
    /// The member path the clause names, as the refusal renders it.
    member: &'static str,
    /// The value the clause quotes, which is the one the request carried.
    value: &'static str,
    /// The expression the clause must quote: what this API PUBLISHES for the member's
    /// shape.
    published: &'static str,
    /// The model's own text for that shape, so a row says which of the two published forms
    /// the shape takes and a model change that moved the text fails this suite.
    declared: &'static str,
}

/// A `SourceArn` the modelled expression admits, for the rows whose offending member is
/// another one (`smithy_query(query="operation",operation="CreateIntegration")`).
const GOOD_SOURCE_ARN: &str = "arn:aws:s3:::a-source-bucket";

/// A `TargetArn` the modelled expression admits, for the same reason.
const GOOD_TARGET_ARN: &str = "arn:aws:redshift:us-west-2:123456789012:namespace/abc-12345678";

/// The modelled `@pattern` of each shape a row drives, as the model spells it
/// (`smithy_query(query="operation",operation="CreateIntegration,CreateCluster")` and
/// `smithy_query(query="shape",shape="SourceArn,TargetArn,IntegrationName")`), written out
/// here rather than imported so a wrong constant in the implementation cannot make a row
/// pass.
const SOURCE_ARN_DECLARED: &str = r"^arn:aws[a-z\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\-\/]+$";
const TARGET_ARN_DECLARED: &str =
    r"^arn:aws[a-z\-]*:redshift(-serverless)?:[a-z0-9\-]+:[0-9]{12}:(namespace\/|namespace:)[a-z0-9\-]+$";
const INTEGRATION_NAME_DECLARED: &str = r"^[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*$";
const CATALOG_NAME_DECLARED: &str = r"^[a-z0-9_-]*[a-z]+[a-z0-9_-]*$";

/// The rows the table drives.
const ROWS: &[PatternRow] = &[
    PatternRow {
        case: "a source ARN naming a service the expression admits nowhere",
        action: "CreateIntegration",
        params: &[
            ("SourceArn", "arn:aws:rds:us-west-2:000000000000:cluster:a-source"),
            ("TargetArn", GOOD_TARGET_ARN),
            ("IntegrationName", "an-integration"),
        ],
        member: "sourceArn",
        value: "arn:aws:rds:us-west-2:000000000000:cluster:a-source",
        // This shape is published AS DECLARED: the pair stays.
        published: SOURCE_ARN_DECLARED,
        declared: SOURCE_ARN_DECLARED,
    },
    PatternRow {
        case: "a source ARN of a supported service whose resource carries a space",
        action: "CreateIntegration",
        params: &[
            (
                "SourceArn",
                "arn:aws:dynamodb:us-west-2:000000000000:table/a table",
            ),
            ("TargetArn", GOOD_TARGET_ARN),
            ("IntegrationName", "an-integration"),
        ],
        member: "sourceArn",
        value: "arn:aws:dynamodb:us-west-2:000000000000:table/a table",
        published: SOURCE_ARN_DECLARED,
        declared: SOURCE_ARN_DECLARED,
    },
    PatternRow {
        case: "a target ARN naming a cluster rather than a namespace",
        action: "CreateIntegration",
        params: &[
            ("SourceArn", GOOD_SOURCE_ARN),
            ("TargetArn", "arn:aws:redshift:us-west-2:123456789012:cluster:a-cluster"),
            ("IntegrationName", "an-integration"),
        ],
        member: "targetArn",
        value: "arn:aws:redshift:us-west-2:123456789012:cluster:a-cluster",
        // Every other shape of this model is published as the BODY of the pair.
        published: r"arn:aws[a-z\-]*:redshift(-serverless)?:[a-z0-9\-]+:[0-9]{12}:(namespace\/|namespace:)[a-z0-9\-]+",
        declared: TARGET_ARN_DECLARED,
    },
    PatternRow {
        case: "an integration name opening with a digit",
        action: "CreateIntegration",
        params: &[
            ("SourceArn", GOOD_SOURCE_ARN),
            ("TargetArn", GOOD_TARGET_ARN),
            ("IntegrationName", "1-leading-digit"),
        ],
        member: "integrationName",
        value: "1-leading-digit",
        published: r"[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*",
        declared: INTEGRATION_NAME_DECLARED,
    },
    PatternRow {
        case: "an integration name carrying a character the class excludes",
        action: "CreateIntegration",
        params: &[
            ("SourceArn", GOOD_SOURCE_ARN),
            ("TargetArn", GOOD_TARGET_ARN),
            ("IntegrationName", "an_integration"),
        ],
        member: "integrationName",
        value: "an_integration",
        published: r"[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*",
        declared: INTEGRATION_NAME_DECLARED,
    },
    PatternRow {
        // Another OPERATION and another shape, so the default rendering is not keyed to the
        // one create the recorded case exercised: `CatalogName` is the only
        // pattern-constrained member of `CreateClusterMessage`
        // (`smithy_query(query="shape",shape="CreateClusterMessage")`), and a value holding no
        // lowercase letter satisfies neither end of it.
        case: "a catalog name with no lowercase letter in it, on another operation",
        action: "CreateCluster",
        params: &[
            ("ClusterIdentifier", "a-published-pattern-cluster"),
            ("NodeType", "ra3.xlplus"),
            ("MasterUsername", "admin"),
            ("MasterUserPassword", "Sekrit-0ne"),
            ("CatalogName", "01_9"),
        ],
        member: "catalogName",
        value: "01_9",
        published: r"[a-z0-9_-]*[a-z]+[a-z0-9_-]*",
        declared: CATALOG_NAME_DECLARED,
    },
];

/// Every `@pattern` refusal quotes the expression this API publishes for the member's
/// shape, inside the one validation envelope this service answers an input refusal with.
#[tokio::test]
async fn a_pattern_refusal_quotes_the_expression_the_api_publishes() {
    let emulator = Emulator::new();
    for row in ROWS {
        // Each row's `declared` says which of the two published forms the shape takes, so a
        // model text that stopped wrapping the whole value fails here rather than silently
        // changing what a clause quotes.
        assert!(
            row.declared.starts_with('^') && row.declared.ends_with('$'),
            "case {:?}: the modelled expression {:?} must wrap the whole value",
            row.case,
            row.declared
        );
        assert!(
            row.published == row.declared
                || row.declared == format!("^{}$", row.published),
            "case {:?}: the published expression {:?} must be the model's text or its body",
            row.case,
            row.published
        );

        let answer = emulator.call(row.action, row.params).await;
        assert_eq!(
            answer.status, 400,
            "case {:?}: expected HTTP 400, got {} with {}",
            row.case, answer.status, answer.body
        );
        assert_eq!(
            answer.error_code().as_deref(),
            Some(VALIDATION_ERROR),
            "case {:?}: expected {VALIDATION_ERROR}, got {:?} with {}",
            row.case,
            answer.error_code(),
            answer.body
        );
        let expected = format!(
            "1 validation error detected: Value '{}' at '{}' failed to satisfy constraint: \
             Member must satisfy regular expression pattern: {}",
            row.value, row.member, row.published
        );
        let message = answer.text("Message").unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );

        // The refused call reached no handler: nothing of this service's state was written
        // for it, which the two stores these rows address show by holding nothing.
        let store = state::service_state(&*emulator.engine)
            .expect("the in-memory engine always has this service's store");
        let held = store.lock();
        assert!(
            held.integrations.is_empty() && held.clusters.is_empty(),
            "case {:?}: a refused create must store nothing, got {} integration(s) and {} \
             cluster(s)",
            row.case,
            held.integrations.len(),
            held.clusters.len()
        );
    }
}

/// The envelope the clause sits in is the awsQuery error document, so the refusal carries
/// the error element and the request id every failed call of this service carries — read
/// here on one row, because the envelope is the protocol's and not this rule's.
#[tokio::test]
async fn a_pattern_refusal_is_answered_in_this_services_error_envelope() {
    let emulator = Emulator::new();
    let row = &ROWS[0];
    let answer = emulator.call(row.action, row.params).await;
    assert_eq!(
        answer.status, 400,
        "expected HTTP 400, got {} with {}",
        answer.status, answer.body
    );
    let request_ids = all_text(&answer.body, "RequestId");
    assert_eq!(
        request_ids.len(),
        1,
        "expected one RequestId in the error envelope, got {request_ids:?} in {}",
        answer.body
    );
    assert!(
        !request_ids[0].is_empty(),
        "expected a non-empty RequestId, got {request_ids:?}"
    );
    let types = all_text(&answer.body, "Type");
    assert_eq!(
        types,
        vec!["Sender".to_string()],
        "expected the sender fault of a client refusal, got {types:?} in {}",
        answer.body
    );
}
