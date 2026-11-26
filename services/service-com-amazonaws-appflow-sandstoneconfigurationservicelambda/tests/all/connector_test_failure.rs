// SPDX-License-Identifier: CC0-1.0

//! What AppFlow ANSWERS when the connection test it runs while defining a connector profile
//! fails because the connector could not take the profile's Data API role on.
//!
//! The rule pinned here: a role AppFlow cannot assume is not a defect in the request. AppFlow
//! takes the request as well formed, runs the connector's test connection, and reports the
//! CONNECTOR's failure — the modelled `ConnectorServerException` ("An error occurred when
//! retrieving data from the connector endpoint", @httpError 400) carrying the
//! connector-communication sentence, whose only request value is the role ARN, taken verbatim.
//! `ValidationException`, "The request has invalid or missing parameters", is for a parameter
//! fault and is never the answer here.
//!
//! The rows differ along what the rule generalises over: the OPERATION that defines the profile
//! (the create and the update both run the test), the FORM of the role ARN the request sent (a
//! plain name, a name under a path, a long generated name, a name carrying the symbols
//! `RoleName` admits) and WHY the assume fails (a role this account's IAM holds nothing under,
//! and one whose trust policy admits another principal). The recorded case is one row.
//!
//! `tests/all/connector_profiles.rs` holds the rest of rule 2 — which configurations are
//! refused at all, and that a refused definition stores nothing; this suite holds the WORDING
//! and the error class the refusal is reported under.

use serde_json::json;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{role_arn, DEFAULT_PATH};

use crate::connector_profiles::{
    appflow_with_iam, connector_test_failure_id, described_one, redshift_config, seed_role, trust,
    APPFLOW_PRINCIPAL, CONNECTOR_TEST_FAILURE_CLOSE, CONNECTOR_TEST_FAILURE_MIDDLE,
    CONNECTOR_TEST_FAILURE_OPENING, CREATE, UPDATE,
};
use crate::flow_lifecycle::call;

/// The error code the connector's own failure is reported under, and the status that shape
/// carries (`smithy_query(query="shape", shape="ConnectorServerException")`).
const CONNECTOR_SERVER_EXCEPTION: &str = "ConnectorServerException";
const CONNECTOR_FAILURE_STATUS: u16 = 400;

/// The error code a PARAMETER fault would be reported under, which this failure never is
/// (`smithy_query(query="operation", operation="UpdateConnectorProfile")`:
/// `ValidationException`, 400, "The request has invalid or missing parameters").
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// One row: a Data API role the connector cannot take on, in the form the request sends it.
struct FailureRow {
    case: &'static str,
    /// The IAM role NAME the ARN carries — the forms a caller supplies differ in it.
    role_name: &'static str,
    /// The trust document to seed that role with, or `None` to leave this account's IAM
    /// holding nothing under the name at all.
    trust: Option<&'static [&'static str]>,
}

/// The rows, one per form of role ARN and reason the assume fails.
fn rows() -> Vec<FailureRow> {
    vec![
        FailureRow {
            case: "a role this account's IAM holds nothing under",
            role_name: "data-api",
            trust: None,
        },
        FailureRow {
            case: "a role whose trust policy admits another service only",
            role_name: "redshift-only-data-api",
            trust: Some(&["redshift.amazonaws.com"]),
        },
        FailureRow {
            case: "a long generated name, as a deployment mints one",
            role_name: "warehouse-stack-DataApiRole-7PQK3ZB1XW4H9TLN6MVC2SDGJ",
            trust: None,
        },
        FailureRow {
            // `RoleName`'s own `@pattern` admits these symbols beside word characters
            // (`smithy_query(query="shape", shape="roleNameType")` in the IAM model:
            // `^[\w+=,.@-]+$`), so a caller may send any of them.
            case: "a name carrying the symbols the role-name pattern admits",
            role_name: "data.api+role=1,2@team-east",
            trust: None,
        },
        FailureRow {
            case: "a role trusting nobody at all",
            role_name: "untrusted-data-api",
            trust: Some(&[]),
        },
    ]
}

/// The ARN the row's request sends for the Data API role, seeded into this account's IAM only
/// where the row says IAM holds it: a row with no trust document is one IAM holds NOTHING under,
/// and the request still sends a well-formed ARN for it.
fn data_api_role_of(row: &FailureRow, engine: &std::sync::Arc<engine::InMemoryEngine>) -> String {
    match row.trust {
        Some(principals) => seed_role(engine, row.role_name, Some(trust("Allow", principals))),
        None => role_arn(DEFAULT_PATH, row.role_name),
    }
}

/// The CREATE's answer for every row: the connector's failure, worded as AWS words it.
#[tokio::test]
async fn the_create_answers_the_connector_test_failure() {
    for row in rows() {
        let (router, engine) = appflow_with_iam();
        let s3_role = seed_role(&engine, "redshift-s3-access", None);
        let data_api_role = data_api_role_of(&row, &engine);
        let answer = call(
            &router,
            CREATE,
            &json!({
                "connectorProfileName": "warehouse",
                "connectorType": "Redshift",
                "connectionMode": "Public",
                "connectorProfileConfig": redshift_config(&s3_role, Some(&data_api_role), "dev"),
            }),
        )
        .await;
        assert_sentence(row.case, &answer, &data_api_role);
        assert_eq!(
            described_one(&router, "warehouse").await,
            None,
            "case {:?}: expected the failed connection test to store no profile",
            row.case
        );
    }
}

