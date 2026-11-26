// SPDX-License-Identifier: CC0-1.0

//! The Rule listing, end to end over HTTP: `CreateRule` then `ListRules`.
//!
//! RULE under test: a listing REPORTS the service's own store — it answers exactly the
//! rules a create persisted and nothing else, one row per stored record (so rules sharing
//! a `ruleName` are each listed as the distinct resource they are), every summary member
//! read out of that record, narrowed to one `EntityStatus` when `status` is sent, cut into
//! pages of at most `maxResults` rows walked in one stable order that a returned
//! `nextToken` resumes immediately after, with a `nextToken` present only when a further
//! row exists; a `nextToken` the service did not mint is refused with the modelled
//! `ValidationException` rather than silently restarting the walk.
//!
//! Every case drives the real wire path (`MultiServiceRouter::dispatch` with a signed
//! restJson1 request), so what is asserted is what a client sees. The rows differ along
//! what the rule generalises over: how many rules the store holds, how many of them share
//! a name, which `RuleType` and which status each carries, which `status` the listing
//! filters by (including one no stored rule carries), the page size (one row per page, the
//! exact row count, past it, and the modelled bounds 1 and 100), and the form of a
//! `nextToken` the service never issued.

use std::collections::BTreeMap;
use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// What a caller sees: the HTTP facts it branches on plus the JSON body.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    json: serde_json::Value,
}

impl Reply {
    fn string_at(&self, member: &str) -> String {
        self.json[member]
            .as_str()
            .unwrap_or_else(|| panic!("expected a string {member} in {:?}", self.json))
            .to_string()
    }
}

/// The emulator under test: this one service mounted on the production router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The wire behaviour under test is the operations', not SigV4's.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "network-security-manager.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            // The credential scope is what routes the request to this service and names
            // the region its ARNs carry.
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/us-east-1/\
                 network-security-manager/aws4_request, SignedHeaders=host, Signature=0",
            )
            .body(payload)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw = response.into_body();
        let json = if raw.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null)
        };
        Reply {
            status,
            error_code,
            json,
        }
    }

    /// Persist one rule, `isPublished` deciding the `EntityStatus` it carries.
    async fn create_rule(&self, name: &str, rule_type: &str, is_published: bool) -> Reply {
        let reply = self
            .call(
                "POST",
                "/rules",
                Some(serde_json::json!({
                    "ruleName": name,
                    // `RuleFirewallType` models exactly one value, WAF.
                    "firewallType": "WAF",
                    "ruleType": rule_type,
                    "isPublished": is_published,
                    "configuration": {"Name": name},
                })),
            )
            .await;
        assert_eq!(
            reply.status, 201,
            "creating {name:?}: expected 201, got {} {:?}",
            reply.status, reply.json
        );
        reply
    }

    /// Read one rule back by the ARN the model documents its identifier to be.
    async fn get_rule(&self, rule_arn: &str) -> Reply {
        // `ruleIdentifier` is a NON-greedy `@httpLabel`, so an ARN reaches the server
        // percent-encoded, exactly as an AWS SDK sends it.
        let label = rule_arn.replace('/', "%2F");
        self.call("GET", &format!("/rules/{label}"), None).await
    }

    /// One `ListRules` page.
    async fn list_page(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/rules".to_string()
        } else {
            format!("/rules?{query}")
        };
        self.call("GET", &path, None).await
    }

    /// Follow `nextToken` to the end, returning every summary listed in page order.
    async fn list_all(
        &self,
        max_results: Option<u32>,
        status: Option<&str>,
    ) -> Vec<serde_json::Value> {
        let mut collected: Vec<serde_json::Value> = Vec::new();
        let mut next: Option<String> = None;
        // Generous: each page returns at least one row, so the walk terminates long
        // before this, and a listing that never terminates fails instead of hanging.
        for _ in 0..256 {
            let mut query: Vec<String> = Vec::new();
            if let Some(limit) = max_results {
                query.push(format!("maxResults={limit}"));
            }
            if let Some(filter) = status {
                query.push(format!("status={filter}"));
            }
            if let Some(token) = &next {
                query.push(format!("nextToken={token}"));
            }
            let label = query.join("&");
            let reply = self.list_page(&label).await;
            assert_eq!(
                reply.status, 200,
                "case listing {label:?}: expected 200, got {} {:?}",
                reply.status, reply.json
            );
            let page = reply.json["rules"].as_array().unwrap_or_else(|| {
                panic!(
                    "case listing {label:?}: rules is required, got {:?}",
                    reply.json
                )
            });
            if let Some(limit) = max_results {
                assert!(
                    page.len() <= limit as usize,
                    "case listing {label:?}: a page holds at most maxResults={limit}, got {}",
                    page.len()
                );
            }
            collected.extend(page.iter().cloned());
            match reply.json["nextToken"].as_str() {
                None => return collected,
                Some(token) => {
                    assert!(
                        !page.is_empty(),
                        "case listing {label:?}: a page that continues must not be empty"
                    );
                    next = Some(token.to_string());
                }
            }
        }
        panic!("case following nextToken: the listing never terminated");
    }
}

