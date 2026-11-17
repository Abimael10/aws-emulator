// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListOTAUpdates`: the OTA updates this account holds, as
//! one page of summaries read off the very records `CreateOTAUpdate` persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `GET /otaUpdates` signed for the `iot` signing name — against the emulator, the
//! `CreateOTAUpdate`/`GetOTAUpdate`/`DeleteOTAUpdate` helpers and the harness of the
//! parent suite, so a listing is compared with what the single-update reader reports for
//! the same id.
//!
//! The rules under test:
//!
//! * the listing reports every recorded update, in `otaUpdateId` order, each summarised
//!   as the id, the ARN and the creation instant the stored record carries — and nothing
//!   else; an account holding none is answered an empty list rather than an absent one;
//! * `maxResults` bounds one page and `nextToken` resumes the SAME listing exactly where
//!   the page stopped, so walking the token chain visits every update once and the last
//!   page carries no token;
//! * `otaUpdateStatus` selects exactly the updates holding that status, so the six
//!   modelled statuses PARTITION the unfiltered listing, and once the creation the clock
//!   owes has completed every update is selected by `CREATE_COMPLETE` and by no other;
//! * a request the listing does not admit — a `nextToken` it never issued, one issued for
//!   a different filter, a `maxResults` outside the modelled range — is refused and lists
//!   nothing;
//! * the listing projects the store as it IS, so an update a delete removed is listed no
//!   more while its neighbours still are.
//!
//! The tables span what the rules generalise over: how many updates the account holds, in
//! which id order they were created, which page size the request asks for (both modelled
//! bounds included, and one past each), and which of the modelled statuses it filters on.

use super::*;

/// `GET /otaUpdates`, the listing's modelled HTTP binding
/// (`smithy_query(query="operation",operation="ListOTAUpdates")`).
const LISTING_PATH: &str = "/otaUpdates";

/// Every value of the modelled `OTAUpdateStatus`
/// (`smithy_query(query="shape",shape="OTAUpdateStatus")`): the whole catalog the
/// `otaUpdateStatus` filter is drawn from, so the partition rule is checked over all of
/// it rather than over the two statuses this emulator's lifecycle reaches.
const OTA_UPDATE_STATUSES: [&str; 6] = [
    "CREATE_PENDING",
    "CREATE_IN_PROGRESS",
    "CREATE_COMPLETE",
    "CREATE_FAILED",
    "DELETE_IN_PROGRESS",
    "DELETE_FAILED",
];

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long a poll keeps looking for a transition the clock owes. Generous on purpose:
/// the test waits for the CONDITION, and a loaded host may take far longer than the span
/// itself to get there.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// The `nextToken` refusal this listing answers a token it did not issue with.
const REFUSED_TOKEN_MESSAGE: &str = "nextToken is not a continuation token of this listing";

/// One `GET /otaUpdates` with `query` appended (no leading `?`).
async fn list(emulator: &Emulator, query: &str) -> Reply {
    let path = if query.is_empty() {
        LISTING_PATH.to_string()
    } else {
        format!("{LISTING_PATH}?{query}")
    };
    emulator.call("GET", &path, None, REGION).await
}

/// The `otaUpdates` member of a listing, which a served listing always carries.
fn summaries(case: &str, reply: &Reply) -> Vec<Value> {
    let listed = reply
        .body
        .get("otaUpdates")
        .unwrap_or_else(|| panic!("case {case:?}: a served listing carries otaUpdates: {:?}", reply.body));
    listed
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: otaUpdates is a list: {:?}", reply.body))
        .clone()
}

/// The ids a listing reported, in the order it reported them.
fn listed_ids(case: &str, reply: &Reply) -> Vec<String> {
    summaries(case, reply)
        .iter()
        .map(|summary| {
            summary
                .get("otaUpdateId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a summary names its update: {summary:?}"))
                .to_string()
        })
        .collect()
}

