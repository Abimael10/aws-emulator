// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for two listings of this service: `ListBrowsers` and
//! `ListConfigurationBundles`.
//!
//! The rule under test: a listing reports exactly the resources the account holds, from
//! the same records a read of one of them answers from — so a refused create contributes
//! nothing to it, a create replayed under one `clientToken` contributes ONE entry, and
//! `ListBrowsers`'s `type` filter selects between the browsers this account created and
//! the browser tool the SERVICE provides in every account.
//!
//! `ListConfigurationBundles` is `@paginated`, so the same rule holds one page at a time:
//! a page holds at most the caller's `maxResults` summaries in ascending `bundleId` order
//! and hands back a `nextToken` that resumes strictly after the entry it was issued for,
//! so the pages of any size concatenate to every bundle the account holds, reported
//! exactly once; a `maxResults` outside the modelled range, and a `nextToken` this service
//! never issued, are the modelled 400.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request in this suite signs for.
const REGION: &str = "ap-south-1";

/// The browser tool the SERVICE provides in every account, which `ResourceType`'s
/// `SYSTEM` member names (`smithy_query(query="shape", shape="ResourceType")`).
const SYSTEM_BROWSER_ID: &str = "aws.browser.v1";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "bedrock-agentcore-control.amazonaws.com")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/aws4_request, SignedHeaders=host, Signature=deadbeef"
            ),
        );
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `ListBrowsers`: `POST /browsers`, optionally filtered by `type`.
async fn list_browsers(router: &MultiServiceRouter, kind: Option<&str>) -> (u16, Value) {
    let uri = match kind {
        Some(kind) => format!("/browsers?type={kind}"),
        None => "/browsers".to_string(),
    };
    send(router, "POST", &uri, Some(&json!({}))).await
}

/// `ListConfigurationBundles`: `POST /configuration-bundles`.
async fn list_bundles(router: &MultiServiceRouter) -> (u16, Value) {
    list_bundles_paged(router, None, None).await
}

/// `ListConfigurationBundles` with `maxResults` and `nextToken` on the query string, where
/// the model binds them (`smithy_query(query="operation",
/// operation="ListConfigurationBundles")`).
async fn list_bundles_paged(
    router: &MultiServiceRouter,
    max_results: Option<i64>,
    next_token: Option<&str>,
) -> (u16, Value) {
    let mut query = Vec::new();
    if let Some(max) = max_results {
        query.push(format!("maxResults={max}"));
    }
    if let Some(token) = next_token {
        query.push(format!("nextToken={token}"));
    }
    let uri = if query.is_empty() {
        "/configuration-bundles".to_string()
    } else {
        format!("/configuration-bundles?{}", query.join("&"))
    };
    send(router, "POST", &uri, Some(&json!({}))).await
}