/// Every `ruleId` in a list of summaries, in the order listed.
fn ids(summaries: &[serde_json::Value]) -> Vec<String> {
    summaries
        .iter()
        .map(|summary| {
            summary["ruleId"]
                .as_str()
                .unwrap_or_else(|| panic!("ruleId is required in a summary, got {summary:?}"))
                .to_string()
        })
        .collect()
}

/// One rule the store holds for the listing cases: what it was created with and the
/// `EntityStatus` `isPublished` gave it.
struct Seeded {
    rule_id: String,
    rule_arn: String,
    rule_name: &'static str,
    rule_type: &'static str,
    status: &'static str,
}

/// The rows: published and unpublished creates of both `RuleType` values interleaved, and
/// a name used by three of them — so neither the insertion order, nor one status, nor a
/// shared name decides what a filtered or paged listing answers.
async fn seed(emulator: &Emulator) -> Vec<Seeded> {
    let mut seeded = Vec::new();
    for (name, rule_type, is_published) in [
        ("shared", "INSPECTION", true),
        ("rule-list-draft", "CONFIGURATION", false),
        ("shared", "CONFIGURATION", false),
        ("rule-list-active", "INSPECTION", true),
        ("shared", "INSPECTION", true),
    ] {
        let created = emulator.create_rule(name, rule_type, is_published).await;
        seeded.push(Seeded {
            rule_id: created.string_at("ruleId"),
            rule_arn: created.string_at("ruleArn"),
            rule_name: name,
            rule_type,
            // `EntityStatus`: "DRAFT (unpublished, editable), ACTIVE (published, in use)".
            status: if is_published { "ACTIVE" } else { "DRAFT" },
        });
    }
    seeded
}