/// Record one OTA update under `ota_update_id`, targeting a device registered for it.
async fn record_update(case: &str, emulator: &Emulator, ota_update_id: &str, files: &Value) {
    let thing_name = format!("listing-{ota_update_id}");
    let registered = emulator.create_thing(&thing_name).await;
    assert_eq!(
        registered.status, 200,
        "case {case:?}: the targeted device must be registrable, got {} {:?}",
        registered.status, registered.body
    );
    let request = json!({
        "targets": [thing_arn(&thing_name)],
        "files": files.clone(),
        "roleArn": ROLE_ARN,
    });
    let created = emulator.create_ota_update(ota_update_id, &request).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the update must be recordable, got {} {:?}",
        created.status, created.body
    );
}

/// An account holding one update per id, each with one S3 file.
async fn seeded(case: &str, ids: &[&str]) -> Emulator {
    let emulator = Emulator::new();
    for id in ids {
        record_update(case, &emulator, id, &json!([s3_file("firmware.bin", "1.0.0")])).await;
    }
    emulator
}

/// The ids `ids` in the order the store keys them: `otaUpdateId` order.
fn in_id_order(ids: &[&str]) -> Vec<String> {
    let mut expected: Vec<String> = ids.iter().map(|id| (*id).to_string()).collect();
    expected.sort();
    expected
}

/// Walk every page of `query`'s listing, `page_size` updates at a time, and answer the
/// ids in the order they were reported together with how many pages it took.
async fn walk_pages(
    case: &str,
    emulator: &Emulator,
    query: &str,
    page_size: usize,
    total: usize,
) -> (Vec<String>, usize) {
    let mut walked: Vec<String> = Vec::new();
    let mut pages = 0usize;
    let mut token: Option<String> = None;
    // One page per update is the most a correct listing can need; a token chain that
    // outruns that is reporting the same page for ever rather than advancing.
    let ceiling = total + 2;
    loop {
        let mut request = format!("maxResults={page_size}");
        if !query.is_empty() {
            request = format!("{query}&{request}");
        }
        if let Some(token) = &token {
            request = format!("{request}&nextToken={token}");
        }
        let page = list(emulator, &request).await;
        assert_eq!(
            page.status, 200,
            "case {case:?}: page {pages} must be served, got {} {:?}",
            page.status, page.body
        );
        let ids = listed_ids(case, &page);
        assert!(
            ids.len() <= page_size,
            "case {case:?}: page {pages} must hold at most {page_size} updates, got {ids:?}"
        );
        walked.extend(ids);
        pages += 1;
        token = page.text("nextToken");
        if token.is_none() {
            return (walked, pages);
        }
        assert!(
            pages < ceiling,
            "case {case:?}: the token chain must end within {ceiling} pages, got {walked:?}"
        );
    }
}

