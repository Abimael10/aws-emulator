// SPDX-License-Identifier: CC0-1.0

//! How a directory listing reports the ids its FILTER could not resolve, driven end to end
//! over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body
//! (`crate::harness`), so nothing here reaches into the state struct and an answer that is
//! not computed from persisted state cannot pass.
//!
//! The rule under test: `DescribeDirectories$DirectoryIds` is a FILTER over the account's
//! directories, so a well-formed id no directory of the account holds is reported as an
//! INVALID VALUE of that filter — `EntityDoesNotExistException`, HTTP 400, whose `Message`
//! names the ids the filter could not resolve, unquoted, under the plural spelling of the
//! member they arrived in, separated by a bare comma, and closing with the call's own
//! request id. It is not the sentence an operation whose SUBJECT is one directory states
//! for the same id.
//!
//! It is asserted over a TABLE spanning what the report varies along — how many ids the
//! filter could not resolve (one, two, three), where they sit among resolvable ones, and a
//! filter that repeats one — with a filter that resolves as the positive control, so no row
//! can pass by matching one recorded message. The same file pins the sentence an EMPTY
//! filter is refused with, over every member of this service whose documentation makes an
//! empty list an `InvalidParameterException`.

use serde_json::json;

use crate::directory_lifecycle::{create_directory, minimal_create};
use crate::harness::{call, ok, service};

/// Well-formed `DirectoryId`s (`^d-[0-9a-f]{10}$`) this account never creates, varied along
/// their digits so no row can pass by matching one recorded id.
const ABSENT_IDS: [&str; 3] = ["d-90000ff0ab", "d-0000000000", "d-abcdef0123"];

/// What this service's refusal envelope appends to every diagnostic it words.
const REQUEST_ID_SEPARATOR: &str = " : RequestId: ";

/// RULE: the refusal names every id the filter could not resolve, in filter order, in the
/// invalid-value sentence of the member they arrived in.
#[tokio::test]
async fn an_unresolvable_filter_reports_its_own_values() {
    let router = service();
    // One stored directory, so a row mixing a resolvable id with absent ones reports only
    // the absent ones. The filter resolves the record from the create on, so nothing here
    // waits for the stage chain that create owes.
    let stored = create_directory(
        &router,
        minimal_create("filter-report.example.com"),
        "the stored one",
    )
    .await;

    // (case, the filter the call sends, the ids the refusal must name in that order)
    let rows: Vec<(&str, Vec<String>, Vec<&str>)> = vec![
        (
            "one unresolvable id",
            vec![ABSENT_IDS[0].to_string()],
            vec![ABSENT_IDS[0]],
        ),
        (
            "two unresolvable ids",
            vec![ABSENT_IDS[1].to_string(), ABSENT_IDS[0].to_string()],
            vec![ABSENT_IDS[1], ABSENT_IDS[0]],
        ),
        (
            "three unresolvable ids",
            ABSENT_IDS.iter().map(|id| (*id).to_string()).collect(),
            ABSENT_IDS.to_vec(),
        ),
        (
            "a stored id between two unresolvable ones",
            vec![
                ABSENT_IDS[2].to_string(),
                stored.clone(),
                ABSENT_IDS[0].to_string(),
            ],
            vec![ABSENT_IDS[2], ABSENT_IDS[0]],
        ),
        (
            "an unresolvable id the filter repeats",
            vec![
                ABSENT_IDS[1].to_string(),
                ABSENT_IDS[1].to_string(),
                ABSENT_IDS[2].to_string(),
            ],
            vec![ABSENT_IDS[1], ABSENT_IDS[2]],
        ),
    ];

    for (case, filter, named) in &rows {
        let (status, parsed) = call(
            &router,
            "DescribeDirectories",
            json!({ "DirectoryIds": filter }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the unresolvable filter to be refused, got {status} \
             with {parsed}"
        );
        assert_eq!(
            parsed["__type"],
            json!("EntityDoesNotExistException"),
            "case {case:?}: expected the modelled filter-value refusal, got {parsed}"
        );
        let request_id = parsed["RequestId"].as_str().unwrap_or_default();
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected the refusal to report a RequestId, got {parsed}"
        );
        let message = parsed["Message"].as_str().unwrap_or_default();
        assert_eq!(
            message,
            format!(
                "Invalid directory ID(s): {ids}.{REQUEST_ID_SEPARATOR}{request_id}",
                ids = named.join(",")
            ),
            "case {case:?}: expected the invalid-value sentence naming the filter's own \
             unresolved ids, got {message:?}"
        );
        // The sentence an operation whose SUBJECT is one directory states is a different
        // one, so a client can tell a bad filter value from a directory that is not there.
        assert!(
            !message.contains("does not exist"),
            "case {case:?}: expected the filter-value sentence rather than the \
             directory-lookup one, got {message:?}"
        );
    }

    // The positive control: a filter that resolves is SERVED, so what refuses the rows above
    // is the values and not the member.
    let answer = ok(
        &router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [&stored] }),
        "a filter that resolves",
    )
    .await;
    assert_eq!(
        answer["DirectoryDescriptions"][0]["DirectoryId"],
        json!(&stored),
        "expected the filtered listing to report the stored directory, got {answer}"
    );
}

/// RULE: a filter member whose own documentation makes an EMPTY list an
/// `InvalidParameterException` is refused with the sentence that names the member and what it
/// is a list of — one wording for every such filter of this service.
#[tokio::test]
async fn an_empty_filter_is_refused_by_naming_the_member_and_its_entries() {
    let router = service();

    // (case, operation, a body whose filter is EMPTY, the member, the shape its entries
    // carry) — the three members whose documentation reads "An empty list results in an
    // InvalidParameterException being thrown"
    // (`smithy_query(query="shape",shape="DescribeDirectoriesRequest,DescribeEventTopicsRequest,DescribeTrustsRequest")`).
    let rows: [(&str, &str, serde_json::Value, &str, &str); 3] = [
        (
            "a directory listing's filter",
            "DescribeDirectories",
            json!({ "DirectoryIds": [] }),
            "DirectoryIds",
            "DirectoryId",
        ),
        (
            "an event topic listing's filter",
            "DescribeEventTopics",
            json!({ "DirectoryId": ABSENT_IDS[0], "TopicNames": [] }),
            "TopicNames",
            "TopicName",
        ),
        (
            "a trust listing's filter",
            "DescribeTrusts",
            json!({ "DirectoryId": ABSENT_IDS[0], "TrustIds": [] }),
            "TrustIds",
            "TrustId",
        ),
    ];

    for (case, operation, body, member, element) in rows {
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the empty filter to be refused, got {status} with \
             {parsed}"
        );
        assert_eq!(
            parsed["__type"],
            json!("InvalidParameterException"),
            "case {case:?}: expected the modelled parameter refusal, got {parsed}"
        );
        let request_id = parsed["RequestId"].as_str().unwrap_or_default();
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected the refusal to report a RequestId, got {parsed}"
        );
        let message = parsed["Message"].as_str().unwrap_or_default();
        assert_eq!(
            message,
            format!(
                "If specified, {member} must have at least one \
                 {element}.{REQUEST_ID_SEPARATOR}{request_id}"
            ),
            "case {case:?}: expected the empty-filter sentence, got {message:?}"
        );
    }
}
