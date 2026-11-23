// SPDX-License-Identifier: CC0-1.0

//! The `@pattern` clause of a refusal that states MORE THAN ONE requirement, end to end
//! over the awsQuery wire.
//!
//! The rule under test: **every clause of a constraint refusal quotes the expression this
//! API publishes for the shape ITS OWN member targets**, so the quoting is a property of
//! the clause and not of the message. A request outside several members' expressions is
//! stated as one clause per violated member — each quoting its own shape's published
//! expression, under the plural count word — and a member outside its `@length` as well as
//! its expression is stated as two clauses, the expression unchanged in the one that
//! carries it.
//!
//! `crate::published_pattern_expression` pins WHICH of the two forms this API publishes
//! each shape with, one violated member per request. This suite is that same rule where
//! the message carries more than one clause: the published expression of a shape has to
//! survive the company of another member's clause and of another constraint class on its
//! own member, since what a caller lifts out of the message and compiles is one clause's
//! expression however many clauses stand beside it.
//!
//! WHICH of the two forms this API publishes a shape with is reference data about the shape
//! — both are spelled `^…$` in the model, so it cannot be read off the text — and this
//! suite reads the rows this crate already states for it (`src/service.rs` lists the
//! source-ARN expression as the one published as declared, which is the shape whose refusal
//! this surface is recorded answering) rather than deciding it again. The CHECK is the
//! model's pattern as written either way, so every row here is refused.
//!
//! The rows span what the rule generalises over: how many clauses the message carries (two
//! and three), which shapes are quoted in it, which of the two published forms each of
//! those takes, and the other constraint class a clause can stand beside (a `@length`
//! ceiling and a `@length` floor).

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::Emulator;

/// The error code this service names a refused request INPUT with, whichever member and
/// however many clauses the message carries.
const VALIDATION_ERROR: &str = "ValidationError";

/// The expression this API publishes for the shape a zero-ETL integration's SOURCE is named
/// by: the model's text verbatim, whole-value anchors included
/// (`smithy_query(query="operation",operation="CreateIntegration")` reports `SourceArn`
/// with this `@pattern`, and `src/service.rs` lists the shape among the ones this API
/// publishes as declared).
const SOURCE_ARN_PUBLISHED: &str = r"^arn:aws[a-z\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\-\/]+$";

/// The expression this API publishes for the shape its TARGET is named by: the body inside
/// the whole-value pair the model wrote around it, which is this API's other published form
/// (`smithy_query(query="operation",operation="CreateIntegration")` reports `TargetArn` as
/// `^arn:aws[a-z\-]*:redshift(-serverless)?:[a-z0-9\-]+:[0-9]{12}:(namespace\/|namespace:)[a-z0-9\-]+$`).
const TARGET_ARN_PUBLISHED: &str =
    r"arn:aws[a-z\-]*:redshift(-serverless)?:[a-z0-9\-]+:[0-9]{12}:(namespace\/|namespace:)[a-z0-9\-]+";

/// The expression this API publishes for an integration's NAME, in the same body form
/// (`smithy_query(query="operation",operation="CreateIntegration")` reports
/// `IntegrationName` as `^[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*$`).
const INTEGRATION_NAME_PUBLISHED: &str = r"[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*";

/// The modelled `@length` of the two ARN members, which is the other constraint class a
/// clause of these rows stands beside
/// (`smithy_query(query="operation",operation="CreateIntegration")`: `SourceArn` 1..255 and
/// `TargetArn` 20..2048).
const SOURCE_ARN_MAX_LENGTH: usize = 255;
const TARGET_ARN_MIN_LENGTH: usize = 20;

/// A `SourceArn` the modelled expression admits, for the rows whose offending members are
/// other ones.
const GOOD_SOURCE_ARN: &str = "arn:aws:s3:::a-source-bucket";

/// A `TargetArn` the modelled expression admits, for the same reason.
const GOOD_TARGET_ARN: &str = "arn:aws:redshift:us-west-2:123456789012:namespace/abc-12345678";