/// The error code `restJson1` puts in `__type` / `x-amzn-errortype`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The ids a browser listing reported, sorted.
fn browser_ids(body: &Value) -> Vec<String> {
    let mut ids: Vec<String> = body["browserSummaries"]
        .as_array()
        .map(|summaries| {
            summaries
                .iter()
                .map(|summary| summary["browserId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// `ListBrowsers` reports the browsers this account created and the one the service
/// provides, and its `type` filter selects between them.
///
/// The browser a refused create would have made is in none of the three listings: a
/// `networkMode` of `VPC` with no `vpcConfig` is refused before any state is written, so
/// the name it asked for appears nowhere.
#[tokio::test]
async fn list_browsers_reports_what_the_account_holds_and_filters_by_type() {
    let router = router();

    // Nothing created yet: only the tool the service provides is there, and the CUSTOM
    // filter selects none of it.
    let (status, listed) = list_browsers(&router, None).await;
    assert_eq!(
        (status, browser_ids(&listed)),
        (200, vec![SYSTEM_BROWSER_ID.to_string()]),
        "an account that created no browser still holds the service's own, got {status} \
         ({listed})"
    );
    let (status, listed) = list_browsers(&router, Some("CUSTOM")).await;
    assert_eq!(
        (status, browser_ids(&listed)),
        (200, Vec::<String>::new()),
        "the CUSTOM filter selects only what this account created, got {status} ({listed})"
    );

    // A create whose network configuration the model refuses leaves nothing behind: "this
    // configuration is required when the network mode is set to VPC".
    let (status, refused) = send(
        &router,
        "PUT",
        "/browsers",
        Some(&json!({
            "name": "VpcWithNoConfig",
            "networkConfiguration": {"networkMode": "VPC"},
        })),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ValidationException"),
        "a VPC browser with no vpcConfig is refused, got {status} ({refused})"
    );

    // Two browsers this account really created, one of them in the VPC mode WITH the
    // configuration that mode requires.
    let mut created = Vec::new();
    for (case, name, network) in [
        ("a public browser", "PublicBrowser", json!({"networkMode": "PUBLIC"})),
        (
            "a VPC browser with the configuration that mode requires",
            "VpcBrowser",
            json!({
                "networkMode": "VPC",
                "vpcConfig": {
                    "subnets": ["subnet-0123456789abcdef0"],
                    "securityGroups": ["sg-0123456789abcdef0"],
                },
            }),
        ),
    ] {
        let (status, answer) = send(
            &router,
            "PUT",
            "/browsers",
            Some(&json!({"name": name, "networkConfiguration": network})),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateBrowser, got {status} ({answer})"
        );
        created.push(
            answer["browserId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: the create reported no browserId"))
                .to_string(),
        );
    }
    created.sort();

    // (case, the filter, the ids expected)
    let mut everything = created.clone();
    everything.push(SYSTEM_BROWSER_ID.to_string());
    everything.sort();
    let rows: [(&str, Option<&str>, Vec<String>); 3] = [
        ("no filter, so every type", None, everything),
        ("only what this account created", Some("CUSTOM"), created),
        (
            "only what the service provides",
            Some("SYSTEM"),
            vec![SYSTEM_BROWSER_ID.to_string()],
        ),
    ];

    for (case, filter, expected) in rows {
        let (status, listed) = list_browsers(&router, filter).await;
        assert_eq!(
            (status, browser_ids(&listed)),
            (200, expected),
            "case {case:?}: unexpected listing, got {status} ({listed})"
        );
        assert!(
            !listed.to_string().contains("VpcWithNoConfig"),
            "case {case:?}: the refused create's name appears in the listing, in {listed}"
        );
    }

    // A listing reports the same records a read of one browser answers from.
    let (status, listed) = list_browsers(&router, Some("CUSTOM")).await;
    assert_eq!(status, 200, "the CUSTOM listing is served, got {status}");
    for summary in listed["browserSummaries"].as_array().unwrap_or(&Vec::new()) {
        let browser_id = summary["browserId"].as_str().unwrap_or_default();
        let (status, read) = send(
            &router,
            "GET",
            &format!("/browsers/{browser_id}"),
            None,
        )
        .await;
        assert_eq!(
            (
                status,
                read["name"].clone(),
                read["status"].clone(),
                read["browserArn"].clone()
            ),
            (
                200,
                summary["name"].clone(),
                summary["status"].clone(),
                summary["browserArn"].clone()
            ),
            "the listing and the read report one browser alike, got {status} ({read}) against \
             {summary}"
        );
    }
}

/// `CreateConfigurationBundle` is idempotent over `clientToken`: concurrent creates that
/// share one token and one request all resolve to the SAME bundle, and the account holds
/// exactly one bundle for them.
///
/// The rows differ along how many times the create is replayed under the one token.
#[tokio::test]
async fn a_bundle_create_replayed_under_one_token_holds_one_bundle() {
    let router = router();

    // (case, bundle name, the token, how many times the create is sent)
    let rows: [(&str, &str, &str, usize); 3] = [
        ("one create", "BundleOnce", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1),
        ("the create replayed twice", "BundleTwice", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 2),
        (
            "the create replayed three times, as a racing client retries it",
            "BundleThrice",
            "cccccccccccccccccccccccccccccccccc",
            3,
        ),
    ];

    for (case, bundle_name, token, attempts) in rows {
        let body = json!({
            "bundleName": bundle_name,
            "clientToken": token,
            "components": {"runtime": {"configuration": {"model": "a-model"}}},
        });
        let mut answers = Vec::new();
        for _ in 0..attempts {
            let (status, created) = send(
                &router,
                "POST",
                "/configuration-bundles/create",
                Some(&body),
            )
            .await;
            assert_eq!(
                status, 201,
                "case {case:?}: every replayed create succeeds, got {status} ({created})"
            );
            answers.push(created);
        }

        // Every answer names one bundle and one version of it.
        for (attempt, answer) in answers.iter().enumerate() {
            assert_eq!(
                (
                    answer["bundleId"].clone(),
                    answer["versionId"].clone(),
                    answer["bundleArn"].clone()
                ),
                (
                    answers[0]["bundleId"].clone(),
                    answers[0]["versionId"].clone(),
                    answers[0]["bundleArn"].clone()
                ),
                "case {case:?}: attempt {attempt} resolved to another bundle, got {answer}"
            );
        }

        // And the account holds exactly one bundle under that name.
        let (status, listed) = list_bundles(&router).await;
        let matching: Vec<&Value> = listed["bundles"]
            .as_array()
            .map(|bundles| {
                bundles
                    .iter()
                    .filter(|bundle| bundle["bundleName"] == json!(bundle_name))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            (status, matching.len()),
            (200, 1),
            "case {case:?}: expected exactly one bundle named {bundle_name}, got {status} \
             ({listed})"
        );
        assert_eq!(
            matching[0]["bundleId"],
            answers[0]["bundleId"],
            "case {case:?}: the listing names the bundle the create answered with, got {listed}"
        );
    }

    // The three bundles above are the three the account holds.
    let (status, listed) = list_bundles(&router).await;
    let mut names: Vec<String> = listed["bundles"]
        .as_array()
        .map(|bundles| {
            bundles
                .iter()
                .map(|bundle| bundle["bundleName"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    assert_eq!(
        (status, names),
        (
            200,
            vec![
                "BundleOnce".to_string(),
                "BundleThrice".to_string(),
                "BundleTwice".to_string()
            ]
        ),
        "the listing reports every bundle once, got {status} ({listed})"
    );
}

/// The modelled bounds of `ListConfigurationBundles`' `maxResults` (range 1..100, from
/// `smithy_query(query="operation", operation="ListConfigurationBundles")`). The model
/// states no other ceiling and no default for it, so a caller naming none gets a page of
/// that modelled maximum.
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 100;

/// How many bundles the paging tests seed: more than every page size they ask for below
/// the modelled maximum, so each of those rows has something left over to resume from.
const PAGED_BUNDLES: usize = 7;

/// `CreateConfigurationBundle`: `POST /configuration-bundles/create`, answering HTTP 201.
/// Hands back the `bundleId` the create minted.
///
/// `bundleName` is `^[a-zA-Z][a-zA-Z0-9_]{0,99}$` and `description` is 1..500 characters
/// (`smithy_query(query="operation", operation="CreateConfigurationBundle")`), so every
/// value built here is inside both.
async fn create_bundle(
    router: &MultiServiceRouter,
    name: &str,
    description: Option<&str>,
) -> String {
    let mut body = json!({
        "bundleName": name,
        "components": {"runtime": {"configuration": {"model": "a-model"}}},
    });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    let (status, created) =
        send(router, "POST", "/configuration-bundles/create", Some(&body)).await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected HTTP 201 from CreateConfigurationBundle, got {status} \
         ({created})"
    );
    created["bundleId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: the create reported no bundleId ({created})"))
        .to_string()
}

/// The `bundleId`s a listing reported, in the order it reported them.
fn listed_bundle_ids(body: &Value) -> Vec<String> {
    body["bundles"]
        .as_array()
        .map(|bundles| {
            bundles
                .iter()
                .map(|bundle| bundle["bundleId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Create `count` bundles under `prefix` and hand back every `bundleId` the account then
/// holds, in the ascending order a listing reports them.
async fn seed_bundles(router: &MultiServiceRouter, prefix: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 1..=count {
        ids.push(create_bundle(router, &format!("{prefix}{index:02}"), None).await);
    }
    ids.sort();
    ids
}

/// Walk the bundle listing from its first page to its last, following the `nextToken` each
/// page hands back, and report the ids in the order the pages reported them together with
/// how many pages the walk took. `held` is how many bundles the account holds, which
/// bounds how many pages even the smallest page size can take.
async fn walk_bundles(
    router: &MultiServiceRouter,
    case: &str,
    max_results: Option<i64>,
    held: usize,
) -> (Vec<String>, usize) {
    let allowed = usize::try_from(max_results.unwrap_or(MAX_RESULTS_MAX)).unwrap_or(1);
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let (status, page) = list_bundles_paged(router, max_results, token.as_deref()).await;
        assert_eq!(
            status, 200,
            "case {case:?} (page {pages}): expected HTTP 200 from ListConfigurationBundles, got \
             {status} ({page})"
        );
        let ids = listed_bundle_ids(&page);
        assert!(
            ids.len() <= allowed,
            "case {case:?} (page {pages}): a page holds at most {allowed} summaries, got {} \
             ({page})",
            ids.len()
        );
        walked.extend(ids);
        pages += 1;
        // The smallest page the model admits is one entry, so even that walk cannot take
        // more pages than the account holds bundles, plus the empty page a cursor sitting
        // exactly on the last of them would answer.
        assert!(
            pages <= held + 2,
            "case {case:?}: the walk must terminate, got {pages} pages and {walked:?}"
        );
        match page["nextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    (walked, pages)
}

/// For every page size the modelled range admits, the pages of `ListConfigurationBundles`
/// concatenate to the whole listing: each bundle once, in ascending `bundleId` order, with
/// no page holding more than `maxResults` summaries.
///
/// The rows differ along the page size, which is what paging generalises over: one entry
/// per page, a size that leaves a part-full last page, the size that ends exactly on the
/// last bundle, one past the number held, each modelled bound, and no `maxResults` at all.
#[tokio::test]
async fn bundle_pages_of_every_size_concatenate_to_the_whole_listing() {
    let router = router();
    let held = seed_bundles(&router, "PagedBundle", PAGED_BUNDLES).await;
    let count = i64::try_from(held.len()).unwrap_or(i64::MAX);

    // (case, maxResults)
    let rows: [(&str, Option<i64>); 7] = [
        (
            "the modelled minimum, one bundle per page",
            Some(MAX_RESULTS_MIN),
        ),
        ("two bundles per page", Some(2)),
        ("a size leaving a part-full last page", Some(3)),
        ("exactly as many as the account holds", Some(count)),
        ("one more than the account holds", Some(count + 1)),
        ("the modelled maximum", Some(MAX_RESULTS_MAX)),
        ("no maxResults at all", None),
    ];

    for (case, max_results) in rows {
        let (walked, pages) = walk_bundles(&router, case, max_results, held.len()).await;
        assert_eq!(
            walked, held,
            "case {case:?}: the pages must concatenate to the whole listing {held:?}, got \
             {walked:?}"
        );
        // A page is as full as the caller asked for until the bundles run out, so the walk
        // takes exactly as many pages as that page size divides the account into.
        let page_size = usize::try_from(max_results.unwrap_or(MAX_RESULTS_MAX)).unwrap_or(1);
        let expected_pages = held.len().div_ceil(page_size).max(1);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} pages for a page size of {page_size}, got \
             {pages}"
        );
    }
}

/// A bundle listing reports every bundle from the same record a read of that bundle
/// answers from: one summary per bundle, carrying the identity, the description of the
/// bundle's latest commit and the instant `GetConfigurationBundle` reports.
///
/// The rows differ along the description a bundle carries, which the summary declares as
/// an optional member: none at all, a short one, and a long one.
#[tokio::test]
async fn a_bundle_listing_reports_the_same_record_a_read_answers_from() {
    let router = router();

    // (case, bundleName, the description the create carries)
    let rows: [(&str, &str, Option<&str>); 3] = [
        ("a bundle carrying no description", "PlainBundle", None),
        ("a bundle described in one word", "ShortBundle", Some("one")),
        (
            "a bundle with a sentence for a description",
            "WordyBundle",
            Some("the configuration this account's agents share, as a commit carries it"),
        ),
    ];

    let mut created = Vec::new();
    for (case, name, description) in rows {
        created.push((case, create_bundle(&router, name, description).await, name, description));
    }

    let (status, listed) = list_bundles_paged(&router, Some(MAX_RESULTS_MAX), None).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListConfigurationBundles, got {status} ({listed})"
    );
    assert_eq!(
        listed_bundle_ids(&listed).len(),
        created.len(),
        "the listing reports exactly the {} bundles this account created, got {listed}",
        created.len()
    );

    for (case, bundle_id, name, description) in &created {
        let summary = listed["bundles"]
            .as_array()
            .and_then(|bundles| {
                bundles
                    .iter()
                    .find(|bundle| bundle["bundleId"] == json!(bundle_id))
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: the listing reports no bundle {bundle_id}, got {listed}")
            });
        let expected_description = description.map_or(Value::Null, |text| json!(text));
        assert_eq!(
            (summary["bundleName"].clone(), summary["description"].clone()),
            (json!(name), expected_description),
            "case {case:?}: unexpected summary, got {summary}"
        );

        let (status, read) = send(
            &router,
            "GET",
            &format!("/configuration-bundles/{bundle_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetConfigurationBundle, got {status} ({read})"
        );
        assert_eq!(
            (
                read["bundleId"].clone(),
                read["bundleName"].clone(),
                read["bundleArn"].clone(),
                read["description"].clone(),
                read["createdAt"].clone()
            ),
            (
                summary["bundleId"].clone(),
                summary["bundleName"].clone(),
                summary["bundleArn"].clone(),
                summary["description"].clone(),
                summary["createdAt"].clone()
            ),
            "case {case:?}: the listing and the read report one bundle alike, got {read} against \
             {summary}"
        );
    }
}

/// `maxResults` outside its modelled range (1..100) is refused with the modelled 400, each
/// modelled bound itself is served, and a `nextToken` this service did not issue is the
/// modelled 400 naming that member — while one its own codec could have issued, resuming
/// past every bundle held, is an empty page rather than a refusal.
#[tokio::test]
async fn a_bundle_listing_refuses_a_page_outside_the_modelled_range() {
    let router = router();
    let held = seed_bundles(&router, "BoundedBundle", 3).await;

    // (case, maxResults, expected status, how many summaries a served page holds)
    let rows: [(&str, i64, u16, usize); 4] = [
        ("one below the modelled minimum", MAX_RESULTS_MIN - 1, 400, 0),
        ("the modelled minimum", MAX_RESULTS_MIN, 200, 1),
        ("the modelled maximum", MAX_RESULTS_MAX, 200, held.len()),
        ("one above the modelled maximum", MAX_RESULTS_MAX + 1, 400, 0),
    ];

    for (case, max_results, expected_status, expected_len) in rows {
        let (status, answered) = list_bundles_paged(&router, Some(max_results), None).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status} for maxResults={max_results}, got \
             {status} ({answered})"
        );
        if expected_status == 200 {
            assert_eq!(
                listed_bundle_ids(&answered),
                held[..expected_len].to_vec(),
                "case {case:?}: expected the first {expected_len} bundles held, got {answered}"
            );
        } else {
            assert_eq!(
                error_code(&answered),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {answered}"
            );
        }
    }

    for (case, token) in [
        ("a word outside the token's alphabet", "notatoken"),
        ("a pair of characters outside it", "zz"),
        ("a token truncated to an odd length", "616"),
        ("a bundleId handed over unencoded", held[0].as_str()),
    ] {
        let (status, answered) = list_bundles_paged(&router, None, Some(token)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for nextToken={token:?}, got {status} ({answered})"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {answered}"
        );
        assert_eq!(
            answered["fieldList"][0]["name"],
            json!("nextToken"),
            "case {case:?}: expected the refusal to name nextToken, got {}",
            answered["fieldList"]
        );
    }

    // `7a7a7a7a` is the token form this service issues (the hex of the store key a page
    // ended on) for the key `zzzz`, which sorts after every bundleId the account holds — so
    // it resumes past the last of them.
    let (status, beyond) = list_bundles_paged(&router, None, Some("7a7a7a7a")).await;
    assert_eq!(
        (status, listed_bundle_ids(&beyond)),
        (200, Vec::<String>::new()),
        "case \"a token that resumes past every bundle\": expected the required bundles member \
         present and empty, got {beyond}"
    );
    assert!(
        beyond["nextToken"].is_null(),
        "case \"a token that resumes past every bundle\": nothing is left over, got {}",
        beyond["nextToken"]
    );
}
