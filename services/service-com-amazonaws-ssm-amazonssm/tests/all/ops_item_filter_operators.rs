// SPDX-License-Identifier: CC0-1.0

//! Which operator each OpsItem filter key accepts.
//!
//! RULE: `DescribeOpsItems` publishes the operators each filter key takes as one
//! "Key: … Operations: …" list (`smithy_query(query="doc",
//! shape="DescribeOpsItemsRequest", member="OpsItemFilters")`) — so although
//! `OpsItemFilterOperator` declares four operators, a key admits only the ones its own row
//! lists, and a filter pairing a key with an operator that key does not accept is refused
//! `OpsCenterInvalidArgumentException` with the fixed word "InvalidFilter" rather than
//! queried. The identical filter under an operator the key does accept is served.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body — and the OpsItem every
//! accepted row may select was opened by a `CreateOpsItem` of this suite.
//!
//! The rows are the published list itself, walked over EVERY operator the model declares:
//! each (key, operator) pair the list admits must be served and each pair it does not must
//! be refused — rather than the single filter a recorded unit happened to send.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// The modelled `OpsItemFilterOperator` values (`smithy_query(query="shape",
/// shape="OpsItemFilterOperator")`).
const OPERATORS: &[&str] = &["Equal", "Contains", "GreaterThan", "LessThan"];

/// The error class OpsCenter refuses such a filter with, and the whole message it carries:
/// one fixed word, naming neither the key nor the operator.
const INVALID_FILTER_CODE: &str = "OpsCenterInvalidArgumentException";

/// See [`INVALID_FILTER_CODE`].
const INVALID_FILTER_MESSAGE: &str = "InvalidFilter";

/// The published list: each key whose operators `DescribeOpsItems` documents, the operators
/// it accepts, and a value of the right shape for it.
///
/// Read off that member's own documentation, with the documented "Equals" spelled as the
/// enumerated type spells it (`Equal`). The documented `LastModifiedBy` is left out: it
/// names no value of the modelled `OpsItemFilterKey`, so no request can carry it.
const PUBLISHED: &[(&str, &[&str], &str)] = &[
    ("CreatedTime", &["GreaterThan", "LessThan"], "2001-02-03T04:05:06Z"),
    (
        "LastModifiedTime",
        &["GreaterThan", "LessThan"],
        "2001-02-03T04:05:06Z",
    ),
    ("Priority", &["Equal"], "2"),
    ("Source", &["Contains", "Equal"], "emulator-tests"),
    ("Status", &["Equal"], "Open"),
    ("Title", &["Equal", "Contains"], "a queried work item"),
    (
        "OperationalData",
        &["Equal"],
        r#"{"key":"team","value":"platform"}"#,
    ),
    ("OperationalDataKey", &["Equal"], "team"),
    ("OperationalDataValue", &["Equal", "Contains"], "platform"),
    ("OpsItemId", &["Equal"], "oi-0123456789ab"),
    ("ResourceId", &["Contains"], "i-0abc"),
    ("AutomationId", &["Equal"], "automation-1"),
    ("AccountId", &["Equal"], crate::parameter_store::ACCOUNT),
];

/// The keys the published list does not name, which are held to nothing: the list says what
/// each key it names takes and says nothing about these, so a filter on one of them is
/// queried under any operator.
const UNPUBLISHED: &[(&str, &str)] = &[
    ("Category", "Performance"),
    ("Severity", "2"),
    ("OpsItemType", "/aws/issue"),
    ("CreatedBy", "arn:aws:iam::000000000000:user/nobody"),
    ("ActualStartTime", "2001-02-03T04:05:06Z"),
    ("ChangeRequestByTemplate", "a-template"),
];

/// One filter of a query.
fn filter(key: &str, operator: &str, value: &str) -> Value {
    json!({ "Key": key, "Operator": operator, "Values": [value] })
}