/// RULE: the listing reports every recorded update, in `otaUpdateId` order, as the three
/// members `OTAUpdateSummary` declares — each read off the record `CreateOTAUpdate`
/// persisted, so the listing and `GetOTAUpdate` report one value.
#[tokio::test]
async fn every_recorded_update_is_listed_as_the_record_the_create_persisted() {
    // An account that holds nothing: "nothing recorded" is an answer, not a missing one.
    let empty = Emulator::new();
    let none = list(&empty, "").await;
    assert_eq!(
        none.status, 200,
        "case \"an account holding no update\": expected 200, got {} {:?}",
        none.status, none.body
    );
    assert_eq!(
        listed_ids("an account holding no update", &none),
        Vec::<String>::new(),
        "case \"an account holding no update\": expected an empty listing, got {:?}",
        none.body
    );
    assert_eq!(
        none.text("nextToken"),
        None,
        "case \"an account holding no update\": an exhausted listing carries no token, got {:?}",
        none.body
    );

    // The ids are created in an order that is NOT their sorted order, and span the forms
    // `OTAUpdateId` admits (letters, digits, `-`, `_`, upper case), so "the order the
    // listing reports" cannot be insertion order by accident.
    let case = "five updates created out of id order";
    let created = ["ota-zulu", "ota-alpha", "OTA-Upper", "ota_mike_2", "ota-9"];
    let emulator = seeded(case, &created).await;

    // A second file form and a second update member on one of the rows: a summary reports
    // the same three members whatever else the record carries.
    record_update(
        case,
        &emulator,
        "ota-streamed",
        &json!([
            s3_file("firmware.bin", "2.0.0"),
            stream_file("bootloader.bin", "stream-one", 1)
        ]),
    )
    .await;

    let mut expected = in_id_order(&created);
    expected.push("ota-streamed".to_string());
    expected.sort();

    let listed = list(&emulator, "").await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200, got {} {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed_ids(case, &listed),
        expected,
        "case {case:?}: expected every recorded update in otaUpdateId order, got {:?}",
        listed.body
    );
    assert_eq!(
        listed.text("nextToken"),
        None,
        "case {case:?}: a listing that returned every update carries no token, got {:?}",
        listed.body
    );

    for summary in summaries(case, &listed) {
        let id = summary["otaUpdateId"].as_str().expect("a summary names its update");
        let read = emulator.get_ota_update(id).await;
        assert_eq!(
            read.status, 200,
            "case {id:?}: the listed update must be readable, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            summary.get("otaUpdateArn").and_then(Value::as_str),
            Some(format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:otaupdate/{id}").as_str()),
            "case {id:?}: expected the stored OTA update ARN, got {summary:?}"
        );
        assert_eq!(
            summary.get("otaUpdateArn"),
            read.info_member("otaUpdateArn"),
            "case {id:?}: the listing and the reader must report one ARN, got {summary:?} and {:?}",
            read.body
        );
        assert_eq!(
            summary.get("creationDate"),
            read.info_member("creationDate"),
            "case {id:?}: the listing and the reader must report one creation instant, got \
             {summary:?} and {:?}",
            read.body
        );
        // `OTAUpdateSummary` is "An OTA update summary" of exactly three members, so a
        // listing states those and invents nothing beside them.
        let mut members: Vec<&str> = summary
            .as_object()
            .expect("a summary is an object")
            .keys()
            .map(String::as_str)
            .collect();
        members.sort_unstable();
        assert_eq!(
            members,
            ["creationDate", "otaUpdateArn", "otaUpdateId"],
            "case {id:?}: expected the three members OTAUpdateSummary declares, got {summary:?}"
        );
    }
}

/// RULE: `maxResults` bounds one page and `nextToken` resumes the same listing where the
/// page stopped, so the token chain reports every update exactly once, in the listing's
/// order, and ends without a token.
#[tokio::test]
async fn the_token_chain_walks_the_listing_once_whatever_the_page_size() {
    let case = "seven updates paged";
    let created = [
        "ota-page-a",
        "ota-page-b",
        "ota-page-c",
        "ota-page-d",
        "ota-page-e",
        "ota-page-f",
        "ota-page-g",
    ];
    let emulator = seeded(case, &created).await;
    let expected = in_id_order(&created);

    // The page sizes span the class: the modelled minimum, sizes that divide the account
    // and sizes that do not, the count itself, one past it, and the modelled maximum.
    for page_size in [1usize, 2, 3, 7, 8, 250] {
        let case = format!("{case}, maxResults={page_size}");
        let (walked, pages) = walk_pages(&case, &emulator, "", page_size, expected.len()).await;
        assert_eq!(
            walked, expected,
            "case {case:?}: expected the whole listing once, in id order, got {walked:?}"
        );
        let wanted_pages = expected.len().div_ceil(page_size).max(1);
        assert_eq!(
            pages, wanted_pages,
            "case {case:?}: expected {wanted_pages} page(s) of {page_size}, got {pages}"
        );
    }

    // A page that stops short of the end carries the token, and the update it resumes
    // after is the last one it reported.
    let first = list(&emulator, "maxResults=3").await;
    let token = first
        .text("nextToken")
        .unwrap_or_else(|| panic!("a truncated listing carries a token, got {:?}", first.body));
    let rest = list(&emulator, &format!("maxResults=250&nextToken={token}")).await;
    assert_eq!(
        listed_ids("resumed after the third update", &rest),
        expected[3..].to_vec(),
        "case \"resumed after the third update\": expected the updates past the first page, got \
         {:?}",
        rest.body
    );
    assert_eq!(
        rest.text("nextToken"),
        None,
        "case \"resumed after the third update\": the last page carries no token, got {:?}",
        rest.body
    );
}

/// RULE: `otaUpdateStatus` selects exactly the updates holding that status — so once the
/// creation the clock owes has completed, the completed status selects every update, no
/// other modelled status selects any, and the statuses together partition the unfiltered
/// listing.
#[tokio::test]
async fn the_status_filter_selects_exactly_the_updates_holding_that_status() {
    let case = "three updates filtered by status";
    let created = ["ota-status-a", "ota-status-b", "ota-status-c"];
    let emulator = seeded(case, &created).await;
    let expected = in_id_order(&created);

    // The completion the clock owes each update is waited FOR, not slept for: the listing
    // is read again until the completed status selects every update. Past that the status
    // of each update is terminal, so what every other filter answers is stable.
    let deadline = Instant::now() + POLL_DEADLINE;
    let completed = loop {
        let filtered = list(
            &emulator,
            &format!("otaUpdateStatus={CREATED_OTA_UPDATE_STATUS}"),
        )
        .await;
        assert_eq!(
            filtered.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            filtered.status, filtered.body
        );
        if listed_ids(case, &filtered) == expected {
            break filtered;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every update's creation must complete, got {:?}",
            filtered.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        completed.text("nextToken"),
        None,
        "case {case:?}: a filtered listing that returned everything carries no token, got {:?}",
        completed.body
    );

    // Each update is in exactly ONE status, so the six filtered listings are disjoint and
    // together are the unfiltered listing.
    let mut partitioned: Vec<String> = Vec::new();
    for status in OTA_UPDATE_STATUSES {
        let case = format!("{case}, otaUpdateStatus={status}");
        let filtered = list(&emulator, &format!("otaUpdateStatus={status}")).await;
        assert_eq!(
            filtered.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            filtered.status, filtered.body
        );
        let ids = listed_ids(&case, &filtered);
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            ids, sorted,
            "case {case:?}: a filtered listing keeps the listing's id order, got {ids:?}"
        );
        let wanted: Vec<String> = if status == CREATED_OTA_UPDATE_STATUS {
            expected.clone()
        } else {
            Vec::new()
        };
        assert_eq!(
            ids, wanted,
            "case {case:?}: expected exactly the updates holding that status, got {:?}",
            filtered.body
        );
        for id in ids {
            assert!(
                !partitioned.contains(&id),
                "case {case:?}: {id:?} was already selected by another status, got {partitioned:?}"
            );
            partitioned.push(id);
        }
    }
    partitioned.sort();
    assert_eq!(
        partitioned,
        listed_ids(case, &list(&emulator, "").await),
        "case {case:?}: the statuses must partition the unfiltered listing, got {partitioned:?}"
    );

    // A filtered listing pages like any other, and its token belongs to IT: the
    // unfiltered listing does not accept it.
    let (walked, pages) = walk_pages(
        case,
        &emulator,
        &format!("otaUpdateStatus={CREATED_OTA_UPDATE_STATUS}"),
        1,
        expected.len(),
    )
    .await;
    assert_eq!(
        walked, expected,
        "case {case:?}: a filtered listing pages through every selected update, got {walked:?}"
    );
    assert_eq!(
        pages,
        expected.len(),
        "case {case:?}: expected one page per selected update, got {pages}"
    );
}

/// RULE: a request the listing does not admit — a `nextToken` it never issued, a token
/// issued for another filter, a `maxResults` outside the modelled range — is refused and
/// lists nothing; both published bounds of the range are served.
#[tokio::test]
async fn a_request_the_listing_does_not_admit_is_refused() {
    let case = "four updates, refused requests";
    let created = [
        "ota-refuse-a",
        "ota-refuse-b",
        "ota-refuse-c",
        "ota-refuse-d",
    ];
    let emulator = seeded(case, &created).await;

    // A token is a position in ONE listing: a token the filtered listing issued names no
    // position of the unfiltered one, and the other way round.
    let unfiltered = list(&emulator, "maxResults=1").await;
    let unfiltered_token = unfiltered
        .text("nextToken")
        .unwrap_or_else(|| panic!("a truncated listing carries a token, got {:?}", unfiltered.body));
    let filtered = list(
        &emulator,
        &format!("otaUpdateStatus={NEW_OTA_UPDATE_STATUS}&maxResults=1"),
    )
    .await;
    let filtered_token = filtered.text("nextToken");

    let mut refusals: Vec<(String, String)> = vec![
        (
            "a nextToken that is not a token of any listing".to_string(),
            "nextToken=not-a-token".to_string(),
        ),
        (
            "a nextToken that is hex but names no position".to_string(),
            format!("nextToken={}", hex::encode("nonsense")),
        ),
        (
            "a nextToken whose resume point is no OTAUpdateId".to_string(),
            format!("nextToken={}", hex::encode("\u{1f}not a valid id")),
        ),
        (
            "a token the unfiltered listing issued, replayed on a filtered one".to_string(),
            format!("otaUpdateStatus={NEW_OTA_UPDATE_STATUS}&nextToken={unfiltered_token}"),
        ),
    ];
    if let Some(token) = &filtered_token {
        refusals.push((
            "a token a filtered listing issued, replayed on the unfiltered one".to_string(),
            format!("nextToken={token}"),
        ));
    }

    for (case, query) in &refusals {
        let refused = list(&emulator, query).await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (400, Some(QUALIFIED_INVALID_REQUEST)),
            "case {case:?}: expected 400 InvalidRequestException, got {} {:?} with {:?}",
            refused.status,
            refused.error_code,
            refused.body
        );
        assert_eq!(
            refused.message(),
            REFUSED_TOKEN_MESSAGE,
            "case {case:?}: expected the token refusal's wording, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.body.get("otaUpdates"),
            None,
            "case {case:?}: a refused request lists nothing, got {:?}",
            refused.body
        );
    }

    // The issuing listing still accepts its own token: refusing a token is about the
    // listing it belongs to, not about tokens.
    let resumed = list(&emulator, &format!("maxResults=1&nextToken={unfiltered_token}")).await;
    assert_eq!(
        resumed.status, 200,
        "case \"the issuing listing replays its own token\": expected 200, got {} {:?}",
        resumed.status, resumed.body
    );

    // `MaxResults` is `range 1..250`: both published bounds are served, and a value
    // outside the range is refused before the listing runs.
    for served in ["1", "250"] {
        let listed = list(&emulator, &format!("maxResults={served}")).await;
        assert_eq!(
            listed.status, 200,
            "case maxResults={served}: the modelled bound must be served, got {} {:?}",
            listed.status, listed.body
        );
    }
    for out_of_range in ["0", "-1", "251"] {
        let refused = list(&emulator, &format!("maxResults={out_of_range}")).await;
        assert_eq!(
            refused.status, 400,
            "case maxResults={out_of_range}: must be refused 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.body.get("otaUpdates"),
            None,
            "case maxResults={out_of_range}: a refused request lists nothing, got {:?}",
            refused.body
        );
    }
}

