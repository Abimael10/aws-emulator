// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Amazon Inspector account-activation and
//! suppression-filter slice: `Enable`, `BatchGetAccountStatus`, `CreateFilter`,
//! `ListFilters`, `UpdateFilter` and `DeleteFilter`, plus the per-account SCAN
//! CONFIGURATION that activation carries — `UpdateConfiguration` and `GetConfiguration`.
//!
//! Every request goes through the real `MultiServiceRouter` — routing, restJson1
//! deserialization, modelled-constraint validation, the handler, and restJson1
//! serialization — so what is asserted here is the wire behaviour a client sees.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_inspector2_inspector2::state::{
    CONFIGURATION_PENDING, CONFIGURATION_SUCCESS, CONFIGURATION_TRANSITION,
    DEFAULT_EC2_SCAN_MODE, DEFAULT_ECR_RESCAN_DURATION, INHERIT_FROM_ADMIN,
};
use smithy_server_runtime::MultiServiceRouter;

/// The single account id every generated emulator presents
/// (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// Every `ResourceScanType` token, in model order.
/// Source: `smithy_query(query="shape", shape="ResourceScanType")`.
const RESOURCE_SCAN_TYPES: [&str; 5] = ["EC2", "ECR", "LAMBDA", "LAMBDA_CODE", "CODE_REPOSITORY"];

/// The `ResourceStatus`/`ResourceState` member each `ResourceScanType` is reported under.
/// Source: `smithy_query(query="shape", shape="ResourceStatus,ResourceState")`.
fn resource_member(resource_type: &str) -> &'static str {
    match resource_type {
        "EC2" => "ec2",
        "ECR" => "ecr",
        "LAMBDA" => "lambda",
        "LAMBDA_CODE" => "lambdaCode",
        "CODE_REPOSITORY" => "codeRepository",
        other => panic!("unmodelled ResourceScanType {other:?}"),
    }
}

/// A router holding only this service, over its own engine, so each test starts from
/// empty state.
pub(crate) fn router() -> MultiServiceRouter {
    router_over(&Arc::new(engine::InMemoryEngine::new()))
}

/// A router holding only this service, over `engine` — for a test that also reaches the
/// store the handlers read.
pub(crate) fn router_over(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_inspector2_inspector2::EmulatedService::new(Arc::clone(engine)),
    );
    router
}

