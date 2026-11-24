// SPDX-License-Identifier: CC0-1.0

//! How `ListCapacityReservations` refuses a `NextToken` it did not mint, driven end to end
//! over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`), so every assertion is about what an SDK client
//! observes.
//!
//! THE RULE UNDER TEST: Athena's capacity-reservation listing refuses a `NextToken` no page
//! of it ever handed out with its own `InvalidRequestException` (HTTP 400,
//! `smithy_query(query="operation",operation="ListCapacityReservations")`), under the
//! `INVALID_INPUT` class, and the refusal is a VERDICT on the token rather than a report of
//! it: the whole sentence is fixed text, so no part of the request reaches it and two
//! differently-forged tokens are refused with identical bytes. That is what separates this
//! listing from the query, workgroup, catalog and tag listings of the same service, which
//! name the token they would not resume from
//! (`crate::work_group_summary_projection` asserts both halves of that split).
//!
//! The rows differ along what the rule generalises over — the FORM of the offered token:
//! a value that is no token at all, one single character, one of the modelled maximum length
//! (`NextToken` targets `Token`, `@length 1..1024`), one too short to carry an
//! authenticator, hexadecimal of a minted token's shape that nobody computed, one carrying
//! the separator a cursor is built from, one another account's copy of THIS listing minted,
//! and one another listing of this same account minted.
//!
//! And the refusal changes nothing: afterwards the listing still reports every reservation
//! the creates registered, and a token this listing DID hand back still resumes where it
//! left off.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    CAPACITY_RESERVATION_PAGE_TOKEN_INVALID, ERROR_CODE_INVALID_INPUT,
};
use service_common::page_tokens::AUTHENTICATOR_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, create_work_group, message, router};

/// The longest `NextToken` the modelled `Token` admits (`@length 1..1024`), which is also
/// the longest value a client can offer at all.
const LONGEST_TOKEN: usize = 1024;

/// A number of data processing units inside the modelled `TargetDpusInteger` range and on
/// its increment, so every create below is accepted.
const SOME_TARGET_DPUS: i32 = 24;

/// Register one capacity reservation over the wire.
async fn create(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateCapacityReservation",
        &json!({ "Name": name, "TargetDpus": SOME_TARGET_DPUS }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the reservation {name} must be created, got {status} with {created:?}"
    );
}

/// One accepted page of the listing, as `(the names it carried, the token it handed back)`.
async fn page(router: &MultiServiceRouter, request: &Value) -> (Vec<String>, Option<String>) {
    let (status, listed) = call(router, "ListCapacityReservations", request).await;
    assert_eq!(
        status, 200,
        "ListCapacityReservations({request}) must answer 200, got {status} with {listed:?}"
    );
    let names = listed["CapacityReservations"]
        .as_array()
        .unwrap_or_else(|| panic!("a listing must carry CapacityReservations, got {listed:?}"))
        .iter()
        .map(|entry| {
            entry["Name"]
                .as_str()
                .unwrap_or_else(|| panic!("every entry names its reservation, got {entry:?}"))
                .to_string()
        })
        .collect();
    (names, listed["NextToken"].as_str().map(str::to_string))
}

/// A token `ListWorkGroups` minted: another listing of this same account, whose tokens name
/// no page of the capacity-reservation registry.
async fn another_listings_token(router: &MultiServiceRouter) -> String {
    create_work_group(router, "capacity-token-neighbour").await;
    let (status, listed) = call(router, "ListWorkGroups", &json!({ "MaxResults": 1 })).await;
    assert_eq!(
        status, 200,
        "ListWorkGroups must answer 200, got {status} with {listed:?}"
    );
    listed["NextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("a page of one of several workgroups hands back a token, got {listed:?}")
        })
        .to_string()
}

