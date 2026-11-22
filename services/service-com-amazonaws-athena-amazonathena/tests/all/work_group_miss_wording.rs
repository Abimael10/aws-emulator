// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the refusal every operation that resolves a `WorkGroup` answers a name
//! this account holds no workgroup under with.
//!
//! The rule under test: Athena models no not-found error for a workgroup, so naming one that
//! does not exist is refused as `InvalidRequestException` (HTTP 400) under the code for a
//! refusal the request itself caused, carrying ONE fixed sentence — `WorkGroup is not
//! found.` — which interpolates nothing from the request. A client that cannot branch on an
//! error code of its own compares that sentence, so the name it asked about may not appear
//! in it.
//!
//! The rule is asserted over a TABLE whose rows differ along what the rule generalises over:
//! the operation refused, and the NAME the request carried — its length, from the shortest
//! the modelled `WorkGroupName` admits to the longest, the character classes that name's
//! pattern admits, and names that stand in a relation to one the account really holds (the
//! same name in another case, and a prefix of it). The sentence being byte-identical across
//! every one of those rows is what pins the absence of any interpolation.
//!
//! Every call goes over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{self, ERROR_CODE_INVALID_INPUT};

use crate::harness::{assert_refusal, call, create_work_group, router};

/// The sentence a workgroup miss is refused with, written out HERE rather than read off the
/// service's own constant: what this suite pins is the TEXT a client compares, so a change
/// of that constant has to fail this suite rather than travel through it.
const WORK_GROUP_IS_NOT_FOUND: &str = "WorkGroup is not found.";

/// A workgroup this account really holds, so the rows naming a variant of it refuse a name
/// that is genuinely absent rather than one that was never near a held workgroup.
const HELD_WORK_GROUP: &str = "wording-suite-held-wg";

/// Where a query of this suite would write its results. No query here runs — each is refused
/// for the workgroup it names — but a submission that named no location at all would be
/// refused for THAT instead, which is a different sentence.
const OUTPUT_LOCATION: &str = "s3://athena-results/wording-suite/";

/// One operation driven against a workgroup name, as the table below drives it.
struct Refused {
    /// The operation refused.
    operation: &'static str,
    /// Its request, built around the workgroup name the row asks about.
    request: fn(&str) -> Value,
}

/// Every operation that resolves a `WorkGroup` refuses a name this account holds none under
/// with the one fixed sentence, whatever name the request carried.
#[tokio::test]
async fn a_workgroup_miss_is_refused_with_one_fixed_sentence_whatever_name_was_asked_about() {
    let router = router();
    create_work_group(&router, HELD_WORK_GROUP).await;

    // The service states the sentence once, for every operation that needs it; this is the
    // text it states.
    assert_eq!(
        state::WORK_GROUP_NOT_FOUND, WORK_GROUP_IS_NOT_FOUND,
        "the workgroup-miss sentence must be {WORK_GROUP_IS_NOT_FOUND:?}, got {:?}",
        state::WORK_GROUP_NOT_FOUND
    );

    // The names, differing along length, character class and their relation to the held
    // workgroup. Every one satisfies the modelled `WorkGroupName`
    // (smithy_query(query="shape",shape="WorkGroupName"): pattern ^[a-zA-Z0-9._-]{1,128}$),
    // so each reaches the handler rather than input validation.
    let longest = "w".repeat(128);
    let names: Vec<(&str, String)> = vec![
        ("a name nothing created", "never-created".to_string()),
        (
            "the shortest name the model admits",
            "w".to_string(),
        ),
        ("the longest name the model admits", longest),
        (
            "the held workgroup's name in another case",
            HELD_WORK_GROUP.to_uppercase(),
        ),
        (
            "a prefix of the held workgroup's name",
            HELD_WORK_GROUP[..8].to_string(),
        ),
        (
            "every character class the name pattern admits",
            "Wg.0_results-9".to_string(),
        ),
        ("digits alone", "0123456789".to_string()),
        (
            "a name carrying a unique suffix",
            "results-wg-missing-9ab3cde".to_string(),
        ),
    ];

    let operations: Vec<Refused> = vec![
        Refused {
            operation: "GetWorkGroup",
            request: |name| json!({ "WorkGroup": name }),
        },
        Refused {
            operation: "UpdateWorkGroup",
            request: |name| {
                json!({
                    "WorkGroup": name,
                    "Description": "should not apply",
                    "ConfigurationUpdates": { "EnforceWorkGroupConfiguration": true },
                })
            },
        },
        Refused {
            operation: "DeleteWorkGroup",
            request: |name| json!({ "WorkGroup": name }),
        },
        Refused {
            operation: "ListQueryExecutions",
            request: |name| json!({ "WorkGroup": name }),
        },
        Refused {
            operation: "ListNamedQueries",
            request: |name| json!({ "WorkGroup": name }),
        },
        Refused {
            operation: "ListPreparedStatements",
            request: |name| json!({ "WorkGroup": name }),
        },
        Refused {
            operation: "StartQueryExecution",
            request: |name| {
                json!({
                    "QueryString": "SELECT 1 AS one",
                    "WorkGroup": name,
                    "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
                })
            },
        },
    ];

    for (naming, name) in &names {
        for refused_call in &operations {
            let operation = refused_call.operation;
            let case = format!("{operation} with {naming} ({name:?})");
            let body = (refused_call.request)(name);
            let (status, refused) = call(&router, operation, &body).await;
            assert_refusal(
                &case,
                status,
                &refused,
                ERROR_CODE_INVALID_INPUT,
                WORK_GROUP_IS_NOT_FOUND,
            );
        }
    }

    // The workgroup the account really holds is still there: every call above refused, so
    // none of them deleted or updated anything.
    let (status, read) = call(
        &router,
        "GetWorkGroup",
        &json!({ "WorkGroup": HELD_WORK_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the held workgroup must still be readable, got {status} with {read:?}"
    );
    assert_eq!(
        read["WorkGroup"]["Name"].as_str(),
        Some(HELD_WORK_GROUP),
        "expected the held workgroup {HELD_WORK_GROUP:?}, got {read:?}"
    );
}
