// SPDX-License-Identifier: CC0-1.0

//! What a PAGE of one daemon's rollout history is, beside the filters
//! [`crate::daemon_scheduling`] pins: the page size a `ListDaemonDeployments` request may
//! ask for, the page it is served when it asks for none, and the continuation token that
//! listing honours.
//!
//! The rules under test, each asserted over a table that spans its class rather than over
//! one request:
//!
//! * "This value can be between 1 and 100. If this parameter isn't used, then
//!   ListDaemonDeployments returns up to 20 results and a nextToken value if applicable"
//!   (`smithy_query(query="doc",shape="ListDaemonDeploymentsRequest",member="maxResults")`)
//!   — a page size outside that inclusive range is refused with `InvalidParameterException`
//!   stating the range, either published bound is inside it and so is paged with, and a
//!   request that names no page size is served the documented default with a token for the
//!   rest;
//! * `nextToken` "should be treated as an opaque identifier that is only used to retrieve
//!   the next items in a list and not for other programmatic purposes" — the listing's own
//!   token resumes exactly where its page stopped, and a value this listing never minted
//!   (one a caller assembled, one minted for another daemon's history, one minted by
//!   another operation) names no page and is refused as the identifier that isn't valid it
//!   is;
//! * the `createdAt` window a request asks for is bounded by the history there is to report
//!   — a bound reaching further back than the deployment records are kept for is refused with
//!   the sentence naming that window, whichever end of the filter carries it, which is the
//!   SAME rule the service deployment listing is held to
//!   ([`crate::list_service_deployments`]).
//!
//! Every case drives the emulator through the shared wire harness in [`crate::ecs_wire`],
//! the same front door a client uses, so nothing reaches into the state struct and a page
//! that is not computed from persisted state cannot pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::daemon_scheduling::{
    create_daemon, create_managed_provider, list_deployments_page, register_daemon_definition,
    IMAGE,
};
use crate::ecs_wire::{create_cluster, ok, refusal, service, string};
// The window a deployment-history `createdAt` filter may reach back over and the sentence a
// bound outside it earns are ONE rule of this service, shared with the SERVICE deployment
// listing's suite rather than written again here, so the two suites cannot drift on it.
use crate::list_service_deployments::{
    epoch_seconds, HISTORY_WINDOW, HISTORY_WINDOW_SENTENCE, HOUR,
};
// The sentence a daemon-plane page size outside its range earns is the ONE sentence every
// daemon-plane listing of this service words for it, so it is taken from where
// `crate::page_size_range_refusals` already spells it rather than written again here: the
// two suites cannot then drift on what they claim Amazon ECS says.
use crate::page_size_range_refusals::range_sentence;

/// "If this parameter isn't used, then ListDaemonDeployments returns up to 20 results and a
/// nextToken value if applicable."
const DEFAULT_PAGE_SIZE: usize = 20;

/// The inclusive range the same member documents a requested page size in: "This value can
/// be between 1 and 100."
const MIN_PAGE_SIZE: i32 = 1;
const MAX_PAGE_SIZE: i32 = 100;

/// The sentence a page token that names no position earns.
const INVALID_TOKEN_SENTENCE: &str = "Invalid nextToken.";

/// One daemon of its own `cluster` whose rollout history holds `rollouts` deployments:
/// the one the create started and one per update after it.
///
/// Returns the daemon's ARN with the ARNs of its deployments NEWEST FIRST — the order the
/// listing reports them in, so a page is compared against a prefix of this.
async fn daemon_history(
    router: &MultiServiceRouter,
    cluster: &str,
    rollouts: usize,
    case: &str,
) -> (String, Vec<String>) {
    create_cluster(router, cluster, case).await;
    let provider = create_managed_provider(router, cluster, &format!("mi-{cluster}"), case).await;
    let family = cluster.replace('-', "");
    let definition = register_daemon_definition(router, &family, IMAGE, case).await;
    let (daemon_arn, first) = create_daemon(
        router,
        cluster,
        "agents",
        std::slice::from_ref(&provider),
        &definition,
        case,
    )
    .await;

    let mut arns = vec![first];
    while arns.len() < rollouts {
        let body = ok(
            router,
            "UpdateDaemon",
            json!({
                "daemonArn": daemon_arn,
                "daemonTaskDefinitionArn": definition,
                "capacityProviderArns": [provider],
            }),
            case,
        )
        .await;
        arns.push(string(&body, "deploymentArn", case));
    }
    arns.reverse();
    (daemon_arn, arns)
}

/// The deployments a page reports, by ARN.
fn reported(page: &[Value], case: &str) -> Vec<String> {
    page.iter()
        .map(|deployment| string(deployment, "daemonDeploymentArn", case))
        .collect()
}