/// Open one OpsItem, so a query that is served has state to select from.
async fn open(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateOpsItem",
        &json!({
            "Title": "a queried work item",
            "Description": "opened by the filter-operator suite",
            "Source": "emulator-tests",
            "Priority": 2,
            "OperationalData": {
                "team": { "Value": "platform", "Type": "SearchableString" },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the OpsItem the accepted rows select must be opened first, got {status} with {created}"
    );
}

/// RULE: every published (key, operator) pair is served, and every pair the list does not
/// admit is refused.
#[tokio::test]
async fn a_key_accepts_only_the_operators_it_publishes() {
    let router = router();
    open(&router).await;

    for (key, accepted, value) in PUBLISHED {
        for operator in OPERATORS {
            let case = format!("{key} {operator}");
            let (status, response) = call(
                &router,
                "DescribeOpsItems",
                &json!({ "OpsItemFilters": [filter(key, operator, value)] }),
            )
            .await;
            if accepted.contains(operator) {
                assert_eq!(
                    status, 200,
                    "case {case:?}: an operator the key accepts is queried, got {status} with \
                     {response}"
                );
                assert!(
                    response["OpsItemSummaries"].is_array(),
                    "case {case:?}: a served query reports its summaries, got {response}"
                );
                continue;
            }
            assert_eq!(
                (status, error_code(&response).as_str()),
                (400, INVALID_FILTER_CODE),
                "case {case:?}: an operator the key does not accept is refused, got {status} \
                 with {response}"
            );
            assert_eq!(
                error_message(&response),
                INVALID_FILTER_MESSAGE,
                "case {case:?}: expected OpsCenter's own fixed message, got {response}"
            );
        }
    }
}

/// RULE, the other half: a key the published list does not name is held to nothing, so it
/// is queried under every operator the model declares.
#[tokio::test]
async fn a_key_the_list_does_not_name_is_queried_under_every_operator() {
    let router = router();
    open(&router).await;

    for (key, value) in UNPUBLISHED {
        for operator in OPERATORS {
            let case = format!("{key} {operator}");
            let (status, response) = call(
                &router,
                "DescribeOpsItems",
                &json!({ "OpsItemFilters": [filter(key, operator, value)] }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: a key the list does not name is queried, got {status} with \
                 {response}"
            );
        }
    }
}

/// RULE: the refusal is about the PAIR, so one bad filter refuses a query whose other
/// filters are sound, and the sound ones alone are served.
#[tokio::test]
async fn one_unaccepted_pair_refuses_the_whole_query() {
    let router = router();
    open(&router).await;

    let sound = filter("Source", "Equal", "emulator-tests");
    let unaccepted = filter("Priority", "GreaterThan", "1");

    let (status, response) = call(
        &router,
        "DescribeOpsItems",
        &json!({ "OpsItemFilters": [sound.clone(), unaccepted.clone()] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, INVALID_FILTER_CODE),
        "a query carrying one unaccepted pair is refused, got {status} with {response}"
    );
    assert_eq!(
        error_message(&response),
        INVALID_FILTER_MESSAGE,
        "expected OpsCenter's own fixed message, got {response}"
    );

    let (status, response) = call(
        &router,
        "DescribeOpsItems",
        &json!({ "OpsItemFilters": [sound] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the sound filters alone are served, got {status} with {response}"
    );
    assert_eq!(
        response["OpsItemSummaries"].as_array().map(Vec::len),
        Some(1),
        "the OpsItem this suite opened is the one selected, got {response}"
    );

    // And the identical filter under the operator the key DOES accept is served.
    let (status, response) = call(
        &router,
        "DescribeOpsItems",
        &json!({ "OpsItemFilters": [filter("Priority", "Equal", "2")] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the same key under the operator it accepts is served, got {status} with {response}"
    );
    assert_eq!(
        response["OpsItemSummaries"].as_array().map(Vec::len),
        Some(1),
        "the OpsItem this suite opened carries that priority, got {response}"
    );
}