/// RULE: the listing projects the store as it IS, so an update a delete removed is listed
/// no more while every other recorded update still is.
#[tokio::test]
async fn an_update_a_delete_removed_is_listed_no_more() {
    let case = "one of three updates deleted";
    let created = ["ota-kept-a", "ota-dropped", "ota-kept-b"];
    let emulator = seeded(case, &created).await;

    let before = list(&emulator, "").await;
    assert_eq!(
        listed_ids(case, &before),
        in_id_order(&created),
        "case {case:?}: expected every recorded update before the delete, got {:?}",
        before.body
    );

    // The job the update deployed is still running (its targeted device has not reported
    // back), which is exactly the case `forceDeleteAWSJob` exists for.
    let deleted = emulator
        .delete_ota_update("ota-dropped", Some(true), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: the delete must be served, got {} {:?}",
        deleted.status, deleted.body
    );

    let after = list(&emulator, "").await;
    assert_eq!(
        listed_ids(case, &after),
        in_id_order(&["ota-kept-a", "ota-kept-b"]),
        "case {case:?}: expected the deleted update to be gone and its neighbours kept, got {:?}",
        after.body
    );
    let read = emulator.get_ota_update("ota-dropped").await;
    assert_eq!(
        read.status, 404,
        "case {case:?}: the deleted update must be unreadable too, got {} {:?}",
        read.status, read.body
    );
}