#[tokio::test]
async fn a_history_longer_than_the_default_page_is_cut_at_twenty_and_walked_by_its_token() {
    let router = service();
    const CASE: &str = "a history one rollout longer than the default page";
    // One more rollout than the default page holds: a history of twenty or fewer would be
    // reported whole whatever the default were, so only a longer one makes the documented
    // default — and the token that carries the remainder — observable.
    let (daemon_arn, newest_first) =
        daemon_history(&router, "daemon-default-page", DEFAULT_PAGE_SIZE + 1, CASE).await;

    let (page, token) =
        list_deployments_page(&router, json!({ "daemonArn": daemon_arn }), CASE).await;
    assert_eq!(
        reported(&page, CASE),
        newest_first[..DEFAULT_PAGE_SIZE].to_vec(),
        "case {CASE:?}: expected the default page of {DEFAULT_PAGE_SIZE} rollouts, newest \
         first, got {page:?}"
    );
    let token = token.unwrap_or_else(|| {
        panic!("case {CASE:?}: expected a nextToken for the rollouts the default page left")
    });

    let (rest, after) = list_deployments_page(
        &router,
        json!({ "daemonArn": daemon_arn, "nextToken": token }),
        CASE,
    )
    .await;
    assert_eq!(
        reported(&rest, CASE),
        newest_first[DEFAULT_PAGE_SIZE..].to_vec(),
        "case {CASE:?}: expected the token to resume where the default page stopped, got \
         {rest:?}"
    );
    assert!(
        after.is_none(),
        "case {CASE:?}: expected no token once every rollout is reported, got {after:?}"
    );
}

#[tokio::test]
async fn a_deployment_page_size_outside_its_documented_range_states_that_range() {
    let router = service();
    const CASE: &str = "a daemon with three rollouts";
    const ROLLOUTS: usize = 3;
    let (daemon_arn, newest_first) =
        daemon_history(&router, "daemon-page-range", ROLLOUTS, CASE).await;

    // The rows differ along the END of the range crossed and the DISTANCE past it, because
    // the refusal is a property of the range rather than of the value: the sentence is the
    // same at either end and however far past a bound the value is.
    let expected = range_sentence(MIN_PAGE_SIZE, MAX_PAGE_SIZE);
    for (row, max_results) in [
        ("one below the range", MIN_PAGE_SIZE - 1),
        ("far below the range", -100),
        ("at the floor of the member's type", i32::MIN),
        ("one above the range", MAX_PAGE_SIZE + 1),
        ("far above the range", 5_000),
        ("at the ceiling of the member's type", i32::MAX),
    ] {
        let case = format!("a deployment page {row}");
        let (code, message) = refusal(
            &router,
            "ListDaemonDeployments",
            json!({ "daemonArn": daemon_arn, "maxResults": max_results }),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("InvalidParameterException", expected.as_str()),
            "case {case:?}: expected the listing to state its page-size range, got {code} \
             with {message:?}"
        );
    }

    // Each published bound is INSIDE the range, so it is paged with rather than refused:
    // the sentence is earned by crossing a bound, not by naming one. A page never holds
    // more than it asked for, and never fewer than the history has left for it.
    for (row, max_results) in [
        ("the smallest page a request may ask for", MIN_PAGE_SIZE),
        ("a page smaller than the history", 2),
        ("the largest page a request may ask for", MAX_PAGE_SIZE),
    ] {
        let case = format!("{row} over {ROLLOUTS} rollouts");
        let (page, token) = list_deployments_page(
            &router,
            json!({ "daemonArn": daemon_arn, "maxResults": max_results }),
            &case,
        )
        .await;
        let expected_len = usize::try_from(max_results).unwrap_or_default().min(ROLLOUTS);
        assert_eq!(
            reported(&page, &case),
            newest_first[..expected_len].to_vec(),
            "case {case:?}: expected the first {expected_len} rollout(s), newest first, got \
             {page:?}"
        );
        assert_eq!(
            token.is_some(),
            expected_len < ROLLOUTS,
            "case {case:?}: expected a token exactly when rollouts are left, got {token:?}"
        );
    }
}