/// An `IntegrationName` the modelled expression admits, for the same reason.
const GOOD_INTEGRATION_NAME: &str = "an-integration";

/// One row: a request outside more than one requirement, and the clauses the refusal must
/// state for it, each one as the message must spell it.
struct Row {
    case: &'static str,
    params: Vec<(String, String)>,
    clauses: Vec<String>,
}

/// One clause of the envelope: the value the request carried, the member path it sat at,
/// and the requirement it missed.
fn clause(value: &str, member: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{member}' failed to satisfy constraint: {requirement}")
}

/// The requirement a member outside its `@pattern` missed, quoting the published
/// expression.
fn pattern_requirement(published: &str) -> String {
    format!("Member must satisfy regular expression pattern: {published}")
}

/// The requirement a value longer than its member's `@length` ceiling missed.
fn at_most_requirement(max: usize) -> String {
    format!("Member must have length less than or equal to {max}")
}

/// The requirement a value shorter than its member's `@length` floor missed.
fn at_least_requirement(min: usize) -> String {
    format!("Member must have length greater than or equal to {min}")
}

/// The head of AWS's constraint envelope for a message carrying SEVERAL clauses: how many
/// of them were detected, with the count word in the number that count takes.
///
/// Every row of this suite breaks more than one requirement, which is what the suite is
/// about, so the singular the envelope takes for one clause is
/// [`crate::published_pattern_expression`]'s to state and is not spelled here.
fn plural_envelope_head(clauses: &[String]) -> String {
    let count = clauses.len();
    assert!(
        count > 1,
        "a row of this suite breaks more than one requirement, got {count}"
    );
    format!("{count} validation errors detected: ")
}

/// The clauses a message carries, read out of it: the envelope's head is stripped and what
/// follows is split on the separator the envelope joins clauses with.
///
/// The clauses are compared as a SET by the caller. Which order the envelope states them in
/// is the validation walk's own and nothing the model or AWS's documentation states, so a
/// row asserts WHICH clauses the message carries and each one byte for byte, rather than
/// pinning that order.
fn clauses_of(message: &str, head: &str) -> Vec<String> {
    match message.strip_prefix(head) {
        Some(stated) => stated.split("; ").map(str::to_owned).collect(),
        None => Vec::new(),
    }
}

/// The request parameters of a `CreateIntegration`, as the form fields the wire carries.
fn create_integration_params(
    source_arn: &str,
    target_arn: &str,
    integration_name: &str,
) -> Vec<(String, String)> {
    vec![
        ("SourceArn".to_owned(), source_arn.to_owned()),
        ("TargetArn".to_owned(), target_arn.to_owned()),
        ("IntegrationName".to_owned(), integration_name.to_owned()),
    ]
}