/// RULE: an unfiltered listing reports exactly the persisted rules — one row per stored
/// record, including every rule that shares a name with another — and every summary member
/// is the stored record's, so a `GetRule` of the same rule states the same values.
#[tokio::test]
async fn a_listing_reports_every_created_rule_out_of_the_store() {
    let emulator = Emulator::new();

    // An empty store is still a listing: `rules` is `@required`, so it is present and
    // empty rather than absent, and no token points past the end.
    let empty = emulator.list_page("").await;
    assert_eq!(
        empty.status, 200,
        "case empty store: expected 200, got {} {:?}",
        empty.status, empty.json
    );
    assert_eq!(
        empty.json["rules"],
        serde_json::json!([]),
        "case empty store: rules is required and empty, got {:?}",
        empty.json["rules"]
    );
    assert!(
        empty.json["nextToken"].is_null(),
        "case empty store: nothing follows an empty listing, got {:?}",
        empty.json["nextToken"]
    );

    let seeded = seed(&emulator).await;
    let listed = emulator.list_all(None, None).await;

    let mut expected: Vec<String> = seeded.iter().map(|row| row.rule_id.clone()).collect();
    expected.sort();
    let mut actual = ids(&listed);
    actual.sort();
    assert_eq!(
        actual, expected,
        "case listing every rule: expected {expected:?}, got {actual:?}"
    );

    // Three of the rows carry one name, and the listing still holds three rows for it: a
    // name is not an identifier, so sharing one never collapses two resources into one.
    let shared = listed
        .iter()
        .filter(|summary| summary["ruleName"].as_str() == Some("shared"))
        .count();
    let seeded_shared = seeded
        .iter()
        .filter(|row| row.rule_name == "shared")
        .count();
    assert_eq!(
        shared, seeded_shared,
        "case rules sharing a name: expected {seeded_shared} rows, got {shared} in {actual:?}"
    );

    // Every summary member comes out of the record the read answers with, so the listing
    // cannot state anything a read contradicts.
    let by_id: BTreeMap<String, serde_json::Value> = listed
        .iter()
        .map(|summary| {
            (
                summary["ruleId"].as_str().unwrap_or_default().to_string(),
                summary.clone(),
            )
        })
        .collect();
    for row in &seeded {
        let summary = by_id
            .get(&row.rule_id)
            .unwrap_or_else(|| panic!("case {:?}: not listed, got {actual:?}", row.rule_id));
        let read = emulator.get_rule(&row.rule_arn).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: reading it expected 200, got {} {:?}",
            row.rule_id, read.status, read.json
        );
        for member in [
            "ruleId",
            "ruleArn",
            "ruleName",
            "firewallType",
            "ruleType",
            "status",
            "version",
            "hasPublishedVersion",
            "updatedAt",
        ] {
            assert_eq!(
                summary[member], read.json[member],
                "case {:?} member {member}: the summary must state the stored record, got {:?}",
                row.rule_id, summary[member]
            );
        }
        assert_eq!(
            summary["ruleName"].as_str(),
            Some(row.rule_name),
            "case {:?}: expected ruleName {:?}, got {:?}",
            row.rule_id,
            row.rule_name,
            summary["ruleName"]
        );
        assert_eq!(
            summary["ruleType"].as_str(),
            Some(row.rule_type),
            "case {:?}: expected ruleType {:?}, got {:?}",
            row.rule_id,
            row.rule_type,
            summary["ruleType"]
        );
        assert_eq!(
            summary["status"].as_str(),
            Some(row.status),
            "case {:?}: expected status {:?}, got {:?}",
            row.rule_id,
            row.status,
            summary["status"]
        );
        // A summary carries ONLY `RuleSummary`'s members
        // (`smithy_query(query="shape", shape="RuleSummary")`): the configuration, the
        // description and the update token belong to the read, not to the listing.
        for absent in ["configuration", "updateToken", "ruleDescription", "isSnapshot"] {
            assert!(
                summary[absent].is_null(),
                "case {:?}: a summary states no {absent}, got {:?}",
                row.rule_id,
                summary[absent]
            );
        }
        assert!(
            row.rule_arn.ends_with(&format!(":rule/{}", row.rule_id)),
            "case {:?}: expected an ARN naming the rule, got {:?}",
            row.rule_id,
            row.rule_arn
        );
    }
}

/// RULE: `status` narrows a listing to the rules carrying that `EntityStatus`, and a
/// filter no stored rule matches answers an empty list rather than everything.
#[tokio::test]
async fn a_listing_filters_by_status() {
    let emulator = Emulator::new();
    let seeded = seed(&emulator).await;

    // The filters come from `EntityStatusFilter`
    // (`smithy_query(query="shape", shape="EntityStatusFilter")`): ACTIVE, DRAFT,
    // DISABLED. No create leaves a rule DISABLED, so that row expects nothing.
    for filter in ["ACTIVE", "DRAFT", "DISABLED"] {
        let mut expected: Vec<String> = seeded
            .iter()
            .filter(|row| row.status == filter)
            .map(|row| row.rule_id.clone())
            .collect();
        expected.sort();

        // Paged and unpaged alike, so the filter is applied to the store and not to one
        // page of it.
        for page_size in [None, Some(1), Some(2)] {
            let mut actual = ids(&emulator.list_all(page_size, Some(filter)).await);
            actual.sort();
            assert_eq!(
                actual, expected,
                "case status={filter}, maxResults={page_size:?}: expected {expected:?}, \
                 got {actual:?}"
            );
        }
    }
}