/// And the UPDATE's, on a profile the account already holds: the same test, so the same answer.
#[tokio::test]
async fn the_update_answers_the_connector_test_failure() {
    for row in rows() {
        let (router, engine) = appflow_with_iam();
        let s3_role = seed_role(&engine, "redshift-s3-access", None);
        let usable = seed_role(
            &engine,
            "appflow-data-api",
            Some(trust("Allow", &[APPFLOW_PRINCIPAL])),
        );
        let created = call(
            &router,
            CREATE,
            &json!({
                "connectorProfileName": "warehouse",
                "connectorType": "Redshift",
                "connectionMode": "Public",
                "connectorProfileConfig": redshift_config(&s3_role, Some(&usable), "dev"),
            }),
        )
        .await;
        assert!(
            created.is_ok(),
            "case {:?}: expected the setup create to succeed, got {} {:?}",
            row.case,
            created.status,
            created.body
        );

        // The row's role, seeded only where the row says this account's IAM holds it.
        let data_api_role = data_api_role_of(&row, &engine);
        let answer = call(
            &router,
            UPDATE,
            &json!({
                "connectorProfileName": "warehouse",
                "connectionMode": "Public",
                "connectorProfileConfig": redshift_config(&s3_role, Some(&data_api_role), "prod"),
            }),
        )
        .await;
        assert_sentence(row.case, &answer, &data_api_role);
        let held = described_one(&router, "warehouse")
            .await
            .unwrap_or_else(|| panic!("case {:?}: the profile survives the failure", row.case));
        assert_eq!(
            held.pointer("/connectorProfileProperties/Redshift/dataApiRoleArn")
                .and_then(serde_json::Value::as_str),
            Some(usable.as_str()),
            "case {:?}: expected the profile left on the role it was created with, got {held:?}",
            row.case
        );
    }
}

/// The one value the sentence carries that is not fixed text or a request value is the
/// connector client's own call id: two failures of the SAME request carry different ids and
/// the same fixed text, so the id identifies the call and nothing about the profile.
#[tokio::test]
async fn only_the_call_id_differs_between_two_identical_failures() {
    let (router, engine) = appflow_with_iam();
    let s3_role = seed_role(&engine, "redshift-s3-access", None);
    let unusable = seed_role(
        &engine,
        "redshift-only-data-api",
        Some(trust("Allow", &["redshift.amazonaws.com"])),
    );
    let request = json!({
        "connectorProfileName": "warehouse",
        "connectorType": "Redshift",
        "connectionMode": "Public",
        "connectorProfileConfig": redshift_config(&s3_role, Some(&unusable), "dev"),
    });
    let first = call(&router, CREATE, &request).await;
    let second = call(&router, CREATE, &request).await;
    let (first, second) = (
        first.string("message").to_string(),
        second.string("message").to_string(),
    );
    let first_id = connector_test_failure_id(&first, &unusable)
        .unwrap_or_else(|| panic!("case first: expected the sentence, got {first:?}"))
        .to_string();
    let second_id = connector_test_failure_id(&second, &unusable)
        .unwrap_or_else(|| panic!("case second: expected the sentence, got {second:?}"))
        .to_string();
    assert_ne!(
        first_id, second_id,
        "case two failures: expected a per-call id, got {first_id:?} both times"
    );
    // Everything either side of the id is the same text, which is what makes the rest of the
    // sentence a requirement rather than one run's output.
    assert_eq!(
        first.replace(&first_id, ""),
        second.replace(&second_id, ""),
        "case two failures: expected the same fixed text, got {first:?} and {second:?}"
    );
}

/// What every row asserts about one answer: the status and error code the connector's failure is
/// reported under, and the sentence AWS words for it — its fixed text, and the role ARN taken
/// verbatim from the request.
fn assert_sentence(case: &str, answer: &crate::flow_lifecycle::Answer, role_arn: &str) {
    assert_eq!(
        (answer.status, answer.error.as_str()),
        (CONNECTOR_FAILURE_STATUS, CONNECTOR_SERVER_EXCEPTION),
        "case {case:?}: expected the connector-endpoint error, got {:?}",
        answer.body
    );
    assert_ne!(
        answer.error.as_str(),
        VALIDATION_EXCEPTION,
        "case {case:?}: a role the connector could not assume is not a parameter fault"
    );
    let message = answer.string("message");
    let id = connector_test_failure_id(message, role_arn).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected {CONNECTOR_TEST_FAILURE_OPENING}{role_arn}\
             {CONNECTOR_TEST_FAILURE_MIDDLE}<id>{CONNECTOR_TEST_FAILURE_CLOSE}, got {message:?}"
        )
    });
    assert!(
        !id.is_empty(),
        "case {case:?}: expected the connector client's own call id in the sentence, got \
         {message:?}"
    );
}
