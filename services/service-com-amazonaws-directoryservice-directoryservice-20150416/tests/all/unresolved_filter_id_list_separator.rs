// SPDX-License-Identifier: CC0-1.0

//! How the ids of a refused `DirectoryIds` FILTER are separated inside the one message that
//! reports them, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body
//! (`crate::harness`), so nothing here reaches into the state struct.
//!
//! The rule under test: where a refusal quotes a LIST member's values back as one value
//! list, the values are separated by a bare comma and nothing else, so splitting that list
//! on the comma hands every value back byte for byte as the request sent it — a separator
//! carrying a space returns every value after the first with a leading space it was never
//! sent with. `DescribeDirectories`' `EntityDoesNotExistException` over an unresolvable
//! `DirectoryIds` filter is that list, and the single-value form is the same sentence with
//! no separator in it at all.
//!
//! It is asserted over a TABLE spanning what the list varies along — how many values it
//! carries (one, two, three, and the published per-request bound), their order, a value the
//! filter repeats, and values sitting either side of a resolvable id — and each row both
//! compares the whole sentence and recovers the values by splitting on the separator, so no
//! row can pass by matching one recorded message.

use serde_json::json;
use service_com_amazonaws_directoryservice_directoryservice_20150416::state::MAX_DIRECTORY_IDS_PER_REQUEST;

use crate::directory_lifecycle::{create_directory, minimal_create};
use crate::harness::{call, service};

/// What this service's refusal envelope appends to every diagnostic it words.
const REQUEST_ID_SEPARATOR: &str = " : RequestId: ";

/// A well-formed `DirectoryId` (`^d-[0-9a-f]{10}$`, the pattern
/// `smithy_query(query="shape",shape="DirectoryId")` publishes) this account never creates,
/// made from `nth` so a row needs as many distinct absent ids as it likes and no row can
/// pass by matching one recorded id.
fn absent_id(nth: usize) -> String {
    format!("d-{nth:010x}")
}

/// RULE: the ids of a refused filter are rendered as one value list separated by a BARE
/// comma, whatever the list's length, order or repeats, so splitting on it recovers exactly
/// the ids the request sent.
#[tokio::test]
async fn the_refused_filter_values_are_separated_by_a_bare_comma() {
    let router = service();
    // One stored directory, so a row that mixes a resolvable id in names only the absent
    // ones. The filter resolves the record from the create on, so nothing here waits for
    // the stage chain the create owes.
    let stored = create_directory(
        &router,
        minimal_create("separator.example.com"),
        "the stored one",
    )
    .await;

    // The bound the service publishes on how many ids one request may filter by: the
    // longest list this message ever renders, so the separator is pinned there too.
    let at_the_bound: Vec<String> = (0..MAX_DIRECTORY_IDS_PER_REQUEST).map(absent_id).collect();

    // (case, the filter the call sends, the ids the refusal must name in that order)
    let rows: Vec<(String, Vec<String>, Vec<String>)> = vec![
        (
            "one unresolvable id".to_string(),
            vec![absent_id(7)],
            vec![absent_id(7)],
        ),
        (
            "two unresolvable ids".to_string(),
            vec![absent_id(1), absent_id(2)],
            vec![absent_id(1), absent_id(2)],
        ),
        (
            "the same two in the other order".to_string(),
            vec![absent_id(2), absent_id(1)],
            vec![absent_id(2), absent_id(1)],
        ),
        (
            "three unresolvable ids".to_string(),
            vec![absent_id(3), absent_id(255), absent_id(4_095)],
            vec![absent_id(3), absent_id(255), absent_id(4_095)],
        ),
        (
            "a resolvable id between two unresolvable ones".to_string(),
            vec![absent_id(9), stored.clone(), absent_id(10)],
            vec![absent_id(9), absent_id(10)],
        ),
        (
            "an unresolvable id the filter repeats".to_string(),
            vec![absent_id(11), absent_id(11), absent_id(12)],
            vec![absent_id(11), absent_id(12)],
        ),
        (
            format!("{MAX_DIRECTORY_IDS_PER_REQUEST} unresolvable ids, the published bound"),
            at_the_bound.clone(),
            at_the_bound,
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
            "case {case:?}: expected the unresolved ids joined by a bare comma, got \
             {message:?}"
        );

        // What a client does with the message: cut the value list out of the sentence and
        // split it on the separator. Every value must come back exactly as it was sent,
        // with no space of the separator's clinging to it.
        let listed = message
            .split_once("Invalid directory ID(s): ")
            .and_then(|(_, rest)| rest.split_once('.'))
            .map(|(ids, _)| ids)
            .unwrap_or_default();
        let recovered: Vec<&str> = listed.split(',').collect();
        assert_eq!(
            recovered, *named,
            "case {case:?}: expected the ids to be recoverable by splitting the list on the \
             separator, got {recovered:?}"
        );
        assert!(
            recovered
                .iter()
                .all(|id| *id == id.trim() && !id.is_empty()),
            "case {case:?}: expected every recovered id to carry no separator whitespace, \
             got {recovered:?}"
        );
        assert!(
            !listed.contains(' '),
            "case {case:?}: expected the value list to carry no spaces, got {listed:?}"
        );
    }
}