/// RULE: pages are cut out of one stable walk — `maxResults` bounds a page, `nextToken`
/// resumes immediately after the last row returned, and a token is present only when a
/// further row exists — so every page size reports the same rows in the same order.
#[tokio::test]
async fn paging_walks_every_rule_once_in_one_order() {
    let emulator = Emulator::new();
    let seeded = seed(&emulator).await;
    let count = seeded.len() as u32;

    let unpaged = ids(&emulator.list_all(None, None).await);
    assert_eq!(
        unpaged.len(),
        seeded.len(),
        "case unpaged: expected {} rows, got {unpaged:?}",
        seeded.len()
    );

    // The rows differ along page size: one row per page, under the row count, exactly it,
    // one past it, and the modelled `MaxResults` bounds 1 and 100
    // (`smithy_query(query="operation", operation="ListRules")`: range 1..100).
    for page_size in [1, 2, count, count + 1, 100] {
        let paged = ids(&emulator.list_all(Some(page_size), None).await);
        assert_eq!(
            paged, unpaged,
            "case maxResults={page_size}: expected {unpaged:?}, got {paged:?}"
        );
    }

    // A page that is the whole store carries no token: nothing follows it.
    let whole = emulator.list_page(&format!("maxResults={count}")).await;
    assert!(
        whole.json["nextToken"].is_null(),
        "case maxResults={count}: a page holding every rule ends the listing, got {:?}",
        whole.json["nextToken"]
    );

    // A first page of one row does carry a token, and following it resumes AFTER that row.
    let first = emulator.list_page("maxResults=1").await;
    let first_ids = ids(first.json["rules"].as_array().expect("rules is required"));
    let token = first.json["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case maxResults=1: expected a nextToken, got {:?}", first.json))
        .to_string();
    let second = emulator
        .list_page(&format!("maxResults=1&nextToken={token}"))
        .await;
    let second_ids = ids(second.json["rules"].as_array().expect("rules is required"));
    assert_eq!(
        second_ids.len(),
        1,
        "case following nextToken: expected one row, got {second_ids:?}"
    );
    assert_ne!(
        second_ids, first_ids,
        "case following nextToken: a resumed page must not repeat the previous row"
    );
    assert_eq!(
        unpaged[..2].to_vec(),
        vec![first_ids[0].clone(), second_ids[0].clone()],
        "case following nextToken: the resumed walk must follow the unpaged order"
    );
}

/// RULE: a `nextToken` this service did not mint names no page, so it is refused with the
/// modelled `ValidationException` (HTTP 400) naming the offending member.
#[tokio::test]
async fn a_next_token_the_service_never_issued_is_refused() {
    let emulator = Emulator::new();
    seed(&emulator).await;

    // The rows differ along the FORM of the token: one inside `NextToken`'s modelled
    // `^[0-9A-Za-z:/+=_-]+$` but not this service's rendering, one with an unknown
    // version tag, one whose payload is not hex, and one whose payload decodes to
    // nothing.
    for token in ["notatoken", "2:6162", "1:zzzz", "1:"] {
        let reply = emulator
            .list_page(&format!("nextToken={}", token.replace(':', "%3A")))
            .await;
        assert_eq!(
            reply.status, 400,
            "case nextToken={token:?}: expected 400, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ValidationException"),
            "case nextToken={token:?}: unexpected x-amzn-errortype, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.json["fieldList"][0]["name"].as_str(),
            Some("nextToken"),
            "case nextToken={token:?}: the refusal must name nextToken, got {:?}",
            reply.json["fieldList"]
        );
    }
}