/// Every form of a token this listing did not mint is refused with the one fixed sentence
/// Athena words this class in, and the refusals leave the listing exactly as it was.
#[tokio::test]
async fn a_token_this_listing_did_not_mint_is_refused_with_one_fixed_sentence() {
    let account = router();
    let mut held: Vec<String> = Vec::new();
    for index in 0..3 {
        let name = format!("capacity-token-case-{index}");
        create(&account, &name).await;
        held.push(name);
    }
    held.sort();

    // Another account's copy of THIS listing: the same operation, a registry of its own, so
    // the token it minted authenticates against another store's secret.
    let other_account = router();
    for index in 0..3 {
        create(&other_account, &format!("capacity-token-case-{index}")).await;
    }
    let (_, other_accounts_token) = page(&other_account, &json!({ "MaxResults": 1 })).await;
    let other_accounts_token =
        other_accounts_token.expect("a page of one of three reservations hands back a token");
    let other_operations_token = another_listings_token(&account).await;

    // `(the case, the token offered)`.
    let rows: Vec<(&str, String)> = vec![
        (
            "a value that is no token at all",
            "not-a-real-pagination-token==".to_string(),
        ),
        ("a single character", "x".to_string()),
        (
            "a token of the modelled maximum length",
            "t".repeat(LONGEST_TOKEN),
        ),
        ("a value too short to carry an authenticator", "00".to_string()),
        (
            "the shape of a minted token with an authenticator nobody computed",
            ::hex::encode(vec![0_u8; AUTHENTICATOR_BYTES + 16]),
        ),
        (
            "a token carrying the separator a cursor is built from",
            "capacityreservations\u{1f}1".to_string(),
        ),
        (
            "another account's page of this same listing",
            other_accounts_token,
        ),
        (
            "a token another listing of this account minted",
            other_operations_token,
        ),
    ];

    let mut sentences: Vec<String> = Vec::new();
    for (case, token) in &rows {
        let (status, refused) = call(
            &account,
            "ListCapacityReservations",
            &json!({ "MaxResults": 2, "NextToken": token }),
        )
        .await;
        // Asserted against the wording itself, not only against the constant the handler
        // words it with, so a change to that wording fails here rather than agreeing with
        // itself.
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            "Invalid next token provided",
        );
        assert_eq!(
            message(&refused),
            CAPACITY_RESERVATION_PAGE_TOKEN_INVALID,
            "case {case:?}: expected the sentence this listing states a token refusal in, \
             got {refused:?}"
        );
        // Fixed text end to end: nothing of the request reaches the sentence, so the
        // offered token cannot appear in it. Asked only of a token of more than one
        // character, since a single letter occurs in any English sentence by itself.
        if token.chars().count() > 1 {
            assert!(
                !message(&refused).contains(token.as_str()),
                "case {case:?}: the refusal must not carry the token it refused, got {:?}",
                message(&refused)
            );
        }
        sentences.push(message(&refused));
    }
    assert!(
        sentences.windows(2).all(|pair| pair[0] == pair[1]),
        "every refused token must read identically, got {sentences:?}"
    );

    // Nothing the refusals touched: the listing still reports every reservation the creates
    // registered, and a token it DID hand back still resumes where it left off.
    let (listed, _) = page(&account, &json!({ "MaxResults": 50 })).await;
    assert_eq!(
        listed, held,
        "expected the refused tokens to leave the listing reporting every reservation, got \
         {listed:?}"
    );

    let (first, token) = page(&account, &json!({ "MaxResults": 2 })).await;
    let token = token.expect("a page of two of three reservations hands back a token");
    let (second, last) = page(
        &account,
        &json!({ "MaxResults": 2, "NextToken": token }),
    )
    .await;
    assert_eq!(
        last, None,
        "expected the last page to hand back no token, got {last:?}"
    );
    let walked: Vec<String> = first.into_iter().chain(second).collect();
    assert_eq!(
        walked, held,
        "expected the walk to report every reservation the account holds, got {walked:?}"
    );
}