/// The rows the table drives.
fn rows() -> Vec<Row> {
    // A source ARN of a service the expression admits nowhere, which is also longer than
    // the member's ceiling: the resource segment is padded until the whole value passes it.
    let padded_source_arn = {
        let prefix = "arn:aws:rds:us-west-2:000000000000:cluster:";
        format!(
            "{prefix}{}",
            "a".repeat(SOURCE_ARN_MAX_LENGTH.saturating_sub(prefix.len()) + 1)
        )
    };
    assert!(
        padded_source_arn.len() > SOURCE_ARN_MAX_LENGTH,
        "the padded source ARN {padded_source_arn:?} must be past the modelled ceiling"
    );
    // A target ARN shorter than the member's floor, which the expression does not admit
    // either.
    let short_target_arn = "arn:aws:redshift:x".to_owned();
    assert!(
        short_target_arn.len() < TARGET_ARN_MIN_LENGTH,
        "the short target ARN {short_target_arn:?} must be under the modelled floor"
    );

    vec![
        Row {
            case: "a source ARN and an integration name, each outside its own expression",
            params: create_integration_params(
                "arn:aws:rds:us-west-2:000000000000:cluster:a-source",
                GOOD_TARGET_ARN,
                "an_integration",
            ),
            clauses: vec![
                clause(
                    "arn:aws:rds:us-west-2:000000000000:cluster:a-source",
                    "sourceArn",
                    &pattern_requirement(SOURCE_ARN_PUBLISHED),
                ),
                clause(
                    "an_integration",
                    "integrationName",
                    &pattern_requirement(INTEGRATION_NAME_PUBLISHED),
                ),
            ],
        },
        Row {
            case: "all three members outside their expressions at once",
            params: create_integration_params(
                "arn:aws:rds:us-west-2:000000000000:cluster:a-source",
                "arn:aws:redshift:us-west-2:123456789012:cluster:a-cluster",
                "1-leading-digit",
            ),
            clauses: vec![
                clause(
                    "arn:aws:rds:us-west-2:000000000000:cluster:a-source",
                    "sourceArn",
                    &pattern_requirement(SOURCE_ARN_PUBLISHED),
                ),
                clause(
                    "arn:aws:redshift:us-west-2:123456789012:cluster:a-cluster",
                    "targetArn",
                    &pattern_requirement(TARGET_ARN_PUBLISHED),
                ),
                clause(
                    "1-leading-digit",
                    "integrationName",
                    &pattern_requirement(INTEGRATION_NAME_PUBLISHED),
                ),
            ],
        },
        Row {
            case: "a source ARN past the member's length ceiling and outside its expression",
            params: create_integration_params(
                &padded_source_arn,
                GOOD_TARGET_ARN,
                GOOD_INTEGRATION_NAME,
            ),
            clauses: vec![
                clause(
                    &padded_source_arn,
                    "sourceArn",
                    &at_most_requirement(SOURCE_ARN_MAX_LENGTH),
                ),
                clause(
                    &padded_source_arn,
                    "sourceArn",
                    &pattern_requirement(SOURCE_ARN_PUBLISHED),
                ),
            ],
        },
        Row {
            case: "a target ARN under the member's length floor and outside its expression",
            params: create_integration_params(
                GOOD_SOURCE_ARN,
                &short_target_arn,
                GOOD_INTEGRATION_NAME,
            ),
            clauses: vec![
                clause(
                    &short_target_arn,
                    "targetArn",
                    &at_least_requirement(TARGET_ARN_MIN_LENGTH),
                ),
                clause(
                    &short_target_arn,
                    "targetArn",
                    &pattern_requirement(TARGET_ARN_PUBLISHED),
                ),
            ],
        },
    ]
}

/// Every clause of a multi-clause refusal quotes the expression this API publishes for the
/// shape its own member targets, inside the one validation envelope this service answers an
/// input refusal with.
#[tokio::test]
async fn a_clause_quotes_its_own_shapes_published_expression() {
    let emulator = Emulator::new();
    for row in rows() {
        let params: Vec<(&str, &str)> = row
            .params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let answer = emulator.call("CreateIntegration", &params).await;
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
        let head = plural_envelope_head(&row.clauses);
        let message = answer.text("Message").unwrap_or_default();
        assert!(
            message.starts_with(&head),
            "case {:?}: expected the message to open with {head:?}, got {message:?}",
            row.case
        );
        let mut stated = clauses_of(&message, &head);
        stated.sort();
        let mut expected = row.clauses.clone();
        expected.sort();
        assert_eq!(
            stated, expected,
            "case {:?}: expected the clauses {expected:?}, got {stated:?} in {message:?}",
            row.case
        );

        // The refused call reached no handler: this service's integration store holds
        // nothing for it, so every row's clauses were stated before anything was written.
        let store = state::service_state(&*emulator.engine)
            .expect("the in-memory engine always has this service's store");
        let held = store.lock();
        assert!(
            held.integrations.is_empty(),
            "case {:?}: a refused create must store nothing, got {} integration(s)",
            row.case,
            held.integrations.len()
        );
    }
}