#[tokio::test]
async fn a_deployment_listing_resumes_only_a_token_it_minted_for_that_daemon() {
    let router = service();
    const CASE: &str = "two daemons, each with its own history";
    const ROLLOUTS: usize = 3;
    let (daemon_arn, newest_first) =
        daemon_history(&router, "daemon-token-walk", ROLLOUTS, CASE).await;
    let (other_arn, _) = daemon_history(&router, "daemon-token-other", ROLLOUTS, CASE).await;

    // The listing's OWN token resumes exactly where its page stopped.
    let (first_page, token) = list_deployments_page(
        &router,
        json!({ "daemonArn": daemon_arn, "maxResults": 1 }),
        CASE,
    )
    .await;
    assert_eq!(
        reported(&first_page, CASE),
        newest_first[..1].to_vec(),
        "case {CASE:?}: expected the newest rollout first, got {first_page:?}"
    );
    let minted = token.unwrap_or_else(|| {
        panic!("case {CASE:?}: expected a nextToken while rollouts are left")
    });
    assert!(
        !minted.contains(&newest_first[0]) && !minted.contains("agents"),
        "case {CASE:?}: expected an opaque token, got {minted:?}"
    );
    let (second_page, _) = list_deployments_page(
        &router,
        json!({ "daemonArn": daemon_arn, "maxResults": 1, "nextToken": minted }),
        CASE,
    )
    .await;
    assert_eq!(
        reported(&second_page, CASE),
        newest_first[1..2].to_vec(),
        "case {CASE:?}: expected the token to resume at the rollout after the page, got \
         {second_page:?}"
    );

    // A token minted for the OTHER daemon's history, so the value is one this store really
    // minted and only its scope differs.
    let (_, other_token) = list_deployments_page(
        &router,
        json!({ "daemonArn": other_arn, "maxResults": 1 }),
        CASE,
    )
    .await;
    let other_token = other_token.unwrap_or_else(|| {
        panic!("case {CASE:?}: expected the other daemon's listing to hand out a token")
    });
    // And one minted by ANOTHER operation of this same store: both clusters of this suite
    // are listed, so a page of one hands out a cluster listing's token.
    let clusters = ok(&router, "ListClusters", json!({ "maxResults": 1 }), CASE).await;
    let cluster_token = clusters["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {CASE:?}: expected a cluster page token, got {clusters}"))
        .to_string();

    // The rows differ along WHERE the value came from: a name or an ARN a caller would
    // assemble, text that is not a token at all, a minted token with a byte dropped, and a
    // token this store minted for another scope. None of them names a position in THIS
    // history.
    for (row, forged) in [
        ("a plain name", "agents".to_string()),
        ("a deployment ARN of this very history", newest_first[1].clone()),
        ("text that is no token", "not-a-token".to_string()),
        ("an empty token", String::new()),
        (
            "this listing's own token with its last byte dropped",
            // The token travels as hexadecimal, two characters a byte, so dropping two
            // characters drops exactly one byte of the value this store minted.
            minted[..minted.len() - 2].to_string(),
        ),
        ("a token minted for another daemon's history", other_token),
        ("a token minted by another operation", cluster_token),
    ] {
        let case = format!("resuming a deployment listing from {row}");
        let (code, message) = refusal(
            &router,
            "ListDaemonDeployments",
            json!({ "daemonArn": daemon_arn, "nextToken": forged }),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("ClientException", INVALID_TOKEN_SENTENCE),
            "case {case:?}: expected a value this listing never minted to name no page, got \
             {code} with {message:?}"
        );
    }

    // The refusals changed nothing: the whole history is still reported, newest first.
    let (whole, token) =
        list_deployments_page(&router, json!({ "daemonArn": daemon_arn }), CASE).await;
    assert_eq!(
        (reported(&whole, CASE), token.clone()),
        (newest_first.clone(), None),
        "case {CASE:?}: expected every rollout of the daemon after the refusals, got \
         {whole:?} with {token:?}"
    );
}

#[tokio::test]
async fn a_created_at_bound_older_than_the_retained_history_is_refused() {
    let router = service();
    const CASE: &str = "one rollout of a daemon";
    let (daemon_arn, newest_first) = daemon_history(&router, "daemon-window", 1, CASE).await;

    let now = epoch_seconds();
    let window = HISTORY_WINDOW.as_secs_f64();

    // The same rule the SERVICE deployment listing is held to, on the listing that filters
    // the daemon-plane history by the same `createdAt` window: each row places a bound either
    // side of the window's own edge, on each end of the filter and on both at once.
    for (row, filter) in [
        (
            "after an hour past the edge of the window",
            json!({ "after": now - window - HOUR }),
        ),
        (
            "before an hour past the edge of the window",
            json!({ "before": now - window - HOUR }),
        ),
        (
            "both bounds past the edge of the window",
            json!({ "after": now - window - 2.0 * HOUR, "before": now - window - HOUR }),
        ),
        (
            "after inside the window with before past its edge",
            json!({ "after": now - HOUR, "before": now - window - HOUR }),
        ),
        ("a bound at the epoch", json!({ "before": 0.0 })),
    ] {
        let case = format!("a daemon deployment listing with {row}");
        let (code, message) = refusal(
            &router,
            "ListDaemonDeployments",
            json!({ "daemonArn": daemon_arn, "createdAt": filter }),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("InvalidParameterException", HISTORY_WINDOW_SENTENCE),
            "case {case:?}: expected the window the filter has to fall inside, got {code} \
             with {message:?}"
        );
    }

    // Inside the window the filter is SERVED, and the rollout the create started is reported.
    for (row, filter) in [
        (
            "after an hour inside the edge of the window",
            json!({ "after": now - window + HOUR }),
        ),
        ("before now", json!({ "before": now })),
        (
            "a window wholly inside the retained one",
            json!({ "after": now - window + HOUR, "before": now + HOUR }),
        ),
    ] {
        let case = format!("a daemon deployment listing with {row}");
        let (page, token) = list_deployments_page(
            &router,
            json!({ "daemonArn": daemon_arn, "createdAt": filter }),
            &case,
        )
        .await;
        assert_eq!(
            (reported(&page, &case), token.clone()),
            (newest_first.clone(), None),
            "case {case:?}: expected the rollout inside the window, got {page:?} with \
             {token:?}"
        );
    }
}
