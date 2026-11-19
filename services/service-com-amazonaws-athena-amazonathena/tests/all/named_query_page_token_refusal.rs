// SPDX-License-Identifier: CC0-1.0

//! How `ListNamedQueries` refuses a `NextToken` it did not mint, driven end to end over the
//! same router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`), so
//! every assertion is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: Amazon Athena words a pagination-token refusal the same way for every
//! one of its paginated reads, and that wording is the fixed text
//! `The nextPageToken is malformed: ` followed by the token
//! exactly as the request carried it — so a client reads back the token the listing would not
//! resume from. The refusal is this operation's own `InvalidRequestException` (HTTP 400,
//! `smithy_query(query="operation",operation="ListNamedQueries")`), and it writes nothing: the
//! listing it refused still reports the workgroup's saved queries, and a token this listing DID
//! hand back still resumes where it left off.
//!
//! The rows differ along what the rule generalises over — the form of the offered token: short
//! and long, padded, a token of the modelled maximum length (`Token` is `@length 1..1024`), one
//! another workgroup's listing of this same operation minted, and one another listing operation
//! minted.
//!
//! No expected value is read out of the answer being checked: every id asserted on is one
//! `CreateNamedQuery` answered with, and every token offered is either written here or one the
//! service handed back.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{self, ERROR_CODE_INVALID_INPUT};

use crate::harness::{assert_refusal, call, create_work_group, router};

/// The longest token the modelled `Token` admits (`@length 1..1024`), which is also the longest
/// a client can offer at all.
const LONGEST_TOKEN: usize = 1024;

/// Save one query in `work_group` and hand back the `NamedQueryId` Athena minted.
async fn save(router: &MultiServiceRouter, work_group: &str, name: &str) -> String {
    let (status, saved) = call(
        router,
        "CreateNamedQuery",
        &json!({
            "Name": name,
            "Database": "logs",
            "QueryString": "SELECT 1",
            "WorkGroup": work_group,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the query {name} must be saved in {work_group}, got {status} with {saved:?}"
    );
    saved["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNamedQuery must answer with an id, got {saved:?}"))
        .to_string()
}

/// One page of `ListNamedQueries`, as `(the ids it carried, the token it handed back)`.
async fn page(router: &MultiServiceRouter, request: &Value) -> (Vec<String>, Option<String>) {
    let (status, listed) = call(router, "ListNamedQueries", request).await;
    assert_eq!(
        status, 200,
        "ListNamedQueries({request}) must answer 200, got {status} with {listed:?}"
    );
    let ids = listed["NamedQueryIds"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect();
    (ids, listed["NextToken"].as_str().map(str::to_string))
}

/// A token `ListWorkGroups` minted — another listing operation of this service, whose tokens
/// name no page of this one.
async fn another_listings_token(router: &MultiServiceRouter) -> String {
    let (status, listed) = call(router, "ListWorkGroups", &json!({ "MaxResults": 1 })).await;
    assert_eq!(
        status, 200,
        "ListWorkGroups must answer 200, got {status} with {listed:?}"
    );
    listed["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a page of one of several workgroups hands back a token, got {listed:?}"))
        .to_string()
}

/// A token this listing did not mint is refused in Athena's own wording for this operation,
/// naming the token the request carried — and the refusal leaves the listing exactly as it was.
#[tokio::test]
async fn a_token_this_listing_did_not_mint_is_refused_naming_that_token() {
    let router = router();
    let work_group = "wg-named-query-tokens";
    let sibling = "wg-named-query-tokens-beside";
    create_work_group(&router, work_group).await;
    create_work_group(&router, sibling).await;

    let mut held = vec![
        save(&router, work_group, "first").await,
        save(&router, work_group, "second").await,
        save(&router, work_group, "third").await,
    ];
    held.sort();
    save(&router, sibling, "elsewhere-first").await;
    save(&router, sibling, "elsewhere-second").await;

    // A token the SIBLING workgroup's listing minted: this operation's own token, for another
    // workgroup's page.
    let (_, siblings_token) = page(&router, &json!({ "WorkGroup": sibling, "MaxResults": 1 })).await;
    let siblings_token =
        siblings_token.expect("a page of one of two saved queries hands back a token");
    let other_operations_token = another_listings_token(&router).await;

    // `(the case, the token offered)`.
    let rows: Vec<(&str, String)> = vec![
        (
            "a token of the form a client pastes from somewhere else",
            "not-a-real-pagination-token==".to_string(),
        ),
        ("a single character", "x".to_string()),
        (
            "a token of the modelled maximum length",
            "t".repeat(LONGEST_TOKEN),
        ),
        (
            "a token carrying the separator a cursor is built from",
            "namedqueries\u{1f}elsewhere\u{1f}1".to_string(),
        ),
        (
            "another workgroup's page of this same listing",
            siblings_token,
        ),
        ("a token another listing minted", other_operations_token),
    ];

    for (case, token) in &rows {
        let (status, refused) = call(
            &router,
            "ListNamedQueries",
            &json!({ "WorkGroup": work_group, "NextToken": token }),
        )
        .await;
        // Asserted against the wording itself, not only against the helper the handler words it
        // with, so a change to that wording fails here rather than agreeing with itself: the
        // fixed text is Athena's and the token the CALLER supplied is its one variable part.
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &format!("The nextPageToken is malformed: {token}"),
        );
        assert_eq!(
            crate::harness::message(&refused),
            state::page_token_malformed(token),
            "case {case:?}: expected the wording this service states a token refusal in, got \
             {refused:?}"
        );
    }

    // Nothing the refusals touched: the listing still reports every saved query of the
    // workgroup, and a token it DID hand back still resumes where it left off.
    let (mut listed, _) = page(&router, &json!({ "WorkGroup": work_group })).await;
    listed.sort();
    assert_eq!(
        listed, held,
        "expected the refused tokens to leave the listing reporting every saved query, got \
         {listed:?}"
    );

    let (first_page, token) = page(
        &router,
        &json!({ "WorkGroup": work_group, "MaxResults": 2 }),
    )
    .await;
    let token = token.expect("a page of two of three saved queries hands back a token");
    let (second_page, last) = page(
        &router,
        &json!({ "WorkGroup": work_group, "MaxResults": 2, "NextToken": token }),
    )
    .await;
    assert_eq!(
        last, None,
        "expected the last page to hand back no token, got {last:?}"
    );
    let mut walked: Vec<String> = first_page.into_iter().chain(second_page).collect();
    walked.sort();
    assert_eq!(
        walked, held,
        "expected the walk to report every saved query of the workgroup, got {walked:?}"
    );
}