/// Send a restJson1 request with `method`, signed for `region` so the service claims it
/// and can read the region out of the SigV4 credential scope. `body` is `None` for a
/// request with no payload, such as a GET whose members are bound to the query string.
pub(crate) async fn send_in(
    router: &MultiServiceRouter,
    method: &str,
    region: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Option<String>, Value) {
    let payload = body.map_or_else(bytes::Bytes::new, |body| {
        bytes::Bytes::from(serde_json::to_vec(body).expect("a serializable request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://localhost{uri}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=TESTKEY/20240101/{region}/inspector2/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, error_type, parsed)
}

/// POST a restJson1 request, signed for `region` so the service claims it and can read
/// the region out of the SigV4 credential scope.
pub(crate) async fn post_in(
    router: &MultiServiceRouter,
    region: &str,
    uri: &str,
    body: &Value,
) -> (u16, Option<String>, Value) {
    send_in(router, "POST", region, uri, Some(body)).await
}

async fn post(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let (status, _, parsed) = post_in(router, "us-east-1", uri, body).await;
    (status, parsed)
}

/// `Enable` records exactly the resource scan types the request named, and every other
/// modelled type stays off: the same table is then read back through
/// `BatchGetAccountStatus`, which must agree with what `Enable` stored.
#[tokio::test]
async fn enable_records_exactly_the_resource_types_requested() {
    // Rows differ along WHICH and HOW MANY scan types are named, including the
    // modelled list bounds (1 and all 5) and a type the issue never cited.
    let cases: [&[&str]; 6] = [
        &["EC2"],
        &["ECR"],
        &["CODE_REPOSITORY"],
        &["EC2", "ECR"],
        &["LAMBDA", "LAMBDA_CODE"],
        &RESOURCE_SCAN_TYPES,
    ];

    for requested in cases {
        let router = router();

        // Before Enable, the account reports everything off.
        let (status, before) = post(&router, "/status/batch/get", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {requested:?}: BatchGetAccountStatus before Enable: expected 200, got \
             {status} / {before}"
        );
        assert_eq!(
            before["accounts"][0]["state"]["status"], "DISABLED",
            "case {requested:?}: a never-enabled account must report DISABLED, got {before}"
        );

        let (status, enabled) =
            post(&router, "/enable", &json!({ "resourceTypes": requested })).await;
        assert_eq!(
            status, 200,
            "case {requested:?}: Enable: expected 200, got {status} / {enabled}"
        );
        assert_eq!(
            enabled["accounts"][0]["accountId"], ACCOUNT,
            "case {requested:?}: Enable must default to the calling account, got {enabled}"
        );
        assert_eq!(
            enabled["accounts"][0]["status"], "ENABLED",
            "case {requested:?}: Enable must report the account ENABLED, got {enabled}"
        );

        let (status, after) = post(&router, "/status/batch/get", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {requested:?}: BatchGetAccountStatus after Enable: expected 200, got \
             {status} / {after}"
        );

        for resource_type in RESOURCE_SCAN_TYPES {
            let expected = if requested.contains(&resource_type) {
                "ENABLED"
            } else {
                "DISABLED"
            };
            let member = resource_member(resource_type);
            assert_eq!(
                enabled["accounts"][0]["resourceStatus"][member], expected,
                "case {requested:?}: Enable's resourceStatus.{member}: expected {expected}, \
                 got {enabled}"
            );
            assert_eq!(
                after["accounts"][0]["resourceState"][member]["status"], expected,
                "case {requested:?}: BatchGetAccountStatus's resourceState.{member}: expected \
                 {expected}, got {after}"
            );
        }
    }
}

/// `Enable` acts on the accounts the request names, and on the calling account only when
/// it names none. `BatchGetAccountStatus` reports each of those accounts from its own
/// record; an account the caller does not administer is reported per entry instead, which
/// `account_status_access` pins.
#[tokio::test]
async fn enable_and_status_are_per_account() {
    let other = "012345678901";
    let third = "109876543210";
    let router = router();

    let (status, enabled) = post(
        &router,
        "/enable",
        &json!({ "resourceTypes": ["EC2"], "accountIds": [ACCOUNT, other] }),
    )
    .await;
    assert_eq!(status, 200, "Enable: expected 200, got {status} / {enabled}");
    let ids: Vec<&str> = enabled["accounts"]
        .as_array()
        .expect("accounts is a list")
        .iter()
        .map(|account| account["accountId"].as_str().expect("an accountId"))
        .collect();
    assert_eq!(
        ids,
        vec![ACCOUNT, other],
        "Enable must report every requested account, got {enabled}"
    );

    let (status, reported) = post(
        &router,
        "/status/batch/get",
        &json!({ "accountIds": [ACCOUNT, other, third] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchGetAccountStatus: expected 200, got {status} / {reported}"
    );
    // `third` was never recorded, so it is not the caller's to read: it is reported per
    // entry instead of with a status of its own.
    assert_eq!(
        reported["failedAccounts"][0]["accountId"], third,
        "an account the caller does not administer belongs in failedAccounts, got {reported}"
    );
    for (index, (account, expected)) in [(ACCOUNT, "ENABLED"), (other, "ENABLED")]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            reported["accounts"][index]["accountId"], account,
            "case {account}: reported out of request order: {reported}"
        );
        assert_eq!(
            reported["accounts"][index]["state"]["status"], expected,
            "case {account}: expected {expected}, got {reported}"
        );
    }
}

/// A repeated `@idempotencyToken` makes `Enable` a replay: the second call reports the
/// enablement the first one stored rather than widening it.
#[tokio::test]
async fn enable_is_idempotent_for_a_repeated_client_token() {
    let router = router();
    let first = json!({ "resourceTypes": ["EC2"], "clientToken": "token-1" });
    let (status, _) = post(&router, "/enable", &first).await;
    assert_eq!(status, 200, "first Enable: expected 200, got {status}");

    // Same token, more resource types: a replay, so nothing changes.
    let replay = json!({ "resourceTypes": ["EC2", "ECR"], "clientToken": "token-1" });
    let (status, replayed) = post(&router, "/enable", &replay).await;
    assert_eq!(
        status, 200,
        "replayed Enable: expected 200, got {status} / {replayed}"
    );
    assert_eq!(
        replayed["accounts"][0]["resourceStatus"]["ecr"], "DISABLED",
        "a replayed clientToken must not widen the stored enablement, got {replayed}"
    );

    // A different token is a new request and does widen it.
    let fresh = json!({ "resourceTypes": ["EC2", "ECR"], "clientToken": "token-2" });
    let (status, widened) = post(&router, "/enable", &fresh).await;
    assert_eq!(
        status, 200,
        "second Enable: expected 200, got {status} / {widened}"
    );
    assert_eq!(
        widened["accounts"][0]["resourceStatus"]["ecr"], "ENABLED",
        "a new clientToken must apply the request, got {widened}"
    );
}

/// `CreateFilter` stores exactly what it was sent and `ListFilters` renders it back: the
/// rows differ along action, criteria shape, and which optional members are present.
#[tokio::test]
async fn created_filters_round_trip_through_list_filters() {
    let cases: [(&str, Value); 4] = [
        (
            "suppress-lows",
            json!({
                "action": "SUPPRESS",
                "name": "suppress-lows",
                "filterCriteria": { "severity": [{ "comparison": "EQUALS", "value": "LOW" }] },
                "description": "tune out the noise",
                "reason": "accepted risk",
                "tags": { "team": "appsec" }
            }),
        ),
        (
            "no-op-filter",
            json!({
                "action": "NONE",
                "name": "no-op-filter",
                "filterCriteria": { "awsAccountId": [{ "comparison": "EQUALS", "value": ACCOUNT }] }
            }),
        ),
        (
            "multi-criteria",
            json!({
                "action": "SUPPRESS",
                "name": "multi-criteria",
                "filterCriteria": {
                    "resourceType": [{ "comparison": "NOT_EQUALS", "value": "AWS_EC2_INSTANCE" }],
                    "ecrImageTags": [
                        { "comparison": "EQUALS", "value": "latest" },
                        { "comparison": "PREFIX", "value": "release-" }
                    ],
                    "inspectorScore": [{ "lowerInclusive": 1.0, "upperInclusive": 7.5 }]
                },
                "tags": { "team": "appsec", "env": "prod", "owner": "platform" }
            }),
        ),
        (
            "x",
            json!({
                "action": "NONE",
                "name": "x",
                "filterCriteria": { "title": [{ "comparison": "PREFIX", "value": "CVE-" }] }
            }),
        ),
    ];

    for (name, request) in cases {
        let router = router();
        let (status, _, created) =
            post_in(&router, "eu-central-1", "/filters/create", &request).await;
        assert_eq!(
            status, 200,
            "case {name}: CreateFilter: expected 200, got {status} / {created}"
        );
        let arn = created["arn"].as_str().unwrap_or_default().to_string();
        let expected_prefix =
            format!("arn:aws:inspector2:eu-central-1:{ACCOUNT}:owner/{ACCOUNT}/filter/");
        assert!(
            arn.starts_with(&expected_prefix),
            "case {name}: expected an ARN starting {expected_prefix}, got {arn:?}"
        );
        assert!(
            !arn.is_empty() && arn.len() <= 128,
            "case {name}: FilterArn is modelled length 1..128, got {} chars: {arn:?}",
            arn.len()
        );

        let (status, listed) = post(&router, "/filters/list", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {name}: ListFilters: expected 200, got {status} / {listed}"
        );
        let filters = listed["filters"].as_array().expect("filters is a list");
        assert_eq!(
            filters.len(),
            1,
            "case {name}: expected exactly the one created filter, got {listed}"
        );
        let filter = &filters[0];
        assert_eq!(filter["arn"], arn, "case {name}: ARN differs: {filter}");
        assert_eq!(
            filter["ownerId"], ACCOUNT,
            "case {name}: ownerId differs: {filter}"
        );
        assert_eq!(filter["name"], request["name"], "case {name}: name differs: {filter}");
        assert_eq!(
            filter["action"], request["action"],
            "case {name}: action differs: {filter}"
        );
        assert_eq!(
            filter["criteria"], request["filterCriteria"],
            "case {name}: criteria must survive the round trip: {filter}"
        );
        for member in ["description", "reason"] {
            let sent = request.get(member).cloned().unwrap_or(Value::Null);
            let got = filter.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                got, sent,
                "case {name}: {member} must be stored as sent (sent {sent}, got {got})"
            );
        }
        // `tags` is always reported, so a filter created without tags reports an empty
        // map rather than leaving the member out (see `filter_tags_are_always_reported`).
        let sent_tags = request.get("tags").cloned().unwrap_or_else(|| json!({}));
        let got_tags = filter.get("tags").cloned().unwrap_or(Value::Null);
        assert_eq!(
            got_tags, sent_tags,
            "case {name}: tags must be reported as stored (sent {sent_tags}, got {got_tags})"
        );
        assert!(
            filter["createdAt"].is_number() && filter["updatedAt"].is_number(),
            "case {name}: createdAt/updatedAt must be timestamps, got {filter}"
        );
        assert_eq!(
            filter["createdAt"], filter["updatedAt"],
            "case {name}: a never-updated filter reports its creation instant as its last \
             update, got {filter}"
        );
    }
}

/// The `arns` and `action` selectors are applied to the stored filters.
#[tokio::test]
async fn list_filters_honours_the_arns_and_action_selectors() {
    let router = router();
    let mut arns = Vec::new();
    for (name, action) in [
        ("one", "SUPPRESS"),
        ("two", "NONE"),
        ("three", "SUPPRESS"),
        ("four", "NONE"),
    ] {
        let (status, created) = post(
            &router,
            "/filters/create",
            &json!({
                "action": action,
                "name": name,
                "filterCriteria": { "title": [{ "comparison": "EQUALS", "value": name }] }
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "creating {name}: expected 200, got {status} / {created}"
        );
        arns.push(created["arn"].as_str().expect("an arn").to_string());
    }

    let names = |listed: &Value| -> Vec<String> {
        listed["filters"]
            .as_array()
            .expect("filters is a list")
            .iter()
            .map(|filter| filter["name"].as_str().unwrap_or_default().to_string())
            .collect()
    };

    let cases: [(&str, Value, Vec<&str>); 5] = [
        ("no selector", json!({}), vec!["one", "two", "three", "four"]),
        (
            "action SUPPRESS",
            json!({ "action": "SUPPRESS" }),
            vec!["one", "three"],
        ),
        ("action NONE", json!({ "action": "NONE" }), vec!["two", "four"]),
        (
            "one arn",
            json!({ "arns": [arns[2]] }),
            vec!["three"],
        ),
        (
            "two arns and an action",
            json!({ "arns": [arns[1], arns[3]], "action": "NONE" }),
            vec!["two", "four"],
        ),
    ];

    for (case, request, expected) in cases {
        let (status, listed) = post(&router, "/filters/list", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: ListFilters: expected 200, got {status} / {listed}"
        );
        let mut got = names(&listed);
        got.sort();
        let mut want: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        want.sort();
        assert_eq!(
            got, want,
            "case {case}: expected filters {want:?}, got {got:?} ({listed})"
        );
    }
}

/// `maxResults` truncates the page and the returned `nextToken` fetches exactly the
/// remainder, with no filter seen twice or skipped. Rows cover both published bounds of
/// the modelled 1..100 range and a page size larger than the result set.
#[tokio::test]
async fn list_filters_pages_at_max_results() {
    for (total, page_size) in [(1usize, 1i64), (5, 1), (5, 2), (5, 4), (5, 5), (3, 100)] {
        let router = router();
        let mut created = Vec::new();
        for index in 0..total {
            let name = format!("filter-{index}");
            let (status, response) = post(
                &router,
                "/filters/create",
                &json!({
                    "action": "SUPPRESS",
                    "name": name,
                    "filterCriteria": { "title": [{ "comparison": "EQUALS", "value": name }] }
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case total={total} maxResults={page_size}: CreateFilter: got {status} / {response}"
            );
            created.push(response["arn"].as_str().expect("an arn").to_string());
        }

        let mut seen = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages = 0;
        loop {
            let request = match &next_token {
                Some(token) => json!({ "maxResults": page_size, "nextToken": token }),
                None => json!({ "maxResults": page_size }),
            };
            let (status, listed) = post(&router, "/filters/list", &request).await;
            assert_eq!(
                status, 200,
                "case total={total} maxResults={page_size}: ListFilters: got {status} / {listed}"
            );
            let page = listed["filters"].as_array().expect("filters is a list");
            assert!(
                page.len() as i64 <= page_size,
                "case total={total} maxResults={page_size}: a page returned {} filters",
                page.len()
            );
            for filter in page {
                seen.push(filter["arn"].as_str().expect("an arn").to_string());
            }
            pages += 1;
            assert!(
                pages <= total + 1,
                "case total={total} maxResults={page_size}: pagination did not terminate \
                 after {pages} pages"
            );
            next_token = listed["nextToken"].as_str().map(str::to_string);
            if next_token.is_none() {
                break;
            }
        }

        assert_eq!(
            seen, created,
            "case total={total} maxResults={page_size}: paging must yield every stored \
             filter exactly once, in the stored order"
        );
    }
}

/// `maxResults` outside the modelled 1..100 range is refused at the boundary, before any
/// filter is read.
#[tokio::test]
async fn list_filters_refuses_a_max_results_outside_the_modelled_range() {
    let router = router();
    for max_results in [0, 101, -1] {
        let (status, error_type, body) = post_in(
            &router,
            "us-east-1",
            "/filters/list",
            &json!({ "maxResults": max_results }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case maxResults={max_results}: expected 400, got {status} / {body}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case maxResults={max_results}: expected ValidationException, got {error_type:?}"
        );
    }
}

/// A `nextToken` this service never minted names no position, so it is refused rather
/// than silently answered from the start of the list.
#[tokio::test]
async fn list_filters_refuses_an_unparseable_next_token() {
    let router = router();
    // Rows: non-hex, odd-length hex, and WELL-FORMED hex that decodes to something this
    // service never minted (hex of "AA" and of a filter ARN of another service).
    let tokens = [
        "not-a-token",
        "zz",
        "0",
        "4141",
        "61726e3a6177733a73333a3a3a6d792d6275636b6574",
    ];
    for token in tokens {
        let (status, error_type, body) = post_in(
            &router,
            "us-east-1",
            "/filters/list",
            &json!({ "nextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {token:?}: expected 400, got {status} / {body}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {token:?}: expected ValidationException, got {error_type:?}"
        );
    }
}

/// `AccountIdSet` is `@uniqueItems`, so a repeated account id is refused at the boundary —
/// no operation ever sees one, and so none can report an account twice.
#[tokio::test]
async fn a_repeated_account_id_is_refused() {
    let other = "012345678901";
    let cases: [(&str, Vec<&str>); 3] = [
        ("one id twice", vec![ACCOUNT, ACCOUNT]),
        ("two ids, one repeated", vec![ACCOUNT, other, ACCOUNT]),
        ("one id four times", vec![other, other, other, other]),
    ];

    for (case, requested) in cases {
        let router = router();
        for (operation, uri) in [("Enable", "/enable"), ("BatchGetAccountStatus", "/status/batch/get")]
        {
            let body = if uri == "/enable" {
                json!({ "resourceTypes": ["EC2"], "accountIds": requested })
            } else {
                json!({ "accountIds": requested })
            };
            let (status, error_type, response) = post_in(&router, "us-east-1", uri, &body).await;
            assert_eq!(
                status, 400,
                "case {case}: {operation}: expected 400, got {status} / {response}"
            );
            assert_eq!(
                error_type.as_deref(),
                Some("ValidationException"),
                "case {case}: {operation}: expected ValidationException, got {error_type:?}"
            );
        }
    }

    // And a list of DISTINCT ids is accepted, so the refusal is about the repeat.
    let router = router();
    let (status, enabled) = post(
        &router,
        "/enable",
        &json!({ "resourceTypes": ["EC2"], "accountIds": [ACCOUNT, other] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "distinct ids must be accepted, got {status} / {enabled}"
    );
}

/// `UpdateFilter` applies only the members the request supplied, leaves the rest alone,
/// and refreshes `updatedAt`.
#[tokio::test]
async fn update_filter_mutates_only_the_supplied_members() {
    let original = json!({
        "action": "SUPPRESS",
        "name": "original-name",
        "filterCriteria": { "severity": [{ "comparison": "EQUALS", "value": "LOW" }] },
        "description": "original description",
        "reason": "original reason",
        "tags": { "team": "appsec" }
    });

    let cases: [(&str, Value); 5] = [
        ("description only", json!({ "description": "tuned" })),
        ("name only", json!({ "name": "renamed" })),
        ("action only", json!({ "action": "NONE" })),
        (
            "criteria only",
            json!({
                "filterCriteria": {
                    "severity": [{ "comparison": "EQUALS", "value": "CRITICAL" }]
                }
            }),
        ),
        (
            "several at once",
            json!({ "name": "renamed", "reason": "reviewed", "action": "NONE" }),
        ),
    ];

    for (case, update) in cases {
        let router = router();
        let (status, created) = post(&router, "/filters/create", &original).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateFilter: expected 200, got {status} / {created}"
        );
        let arn = created["arn"].as_str().expect("an arn").to_string();

        let (status, before) = post(&router, "/filters/list", &json!({})).await;
        assert_eq!(status, 200, "case {case}: ListFilters: got {status} / {before}");
        let before = before["filters"][0].clone();

        // A measurable gap, so a refreshed `updatedAt` is strictly later than the
        // creation instant rather than merely not earlier.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let mut request = update.clone();
        request["filterArn"] = Value::String(arn.clone());
        let (status, updated) = post(&router, "/filters/update", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: UpdateFilter: expected 200, got {status} / {updated}"
        );
        assert_eq!(
            updated["arn"], arn,
            "case {case}: UpdateFilter must return the filter's ARN, got {updated}"
        );

        let (status, listed) = post(&router, "/filters/list", &json!({})).await;
        assert_eq!(status, 200, "case {case}: ListFilters: got {status} / {listed}");
        let after = listed["filters"][0].clone();

        for (member, input_member) in [
            ("name", "name"),
            ("action", "action"),
            ("criteria", "filterCriteria"),
            ("description", "description"),
            ("reason", "reason"),
        ] {
            let expected = update
                .get(input_member)
                .cloned()
                .unwrap_or_else(|| before[member].clone());
            assert_eq!(
                after[member], expected,
                "case {case}: {member}: expected {expected}, got {}",
                after[member]
            );
        }
        assert_eq!(
            after["tags"], before["tags"],
            "case {case}: UpdateFilter does not take tags, so they must be untouched: {after}"
        );
        assert_eq!(
            after["createdAt"], before["createdAt"],
            "case {case}: createdAt must not move, got {after}"
        );
        let created_at = after["createdAt"].as_f64().expect("a createdAt timestamp");
        let updated_at = after["updatedAt"].as_f64().expect("an updatedAt timestamp");
        assert!(
            updated_at > created_at,
            "case {case}: updatedAt must be refreshed (createdAt {created_at}, updatedAt \
             {updated_at})"
        );
    }
}

/// An ARN that names no stored filter is a 404 `ResourceNotFoundException` for
/// `UpdateFilter`, which has to resolve a filter to apply its changes — including the ARN of
/// a filter already deleted. `DeleteFilter` answers the same ARN with its idempotent
/// success, which `delete_filter_idempotence` pins.
#[tokio::test]
async fn unknown_filter_arns_are_not_found() {
    let router = router();
    let (status, created) = post(
        &router,
        "/filters/create",
        &json!({
            "action": "SUPPRESS",
            "name": "doomed",
            "filterCriteria": { "title": [{ "comparison": "EQUALS", "value": "doomed" }] }
        }),
    )
    .await;
    assert_eq!(status, 200, "CreateFilter: got {status} / {created}");
    let arn = created["arn"].as_str().expect("an arn").to_string();

    let (status, deleted) = post(&router, "/filters/delete", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "DeleteFilter: expected 200, got {status} / {deleted}"
    );
    assert_eq!(
        deleted["arn"], arn,
        "DeleteFilter must return the deleted filter's ARN, got {deleted}"
    );

    let (status, listed) = post(&router, "/filters/list", &json!({})).await;
    assert_eq!(status, 200, "ListFilters: got {status} / {listed}");
    assert_eq!(
        listed["filters"].as_array().map(Vec::len),
        Some(0),
        "a deleted filter must be gone from ListFilters, got {listed}"
    );

    // Rows: the ARN of a filter that was deleted, and a well-formed ARN that was never
    // minted. A value that is not a well-formed filter ARN at all is a 400
    // `ValidationException` instead, which `malformed_filter_arns_are_refused_as_invalid`
    // covers.
    let unknown = [
        ("already deleted", arn.clone()),
        (
            "never minted",
            format!("arn:aws:inspector2:us-east-1:{ACCOUNT}:owner/{ACCOUNT}/filter/deadbeef"),
        ),
        (
            "another account's",
            "arn:aws:inspector2:us-east-1:111122223333:owner/111122223333/filter/0123456789abcdef"
                .to_string(),
        ),
    ];
    for (case, candidate) in unknown {
        let (status, error_type, body) = post_in(
            &router,
            "us-east-1",
            "/filters/update",
            &json!({ "filterArn": candidate, "description": "tuned" }),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case}: UpdateFilter: expected 404, got {status} / {body}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: UpdateFilter: expected ResourceNotFoundException, got {error_type:?}"
        );
    }
}

/// A regional ARN is built from the region the request is signed for, not from a fixed
/// one.
#[tokio::test]
async fn a_filter_arn_carries_the_requests_region() {
    for region in ["us-east-1", "us-west-2", "eu-central-1", "ap-southeast-2"] {
        let router = router();
        let (status, _, created) = post_in(
            &router,
            region,
            "/filters/create",
            &json!({
                "action": "NONE",
                "name": "regional",
                "filterCriteria": { "title": [{ "comparison": "EQUALS", "value": "regional" }] }
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {region}: CreateFilter: expected 200, got {status} / {created}"
        );
        let arn = created["arn"].as_str().unwrap_or_default();
        let expected = format!("arn:aws:inspector2:{region}:{ACCOUNT}:owner/{ACCOUNT}/filter/");
        assert!(
            arn.starts_with(&expected),
            "case {region}: expected an ARN starting {expected}, got {arn:?}"
        );
    }
}

/// The filter store is bounded: once it holds `MAX_STORED_FILTERS` records, `CreateFilter`
/// answers its own modelled `ServiceQuotaExceededException` (402) instead of growing the
/// store, and deleting one makes room again.
#[tokio::test]
async fn create_filter_refuses_past_the_stored_filter_cap() {
    const CAP: usize = 1_000;
    let router = router();
    let new_filter = |index: usize| {
        json!({
            "action": "SUPPRESS",
            "name": format!("filter-{index}"),
            "filterCriteria": { "title": [{ "comparison": "EQUALS", "value": "x" }] }
        })
    };

    let mut last = String::new();
    for index in 0..CAP {
        let (status, created) = post(&router, "/filters/create", &new_filter(index)).await;
        assert_eq!(
            status, 200,
            "case filter #{index}: expected 200 up to the cap, got {status} / {created}"
        );
        last = created["arn"].as_str().expect("an arn").to_string();
    }

    let (status, error_type, body) =
        post_in(&router, "us-east-1", "/filters/create", &new_filter(CAP)).await;
    assert_eq!(
        status, 402,
        "case one past the cap: expected 402, got {status} / {body}"
    );
    assert_eq!(
        error_type.as_deref(),
        Some("ServiceQuotaExceededException"),
        "case one past the cap: expected ServiceQuotaExceededException, got {error_type:?}"
    );

    // The refused request stored nothing.
    let (status, listed) = post(&router, "/filters/list", &json!({ "maxResults": 100 })).await;
    assert_eq!(status, 200, "ListFilters: got {status} / {listed}");
    assert!(
        listed["nextToken"].is_string(),
        "the store should still hold a full cap's worth of filters, got {listed}"
    );

    // Deleting one makes room for exactly one more.
    let (status, deleted) = post(&router, "/filters/delete", &json!({ "arn": last })).await;
    assert_eq!(status, 200, "DeleteFilter: got {status} / {deleted}");
    let (status, created) = post(&router, "/filters/create", &new_filter(CAP)).await;
    assert_eq!(
        status, 200,
        "after a delete there is room for one more filter, got {status} / {created}"
    );
}

/// The account store is bounded: `Enable` refuses to take it past `MAX_ENABLED_ACCOUNTS`,
/// naming the member and the bound, and the refused request stores nothing.
#[tokio::test]
async fn enable_refuses_past_the_enabled_account_cap() {
    const CAP: usize = 1_000;
    // `accountIds` is modelled length 0..100, so the cap is reached in batches of 100.
    const BATCH: usize = 100;
    let router = router();

    for batch in 0..CAP / BATCH {
        let ids: Vec<String> = (0..BATCH)
            .map(|offset| format!("{:012}", batch * BATCH + offset))
            .collect();
        let (status, enabled) = post(
            &router,
            "/enable",
            &json!({ "resourceTypes": ["EC2"], "accountIds": ids }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case batch {batch}: expected 200 up to the cap, got {status} / {enabled}"
        );
    }

    let (status, error_type, body) = post_in(
        &router,
        "us-east-1",
        "/enable",
        &json!({ "resourceTypes": ["EC2"], "accountIds": ["999999999999"] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case one past the cap: expected 400, got {status} / {body}"
    );
    assert_eq!(
        error_type.as_deref(),
        Some("ValidationException"),
        "case one past the cap: expected ValidationException, got {error_type:?}"
    );
    assert_eq!(
        body["fields"][0]["name"], "accountIds",
        "the refusal must name the member it is about, got {body}"
    );

    // Nothing was stored for the refused account, and an account already in the store can
    // still be enabled again. The refused account is therefore not one the caller
    // administers, so it is reported per entry rather than with a status.
    let (status, reported) = post(
        &router,
        "/status/batch/get",
        &json!({ "accountIds": ["999999999999", "000000000042"] }),
    )
    .await;
    assert_eq!(status, 200, "BatchGetAccountStatus: got {status} / {reported}");
    assert_eq!(
        reported["failedAccounts"][0]["accountId"], "999999999999",
        "the refused account must not have been stored, got {reported}"
    );
    assert_eq!(
        reported["accounts"][0]["accountId"], "000000000042",
        "an account inside the cap is still readable, got {reported}"
    );
    assert_eq!(
        reported["accounts"][0]["state"]["status"], "ENABLED",
        "an account inside the cap stays enabled, got {reported}"
    );
}

/// Remembered `clientToken`s are a bounded, oldest-first queue: a token older than the
/// most recent `MAX_CLIENT_TOKENS_PER_ACCOUNT` is no longer remembered, so a request
/// carrying it is applied rather than replayed.
#[tokio::test]
async fn remembered_client_tokens_are_bounded() {
    const CAP: usize = 64;
    let router = router();
    let enable = |token: &str, types: &[&str]| {
        json!({ "resourceTypes": types, "clientToken": token })
    };

    let (status, _) = post(&router, "/enable", &enable("token-oldest", &["EC2"])).await;
    assert_eq!(status, 200, "first Enable: expected 200, got {status}");

    // Still remembered: a replay does not widen the enablement.
    let (status, replayed) = post(&router, "/enable", &enable("token-oldest", &["ECR"])).await;
    assert_eq!(status, 200, "replay: expected 200, got {status} / {replayed}");
    assert_eq!(
        replayed["accounts"][0]["resourceStatus"]["ecr"], "DISABLED",
        "a remembered token must still replay, got {replayed}"
    );

    // Push the oldest token out of the queue.
    for index in 0..CAP {
        let token = format!("token-{index}");
        let (status, pushed) = post(&router, "/enable", &enable(&token, &["EC2"])).await;
        assert_eq!(
            status, 200,
            "case {token}: expected 200, got {status} / {pushed}"
        );
    }

    let (status, applied) = post(&router, "/enable", &enable("token-oldest", &["ECR"])).await;
    assert_eq!(
        status, 200,
        "evicted token: expected 200, got {status} / {applied}"
    );
    assert_eq!(
        applied["accounts"][0]["resourceStatus"]["ecr"], "ENABLED",
        "a token past the bound is no longer remembered, so the request applies: {applied}"
    );
}

/// A filter's stored tags round-trip through `ListFilters`: the entries it was created
/// with, and an EMPTY map when it was created with none or with an empty one — the member
/// is never left out. The table over presence and size lives in
/// `filter_tags_are_always_reported`; this row set only pins the round trip.
#[tokio::test]
async fn filter_tags_round_trip_as_sent() {
    let cases: [(&str, Value, Value); 3] = [
        ("absent", Value::Null, json!({})),
        ("empty map", json!({}), json!({})),
        (
            "one entry",
            json!({ "team": "appsec" }),
            json!({ "team": "appsec" }),
        ),
    ];

    for (case, tags, expected) in cases {
        let router = router();
        let mut request = json!({
            "action": "NONE",
            "name": "tagged",
            "filterCriteria": { "title": [{ "comparison": "EQUALS", "value": "tagged" }] }
        });
        if !tags.is_null() {
            request["tags"] = tags.clone();
        }
        let (status, created) = post(&router, "/filters/create", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateFilter: expected 200, got {status} / {created}"
        );

        let (status, listed) = post(&router, "/filters/list", &json!({})).await;
        assert_eq!(status, 200, "case {case}: ListFilters: got {status} / {listed}");
        let got = listed["filters"][0]
            .get("tags")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            got, expected,
            "case {case}: tags must read back as stored (sent {tags}, expected {expected}, \
             got {got})"
        );
    }
}

// --- the per-account scan configuration -------------------------------------------------
//
// The rule under test: an account's Amazon Inspector scan configuration is the one its last
// `UpdateConfiguration` wrote for it — the ECR automated re-scan setting and the Amazon EC2
// scan mode, each member the call named replacing the value in force and each member it
// omitted leaving that value alone — reported as PENDING while a CHANGE is still being
// applied and as applied once that span has fallen due; an account that was never
// configured individually reports the delegated administrator's configuration, and the
// administrator's own reports the values the service applies until it is asked for others.

/// `GetConfiguration`'s route.
/// Source: `smithy_query(query="operation", operation="GetConfiguration")` — `POST
/// /configuration/get`.
const CONFIGURATION_GET_URI: &str = "/configuration/get";

/// `UpdateConfiguration`'s route.
/// Source: `smithy_query(query="operation", operation="UpdateConfiguration")` — `POST
/// /configuration/update`.
const CONFIGURATION_UPDATE_URI: &str = "/configuration/update";

/// A 12-digit account id other than the calling account's, which `Enable` brings into the
/// organization this emulator presents.
const MEMBER: &str = "222222222222";

/// A 12-digit account id nothing ever recorded, so it is outside what the calling account
/// administers.
const OUTSIDE: &str = "333333333333";

/// Every `EcrRescanDuration` token, in model order — the push-date windows an account may
/// ask for.
/// Source: `smithy_query(query="shape", shape="EcrRescanDuration")`.
const ECR_RESCAN_DURATIONS: [&str; 8] = [
    "LIFETIME",
    "DAYS_30",
    "DAYS_180",
    "DAYS_14",
    "DAYS_60",
    "DAYS_90",
    "DAYS_3",
    "DAYS_7",
];

/// Every `Ec2ScanMode` token, in model order.
/// Source: `smithy_query(query="shape", shape="Ec2ScanMode")`.
const EC2_SCAN_MODES: [&str; 2] = ["EC2_SSM_AGENT_BASED", "EC2_HYBRID"];

/// What a `GetConfiguration` read must report: the ECR push-date window, the two pull-date
/// members (absent while no account in the inheritance chain has configured them), the EC2
/// scan mode and the VM scanner activation (absent while none has asked either way).
struct ExpectedConfiguration {
    rescan_duration: &'static str,
    pull_date_rescan_duration: Option<&'static str>,
    pull_date_rescan_mode: Option<&'static str>,
    scan_mode: &'static str,
    vm_scanner_activated: Option<bool>,
}

/// One row of the scan-configuration table: an `UpdateConfiguration` body, and the
/// configuration the account must report once it has been applied.
struct ConfigurationRow {
    case: &'static str,
    request: Value,
    expected: ExpectedConfiguration,
}

/// `GetConfiguration` — `POST /configuration/get`.
async fn get_configuration(
    router: &MultiServiceRouter,
    body: &Value,
) -> (u16, Option<String>, Value) {
    post_in(router, "us-east-1", CONFIGURATION_GET_URI, body).await
}

/// `UpdateConfiguration` — `POST /configuration/update`.
async fn update_configuration(
    router: &MultiServiceRouter,
    body: &Value,
) -> (u16, Option<String>, Value) {
    post_in(router, "us-east-1", CONFIGURATION_UPDATE_URI, body).await
}

/// `UpdateConfiguration`, asserting the call was accepted.
async fn update_ok(router: &MultiServiceRouter, body: &Value, case: &str) {
    let (status, error_type, response) = update_configuration(router, body).await;
    assert_eq!(
        status, 200,
        "case {case}: UpdateConfiguration: expected 200, got {status} / {response} \
         ({error_type:?})"
    );
}

/// The ECR member of a read, and the EC2 scan-mode member.
fn ecr_state(body: &Value) -> &Value {
    &body["ecrConfiguration"]["rescanDurationState"]
}

fn ec2_scan_mode_state(body: &Value) -> &Value {
    &body["ec2Configuration"]["scanModeState"]
}

/// `Some(token)` rendered as the member it is reported in, and `Value::Null` for the member
/// a read must leave out.
fn member_or_absent(token: Option<&str>) -> Value {
    token.map_or(Value::Null, |token| json!(token))
}

/// The status tokens a read may report `elapsed` after a change: a change is applied over a
/// span, so while that span is open the setting is PENDING, and only a test slow enough to
/// have passed the due instant may see it already applied. Recomputed before each
/// assertion, so a stall between two calls cannot make a test flaky.
fn owed_status(elapsed: Duration) -> Vec<&'static str> {
    if elapsed < CONFIGURATION_TRANSITION {
        vec![CONFIGURATION_PENDING]
    } else {
        vec![CONFIGURATION_PENDING, CONFIGURATION_SUCCESS]
    }
}

/// Assert the configuration the account `request` names is exactly `expected`, read TWICE:
/// the configuration is state, so reading it reports the same thing again and changes
/// nothing a later caller sees.
async fn assert_reports(
    router: &MultiServiceRouter,
    request: &Value,
    case: &str,
    expected: &ExpectedConfiguration,
) {
    for read in 1..=2 {
        let (status, error_type, body) = get_configuration(router, request).await;
        assert_eq!(
            status, 200,
            "case {case}: read #{read}: GetConfiguration: expected 200, got {status} / {body} \
             ({error_type:?})"
        );
        let ecr = ecr_state(&body);
        assert_eq!(
            ecr["rescanDuration"],
            json!(expected.rescan_duration),
            "case {case}: read #{read}: expected rescanDuration {:?}, got {body}",
            expected.rescan_duration
        );
        assert_eq!(
            ecr.get("pullDateRescanDuration")
                .cloned()
                .unwrap_or(Value::Null),
            member_or_absent(expected.pull_date_rescan_duration),
            "case {case}: read #{read}: expected pullDateRescanDuration {:?}, got {body}",
            expected.pull_date_rescan_duration
        );
        assert_eq!(
            ecr.get("pullDateRescanMode")
                .cloned()
                .unwrap_or(Value::Null),
            member_or_absent(expected.pull_date_rescan_mode),
            "case {case}: read #{read}: expected pullDateRescanMode {:?}, got {body}",
            expected.pull_date_rescan_mode
        );
        let ec2 = ec2_scan_mode_state(&body);
        assert_eq!(
            ec2["scanMode"],
            json!(expected.scan_mode),
            "case {case}: read #{read}: expected scanMode {:?}, got {body}",
            expected.scan_mode
        );
        let vm_scanner = body["ec2Configuration"]
            .get("vmScannerState")
            .cloned()
            .unwrap_or(Value::Null);
        match expected.vm_scanner_activated {
            None => assert!(
                vm_scanner.is_null(),
                "case {case}: read #{read}: an account that never asked for the VM scanner \
                 must report no vmScannerState, got {body}"
            ),
            Some(activated) => assert_eq!(
                vm_scanner["activated"],
                json!(activated),
                "case {case}: read #{read}: expected the VM scanner activated={activated}, got \
                 {body}"
            ),
        }
        // Every status member reports one of the two tokens a setting passes through; which
        // one at which instant is pinned by
        // `a_changed_setting_is_pending_until_the_service_has_applied_it`.
        for (member, reported) in [
            ("ecrConfiguration", ecr["status"].as_str()),
            ("ec2Configuration", ec2["scanModeStatus"].as_str()),
        ] {
            let reported = reported.unwrap_or_default();
            assert!(
                [CONFIGURATION_PENDING, CONFIGURATION_SUCCESS].contains(&reported),
                "case {case}: read #{read}: {member} must report an applied or pending status, \
                 got {reported:?} / {body}"
            );
        }
    }
}

/// Poll `GetConfiguration` until both settings report themselves applied, or a generous
/// deadline passes. Waits for the CONDITION rather than for a fixed time.
async fn wait_until_applied(router: &MultiServiceRouter, case: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, _, body) = get_configuration(router, &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case}: GetConfiguration while polling: expected 200, got {status} / {body}"
        );
        let applied = ecr_state(&body)["status"] == json!(CONFIGURATION_SUCCESS)
            && ec2_scan_mode_state(&body)["scanModeStatus"] == json!(CONFIGURATION_SUCCESS);
        if applied {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: the configuration never reported itself applied, last read {body}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// An account nothing has configured reports the configuration the service applies until it
/// is asked for another: the window that never closes, the scan mode an account starts on,
/// both already applied, and no instant of a change that never happened.
#[tokio::test]
async fn an_unconfigured_account_reports_the_configuration_the_service_applies() {
    assert!(
        ECR_RESCAN_DURATIONS.contains(&DEFAULT_ECR_RESCAN_DURATION)
            && EC2_SCAN_MODES.contains(&DEFAULT_EC2_SCAN_MODE),
        "the configuration the service applies must be made of published tokens, got \
         {DEFAULT_ECR_RESCAN_DURATION:?} and {DEFAULT_EC2_SCAN_MODE:?}"
    );

    let router = router();
    // Rows: the two ways a request names the calling account — not at all, and by its id.
    for (case, request) in [
        ("no accountId", json!({})),
        ("its own accountId", json!({ "accountId": ACCOUNT })),
    ] {
        assert_reports(
            &router,
            &request,
            case,
            &ExpectedConfiguration {
                rescan_duration: DEFAULT_ECR_RESCAN_DURATION,
                pull_date_rescan_duration: None,
                pull_date_rescan_mode: None,
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        )
        .await;

        let (_, _, body) = get_configuration(&router, &request).await;
        assert!(
            ecr_state(&body).get("updatedAt").is_none(),
            "case {case}: a setting that never changed reports no updatedAt, got {body}"
        );
        assert_eq!(
            ecr_state(&body)["status"],
            json!(CONFIGURATION_SUCCESS),
            "case {case}: with nothing being applied the ECR setting is applied, got {body}"
        );
        assert_eq!(
            ec2_scan_mode_state(&body)["scanModeStatus"],
            json!(CONFIGURATION_SUCCESS),
            "case {case}: with nothing being applied the EC2 setting is applied, got {body}"
        );
    }
}

/// The configuration an account reports is the one its updates wrote: each member a call
/// names replaces the value in force, each member it omits leaves that value alone, and a
/// call naming nothing changes nothing.
#[tokio::test]
async fn the_scan_configuration_read_back_is_the_one_the_updates_wrote() {
    // Rows differ along WHICH scan type is configured, which of that structure's optional
    // members the call names, and the token asked for — over the whole published
    // `EcrRescanDuration` set, both `Ec2ScanMode` tokens, both pull-date modes and the VM
    // scanner either way.
    let rows: Vec<ConfigurationRow> = vec![
        ConfigurationRow {
            case: "the window already in force",
            request: json!({ "ecrConfiguration": { "rescanDuration": "LIFETIME" } }),
            expected: ExpectedConfiguration {
                rescan_duration: "LIFETIME",
                pull_date_rescan_duration: None,
                pull_date_rescan_mode: None,
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "a 30-day window",
            request: json!({ "ecrConfiguration": { "rescanDuration": "DAYS_30" } }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_30",
                pull_date_rescan_duration: None,
                pull_date_rescan_mode: None,
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "a 180-day window with a pull-date window beside it",
            request: json!({
                "ecrConfiguration": {
                    "rescanDuration": "DAYS_180",
                    "pullDateRescanDuration": "DAYS_90"
                }
            }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_180",
                pull_date_rescan_duration: Some("DAYS_90"),
                pull_date_rescan_mode: None,
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "a 14-day window naming only the pull-date mode",
            request: json!({
                "ecrConfiguration": {
                    "rescanDuration": "DAYS_14",
                    "pullDateRescanMode": "LAST_IN_USE_AT"
                }
            }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_14",
                // Omitted, so the pull-date window stays as the row before left it.
                pull_date_rescan_duration: Some("DAYS_90"),
                pull_date_rescan_mode: Some("LAST_IN_USE_AT"),
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "a 60-day window with both pull-date members at once",
            request: json!({
                "ecrConfiguration": {
                    "rescanDuration": "DAYS_60",
                    "pullDateRescanDuration": "DAYS_3",
                    "pullDateRescanMode": "LAST_PULL_DATE"
                }
            }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_60",
                pull_date_rescan_duration: Some("DAYS_3"),
                pull_date_rescan_mode: Some("LAST_PULL_DATE"),
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "a 90-day window, the pull-date members left alone",
            request: json!({ "ecrConfiguration": { "rescanDuration": "DAYS_90" } }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_90",
                pull_date_rescan_duration: Some("DAYS_3"),
                pull_date_rescan_mode: Some("LAST_PULL_DATE"),
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "the shortest window",
            request: json!({ "ecrConfiguration": { "rescanDuration": "DAYS_3" } }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_3",
                pull_date_rescan_duration: Some("DAYS_3"),
                pull_date_rescan_mode: Some("LAST_PULL_DATE"),
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "a 7-day window with the longest pull-date window",
            request: json!({
                "ecrConfiguration": {
                    "rescanDuration": "DAYS_7",
                    "pullDateRescanDuration": "DAYS_180"
                }
            }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_7",
                pull_date_rescan_duration: Some("DAYS_180"),
                pull_date_rescan_mode: Some("LAST_PULL_DATE"),
                scan_mode: DEFAULT_EC2_SCAN_MODE,
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "the agent-based scan mode, the ECR setting left alone",
            request: json!({ "ec2Configuration": { "scanMode": "EC2_SSM_AGENT_BASED" } }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_7",
                pull_date_rescan_duration: Some("DAYS_180"),
                pull_date_rescan_mode: Some("LAST_PULL_DATE"),
                scan_mode: "EC2_SSM_AGENT_BASED",
                vm_scanner_activated: None,
            },
        },
        ConfigurationRow {
            case: "the hybrid scan mode with the VM scanner activated",
            request: json!({
                "ec2Configuration": { "scanMode": "EC2_HYBRID", "activateVMScanner": true }
            }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_7",
                pull_date_rescan_duration: Some("DAYS_180"),
                pull_date_rescan_mode: Some("LAST_PULL_DATE"),
                scan_mode: "EC2_HYBRID",
                vm_scanner_activated: Some(true),
            },
        },
        ConfigurationRow {
            case: "the scan mode changed, the VM scanner left alone",
            request: json!({ "ec2Configuration": { "scanMode": "EC2_SSM_AGENT_BASED" } }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_7",
                pull_date_rescan_duration: Some("DAYS_180"),
                pull_date_rescan_mode: Some("LAST_PULL_DATE"),
                scan_mode: "EC2_SSM_AGENT_BASED",
                vm_scanner_activated: Some(true),
            },
        },
        ConfigurationRow {
            case: "the VM scanner deactivated",
            request: json!({
                "ec2Configuration": {
                    "scanMode": "EC2_SSM_AGENT_BASED",
                    "activateVMScanner": false
                }
            }),
            expected: ExpectedConfiguration {
                rescan_duration: "DAYS_7",
                pull_date_rescan_duration: Some("DAYS_180"),
                pull_date_rescan_mode: Some("LAST_PULL_DATE"),
                scan_mode: "EC2_SSM_AGENT_BASED",
                vm_scanner_activated: Some(false),
            },
        },
        ConfigurationRow {
            case: "both scan types in one call",
            request: json!({
                "ecrConfiguration": {
                    "rescanDuration": "LIFETIME",
                    "pullDateRescanDuration": "DAYS_14",
                    "pullDateRescanMode": "LAST_IN_USE_AT"
                },
                "ec2Configuration": { "scanMode": "EC2_HYBRID", "activateVMScanner": true }
            }),
            expected: ExpectedConfiguration {
                rescan_duration: "LIFETIME",
                pull_date_rescan_duration: Some("DAYS_14"),
                pull_date_rescan_mode: Some("LAST_IN_USE_AT"),
                scan_mode: "EC2_HYBRID",
                vm_scanner_activated: Some(true),
            },
        },
        ConfigurationRow {
            case: "a call asking for nothing at all",
            request: json!({}),
            expected: ExpectedConfiguration {
                rescan_duration: "LIFETIME",
                pull_date_rescan_duration: Some("DAYS_14"),
                pull_date_rescan_mode: Some("LAST_IN_USE_AT"),
                scan_mode: "EC2_HYBRID",
                vm_scanner_activated: Some(true),
            },
        },
    ];

    // The table spans the published push-date window set and both scan modes, so a token
    // the issue never cited is exercised here too.
    for token in ECR_RESCAN_DURATIONS {
        assert!(
            rows.iter().any(|row| row.expected.rescan_duration == token),
            "the table must cover the published EcrRescanDuration token {token:?}"
        );
    }
    for token in EC2_SCAN_MODES {
        assert!(
            rows.iter().any(|row| row.expected.scan_mode == token),
            "the table must cover the published Ec2ScanMode token {token:?}"
        );
    }

    let router = router();
    for row in &rows {
        update_ok(&router, &row.request, row.case).await;
        assert_reports(&router, &json!({}), row.case, &row.expected).await;
    }
}

/// The ECR setting reports the instant it last CHANGED: nothing while it never has, the
/// instant of a change once one has happened, and the same instant again for a call asking
/// for the window already in force.
#[tokio::test]
async fn the_ecr_setting_reports_the_instant_it_last_changed() {
    let router = router();
    let reported_at = |body: &Value| {
        ecr_state(body)
            .get("updatedAt")
            .and_then(Value::as_f64)
    };

    let (_, _, body) = get_configuration(&router, &json!({})).await;
    assert!(
        reported_at(&body).is_none(),
        "an account that never configured the setting reports no updatedAt, got {body}"
    );

    update_ok(
        &router,
        &json!({ "ecrConfiguration": { "rescanDuration": DEFAULT_ECR_RESCAN_DURATION } }),
        "the window already in force",
    )
    .await;
    let (_, _, body) = get_configuration(&router, &json!({})).await;
    assert!(
        reported_at(&body).is_none(),
        "a call asking for the window in force changes nothing, so there is still no \
         updatedAt, got {body}"
    );

    update_ok(
        &router,
        &json!({ "ecrConfiguration": { "rescanDuration": "DAYS_30" } }),
        "the first change",
    )
    .await;
    let (_, _, body) = get_configuration(&router, &json!({})).await;
    let first = reported_at(&body)
        .unwrap_or_else(|| panic!("a changed setting must report when it changed, got {body}"));

    update_ok(
        &router,
        &json!({ "ecrConfiguration": { "rescanDuration": "DAYS_30" } }),
        "the same window again",
    )
    .await;
    let (_, _, body) = get_configuration(&router, &json!({})).await;
    assert_eq!(
        reported_at(&body),
        Some(first),
        "a call asking for the window in force must not move updatedAt, got {body}"
    );

    update_ok(
        &router,
        &json!({ "ecrConfiguration": { "rescanDuration": "DAYS_180" } }),
        "the later change",
    )
    .await;
    let (_, _, body) = get_configuration(&router, &json!({})).await;
    let later = reported_at(&body)
        .unwrap_or_else(|| panic!("the later change must report when it changed, got {body}"));
    assert!(
        later >= first,
        "the later change must be reported at or after the first, got {later} after {first}"
    );
}

/// A change is reported as pending until the span the service applies it over has fallen
/// due, and a call asking for the values already in force starts no span at all — a
/// transition the CLOCK owes, so merely reading does not bring it forward.
#[tokio::test]
async fn a_changed_setting_is_pending_until_the_service_has_applied_it() {
    let router = router();
    // Rows differ along WHICH scan type changes, including both at once, and the last asks
    // for the values already in force.
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "the ECR window changes",
            json!({ "ecrConfiguration": { "rescanDuration": "DAYS_30" } }),
            true,
        ),
        (
            "the EC2 scan mode changes",
            json!({ "ec2Configuration": { "scanMode": "EC2_SSM_AGENT_BASED" } }),
            true,
        ),
        (
            "both change at once",
            json!({
                "ecrConfiguration": { "rescanDuration": "DAYS_7" },
                "ec2Configuration": { "scanMode": "EC2_HYBRID" }
            }),
            true,
        ),
        (
            "the ECR window is asked for again",
            json!({ "ecrConfiguration": { "rescanDuration": "DAYS_7" } }),
            false,
        ),
    ];

    for (case, request, expect_pending) in rows {
        let started = Instant::now();
        update_ok(&router, &request, case).await;

        let (status, _, body) = get_configuration(&router, &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case}: GetConfiguration after the update: expected 200, got {status} / {body}"
        );
        // Only the scan types the call named have anything to apply.
        let mut reported = Vec::new();
        if request.get("ecrConfiguration").is_some() {
            reported.push(("ecrConfiguration", ecr_state(&body)["status"].as_str()));
        }
        if request.get("ec2Configuration").is_some() {
            reported.push((
                "ec2Configuration",
                ec2_scan_mode_state(&body)["scanModeStatus"].as_str(),
            ));
        }
        for (member, status) in reported {
            let status = status.unwrap_or_default();
            if expect_pending {
                let allowed = owed_status(started.elapsed());
                assert!(
                    allowed.contains(&status),
                    "case {case}: {member} must report one of {allowed:?} inside the span, got \
                     {status:?} / {body}"
                );
            } else {
                assert_eq!(
                    status, CONFIGURATION_SUCCESS,
                    "case {case}: {member} has nothing to apply, so it stays applied, got \
                     {status:?} / {body}"
                );
            }
        }

        wait_until_applied(&router, case).await;
    }
}

/// A member account reports the delegated administrator's configuration until it is
/// configured individually, holds its own once it is, and follows the administrator's again
/// once an inheritance reset drops its own.
#[tokio::test]
async fn a_member_account_follows_the_administrator_until_it_is_configured_itself() {
    let router = router();
    // `Enable` is how an account enters the organization this emulator presents.
    let (status, error_type, body) = post_in(
        &router,
        "us-east-1",
        "/enable",
        &json!({ "resourceTypes": ["ECR"], "accountIds": [MEMBER] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "Enable for the member account: expected 200, got {status} / {body} ({error_type:?})"
    );

    let member = json!({ "accountId": MEMBER });
    let administrator = json!({});

    update_ok(
        &router,
        &json!({
            "ecrConfiguration": { "rescanDuration": "DAYS_30" },
            "ec2Configuration": { "scanMode": "EC2_SSM_AGENT_BASED" }
        }),
        "the administrator's configuration",
    )
    .await;
    // Propagated: the member has nothing of its own, so it reads the administrator's.
    assert_reports(
        &router,
        &member,
        "the member before it is configured",
        &ExpectedConfiguration {
            rescan_duration: "DAYS_30",
            pull_date_rescan_duration: None,
            pull_date_rescan_mode: None,
            scan_mode: "EC2_SSM_AGENT_BASED",
            vm_scanner_activated: None,
        },
    )
    .await;

    update_ok(
        &router,
        &json!({
            "accountId": MEMBER,
            "ecrConfiguration": { "rescanDuration": "DAYS_7" }
        }),
        "the member's own ECR configuration",
    )
    .await;
    assert_reports(
        &router,
        &member,
        "the member once its ECR setting is its own",
        &ExpectedConfiguration {
            rescan_duration: "DAYS_7",
            pull_date_rescan_duration: None,
            pull_date_rescan_mode: None,
            scan_mode: "EC2_SSM_AGENT_BASED",
            vm_scanner_activated: None,
        },
    )
    .await;
    assert_reports(
        &router,
        &administrator,
        "the administrator after the member was configured",
        &ExpectedConfiguration {
            rescan_duration: "DAYS_30",
            pull_date_rescan_duration: None,
            pull_date_rescan_mode: None,
            scan_mode: "EC2_SSM_AGENT_BASED",
            vm_scanner_activated: None,
        },
    )
    .await;

    // A later administrator change reaches the setting the member does NOT hold of its own,
    // and leaves the one it does alone.
    update_ok(
        &router,
        &json!({
            "ecrConfiguration": { "rescanDuration": "DAYS_14" },
            "ec2Configuration": { "scanMode": "EC2_HYBRID" }
        }),
        "the administrator's later configuration",
    )
    .await;
    assert_reports(
        &router,
        &member,
        "the member with one setting of its own",
        &ExpectedConfiguration {
            rescan_duration: "DAYS_7",
            pull_date_rescan_duration: None,
            pull_date_rescan_mode: None,
            scan_mode: "EC2_HYBRID",
            vm_scanner_activated: None,
        },
    )
    .await;

    update_ok(
        &router,
        &json!({
            "accountId": MEMBER,
            "updateConfigurationInheritance": { "ecrConfiguration": INHERIT_FROM_ADMIN }
        }),
        "the ECR inheritance reset",
    )
    .await;
    assert_reports(
        &router,
        &member,
        "the member after its ECR setting is reset to inherit",
        &ExpectedConfiguration {
            rescan_duration: "DAYS_14",
            pull_date_rescan_duration: None,
            pull_date_rescan_mode: None,
            scan_mode: "EC2_HYBRID",
            vm_scanner_activated: None,
        },
    )
    .await;

    // The same holds for the EC2 setting, whose VM scanner state belongs to the override
    // and goes with it.
    update_ok(
        &router,
        &json!({
            "accountId": MEMBER,
            "ec2Configuration": { "scanMode": "EC2_SSM_AGENT_BASED", "activateVMScanner": true }
        }),
        "the member's own EC2 configuration",
    )
    .await;
    assert_reports(
        &router,
        &member,
        "the member once its EC2 setting is its own",
        &ExpectedConfiguration {
            rescan_duration: "DAYS_14",
            pull_date_rescan_duration: None,
            pull_date_rescan_mode: None,
            scan_mode: "EC2_SSM_AGENT_BASED",
            vm_scanner_activated: Some(true),
        },
    )
    .await;
    update_ok(
        &router,
        &json!({
            "accountId": MEMBER,
            "updateConfigurationInheritance": { "ec2Configuration": INHERIT_FROM_ADMIN }
        }),
        "the EC2 inheritance reset",
    )
    .await;
    assert_reports(
        &router,
        &member,
        "the member after its EC2 setting is reset to inherit",
        &ExpectedConfiguration {
            rescan_duration: "DAYS_14",
            pull_date_rescan_duration: None,
            pull_date_rescan_mode: None,
            scan_mode: "EC2_HYBRID",
            vm_scanner_activated: None,
        },
    )
    .await;
}

/// An inheritance reset outside the combination the model documents for it — beside a
/// scan-type configuration, or without the account to reset — is refused 400
/// `ValidationException`, and the refused call changes nothing.
#[tokio::test]
async fn an_inheritance_reset_outside_its_documented_combination_is_refused() {
    let router = router();
    update_ok(
        &router,
        &json!({ "ecrConfiguration": { "rescanDuration": "DAYS_60" } }),
        "the configuration in force",
    )
    .await;
    let in_force = ExpectedConfiguration {
        rescan_duration: "DAYS_60",
        pull_date_rescan_duration: None,
        pull_date_rescan_mode: None,
        scan_mode: DEFAULT_EC2_SCAN_MODE,
        vm_scanner_activated: None,
    };

    // Rows differ along WHICH half of the documented requirement is broken and along which
    // scan type's member breaks it.
    let rows: Vec<(&str, Value)> = vec![
        (
            "no accountId, the ECR member",
            json!({
                "updateConfigurationInheritance": { "ecrConfiguration": INHERIT_FROM_ADMIN }
            }),
        ),
        (
            "no accountId, the EC2 member",
            json!({
                "updateConfigurationInheritance": { "ec2Configuration": INHERIT_FROM_ADMIN }
            }),
        ),
        (
            "beside an ecrConfiguration",
            json!({
                "accountId": ACCOUNT,
                "ecrConfiguration": { "rescanDuration": "DAYS_3" },
                "updateConfigurationInheritance": { "ecrConfiguration": INHERIT_FROM_ADMIN }
            }),
        ),
        (
            "beside an ec2Configuration",
            json!({
                "accountId": ACCOUNT,
                "ec2Configuration": { "scanMode": "EC2_SSM_AGENT_BASED" },
                "updateConfigurationInheritance": { "ec2Configuration": INHERIT_FROM_ADMIN }
            }),
        ),
        (
            "beside both, and with no accountId",
            json!({
                "ecrConfiguration": { "rescanDuration": "DAYS_3" },
                "ec2Configuration": { "scanMode": "EC2_SSM_AGENT_BASED" },
                "updateConfigurationInheritance": {
                    "ecrConfiguration": INHERIT_FROM_ADMIN,
                    "ec2Configuration": INHERIT_FROM_ADMIN
                }
            }),
        ),
    ];

    for (case, request) in rows {
        let (status, error_type, body) = update_configuration(&router, &request).await;
        assert_eq!(
            status, 400,
            "case {case}: UpdateConfiguration: expected 400, got {status} / {body}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {error_type:?} / {body}"
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("updateConfigurationInheritance"),
            "case {case}: the refusal must name the member it is about, got {message:?}"
        );
        assert_reports(&router, &json!({}), case, &in_force).await;
    }
}

/// A configuration call for an account the caller does not administer is refused 403
/// `AccessDeniedException` and changes nothing, while the accounts it does administer — its
/// own, and one `Enable` brought into the organization — are served.
#[tokio::test]
async fn a_configuration_call_for_an_account_outside_the_organization_is_refused() {
    let router = router();
    update_ok(
        &router,
        &json!({ "ecrConfiguration": { "rescanDuration": "DAYS_90" } }),
        "the caller's own configuration",
    )
    .await;
    let in_force = ExpectedConfiguration {
        rescan_duration: "DAYS_90",
        pull_date_rescan_duration: None,
        pull_date_rescan_mode: None,
        scan_mode: DEFAULT_EC2_SCAN_MODE,
        vm_scanner_activated: None,
    };

    // Rows: the read and the write, each naming an account nothing ever recorded, and a
    // write that would change nothing even if it were served.
    let rows: Vec<(&str, &str, Value)> = vec![
        ("the read", CONFIGURATION_GET_URI, json!({ "accountId": OUTSIDE })),
        (
            "the write",
            CONFIGURATION_UPDATE_URI,
            json!({
                "accountId": OUTSIDE,
                "ecrConfiguration": { "rescanDuration": "DAYS_3" }
            }),
        ),
        (
            "a write asking for nothing",
            CONFIGURATION_UPDATE_URI,
            json!({ "accountId": OUTSIDE }),
        ),
    ];

    for (case, uri, request) in rows {
        let (status, error_type, body) = post_in(&router, "us-east-1", uri, &request).await;
        assert_eq!(
            status, 403,
            "case {case}: expected 403, got {status} / {body}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("AccessDeniedException"),
            "case {case}: expected AccessDeniedException, got {error_type:?} / {body}"
        );
    }

    assert_reports(
        &router,
        &json!({}),
        "the caller's own account after the refusals",
        &in_force,
    )
    .await;

    let (status, _, body) = post_in(
        &router,
        "us-east-1",
        "/enable",
        &json!({ "resourceTypes": ["ECR"], "accountIds": [MEMBER] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "Enable for the member account: expected 200, got {status} / {body}"
    );
    assert_reports(
        &router,
        &json!({ "accountId": MEMBER }),
        "an account of the organization the caller administers",
        &in_force,
    )
    .await;
}
