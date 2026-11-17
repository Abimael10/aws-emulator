// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of the calls that address an API's CACHE, and of the account's custom
//! domain INVENTORY — the two spaces of this service that no ARN spelling of its own
//! covers, judged by the resource each request names rather than by the API above it.
//!
//! Two rules, each asserted over a table spanning the class it generalises over rather than
//! the one call that exposed the defect:
//!
//! 1. the decision is taken for EVERY one of these calls before the operation runs, so a
//!    principal an explicit `Deny` covers is refused the modelled `AccessDeniedException`
//!    (HTTP 403) with IAM's explicit-deny sentence — naming the principal, the action
//!    `appsync:<OperationName>` and the resource the request addressed — and the call
//!    performs nothing: no instance is created, emptied or torn down;
//! 2. WHICH resource that is follows from what the request addresses. A cache is
//!    configuration of an API that neither this service's ARN builders nor the resource
//!    types IAM publishes for it name, so the resource is the request's own URI path hung
//!    off this service's ARN prefix (`arn:aws:appsync:<region>:<account>:/v1/apis/<apiId>/…`)
//!    — a grant written about the API's own ARN does not reach it and a grant written about
//!    that path does — while a LISTING of the account's custom domains names no one resource
//!    and is judged against the any-resource wildcard.
//!
//! Every principal is a real one: the roles, their policies and their sessions are made with
//! IAM's and STS's own operations over the same engine, through the harness
//! [`crate::identity_authorization`], so the documents this service reads are the ones those
//! services wrote.
//!
//! The rows span the five cache calls (`CreateApiCache`, `GetApiCache`, `UpdateApiCache`,
//! `DeleteApiCache`, `FlushApiCache` — three methods and three URI paths between them,
//! `smithy_query(query="digest")`) and the domain-name listing, each with the answer an
//! authorized caller gets, which is how a served call is told from a refused one.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::authorization::{
    ACTION_PREFIX, ANY_RESOURCE,
};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use crate::identity_authorization::{
    allow, allow_all_but, arn, default_deny_message, Emulator, Principal, Reply, ACCESS_DENIED,
    ACCESS_DENIED_STATUS, ACCOUNT_KEY,
};

/// AppSync's sentence when a flush finds no cache to empty, which is the answer an
/// authorized caller gets for every API created here: the fixed text of that class, with
/// nothing of the request in it.
const FLUSH_WITHOUT_A_CACHE: &str =
    "Unable to flush the cache as it doesn't exist, please create the cache first.";

/// One call the decision is taken about: its name, how a client sends it, the answer an
/// AUTHORIZED caller gets, and — where the call addresses a cache — the URI segment under
/// the API that identifies what it acts on.
struct Call {
    operation: &'static str,
    method: &'static str,
    /// The path under `/v1/apis/{apiId}/` the call is bound to, or `None` for a call that
    /// does not address an API at all.
    cache_path: Option<&'static str>,
    /// The path of a call that addresses no API, used when `cache_path` is `None`.
    absolute_path: &'static str,
    body: Option<Value>,
    served_status: u16,
}

/// A `CreateApiCache` body with the members that create requires.
/// Source: `smithy_query(query="operation", operation="CreateApiCache")`.
fn create_cache_body() -> Value {
    json!({
        "ttl": 300,
        "apiCachingBehavior": "FULL_REQUEST_CACHING",
        "type": "SMALL",
        "transitEncryptionEnabled": false,
        "atRestEncryptionEnabled": false,
    })
}

/// An `UpdateApiCache` body with the three members that update requires.
/// Source: `smithy_query(query="operation", operation="UpdateApiCache")`.
fn update_cache_body() -> Value {
    json!({
        "ttl": 600,
        "apiCachingBehavior": "PER_RESOLVER_CACHING",
        "type": "SMALL",
    })
}

/// Every call the two rules are asserted over.
fn calls() -> Vec<Call> {
    vec![
        Call {
            operation: "CreateApiCache",
            method: "POST",
            cache_path: Some("ApiCaches"),
            absolute_path: "",
            body: Some(create_cache_body()),
            served_status: 200,
        },
        Call {
            operation: "GetApiCache",
            method: "GET",
            cache_path: Some("ApiCaches"),
            absolute_path: "",
            body: None,
            // The API holds no cache, so an authorized reader meets the absence.
            served_status: 404,
        },
        Call {
            operation: "UpdateApiCache",
            method: "POST",
            cache_path: Some("ApiCaches/update"),
            absolute_path: "",
            body: Some(update_cache_body()),
            served_status: 404,
        },
        Call {
            operation: "DeleteApiCache",
            method: "DELETE",
            cache_path: Some("ApiCaches"),
            absolute_path: "",
            // The API never had a cache, so an authorized teardown has none to remove.
            body: None,
            served_status: 404,
        },
        Call {
            operation: "FlushApiCache",
            method: "DELETE",
            cache_path: Some("FlushCache"),
            absolute_path: "",
            body: None,
            served_status: 404,
        },
        Call {
            operation: "ListDomainNames",
            method: "GET",
            cache_path: None,
            absolute_path: "/v1/domainnames",
            body: None,
            served_status: 200,
        },
    ]
}

impl Call {
    /// The path a client sends this call to, for the API `api_id`.
    fn path(&self, api_id: &str) -> String {
        match self.cache_path {
            Some(under) => format!("/v1/apis/{api_id}/{under}"),
            None => self.absolute_path.to_string(),
        }
    }

    /// The ARN IAM judges this call against: the path of the SPACE the request addresses for
    /// a call on an API's cache (no ARN spelling covers one, and a segment deeper than the
    /// space names the call rather than another resource), and the any-resource wildcard for
    /// a listing, which names no single resource.
    fn judged_resource(&self, api_id: &str) -> String {
        match self.cache_path {
            Some(under) => {
                let space = under.split('/').next().unwrap_or(under);
                arn(&format!("/v1/apis/{api_id}/{space}"))
            }
            None => arn(ANY_RESOURCE),
        }
    }

    /// The action a policy states about this call.
    fn action(&self) -> String {
        format!("{ACTION_PREFIX}:{}", self.operation)
    }
}

/// The refusal IAM words for a request an explicit `Deny` in an identity-based policy
/// covers — the strongest refusal IAM has, and the one the recorded principals met.
fn explicit_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         with an explicit deny in an identity-based policy"
    )
}

/// Create one GraphQL API with the account's own credential and answer its id. Each row gets
/// its own API, so no row sees the state another row left behind.
async fn api(emulator: &Emulator, label: &str) -> String {
    let created = emulator
        .call(
            ACCOUNT_KEY,
            "POST",
            "/v1/apis",
            Some(&json!({"name": format!("cache_gate_{label}"), "authenticationType": "API_KEY"})),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {label:?}: expected CreateGraphqlApi 200, got {created:?}"
    );
    created.body["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: CreateGraphqlApi answers an apiId, got {created:?}"))
        .to_string()
}

/// Assert `reply` is the modelled 403 whose message is `expected`.
fn assert_refused(case: &str, operation: &str, expected: &str, reply: &Reply) {
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?} / {operation}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} \
         {:?} with {:?}",
        reply.status,
        reply.error_code,
        reply.body
    );
    assert_eq!(
        reply.message(),
        expected,
        "case {case:?} / {operation}: expected IAM's sentence {expected:?}, got {:?}",
        reply.message()
    );
}

/// RULE 1: an explicit `Deny` on one of these actions refuses the call with 403
/// `AccessDeniedException` and IAM's explicit-deny sentence, whatever else the principal's
/// policies allow — and the call performs nothing.
#[tokio::test]
async fn an_explicit_deny_refuses_every_cache_call_and_the_domain_listing() {
    let emulator = Emulator::new();
    // A custom domain the account holds, so the listing a denied principal asks for would
    // have had something to report had it been served.
    let held = emulator
        .call(
            ACCOUNT_KEY,
            "POST",
            "/v1/domainnames",
            Some(&json!({
                "domainName": "cache-gate.example.com",
                "certificateArn": "arn:aws:acm:us-east-1:000000000000:certificate/cache-gate",
            })),
        )
        .await;
    assert_eq!(
        held.status, 200,
        "case \"the fixture\": expected CreateDomainName 200, got {held:?}"
    );

    for (index, call) in calls().into_iter().enumerate() {
        let api_id = api(&emulator, &format!("deny{index}")).await;
        let principal: Principal = emulator
            .assumed_role(
                &format!("cache_denied_{index}"),
                Some(&allow_all_but(&call.action())),
            )
            .await;
        let reply = emulator
            .call(
                &principal.access_key_id,
                call.method,
                &call.path(&api_id),
                call.body.as_ref(),
            )
            .await;
        assert_refused(
            "an explicit deny beside a wildcard allow",
            call.operation,
            &explicit_deny_message(
                &principal.arn,
                &call.action(),
                &call.judged_resource(&api_id),
            ),
            &reply,
        );

        // The refused call performed nothing: the API still holds no cache, which is the
        // state every one of these rows started from.
        let read = emulator
            .call(ACCOUNT_KEY, "GET", &format!("/v1/apis/{api_id}/ApiCaches"), None)
            .await;
        assert_eq!(
            read.status, 404,
            "case \"a refused call performs nothing\" / {}: expected the API to still hold no \
             cache, got {read:?}",
            call.operation
        );
    }

    // Nor did the refused listing remove the domain the account holds.
    let listed = emulator
        .call(ACCOUNT_KEY, "GET", "/v1/domainnames", None)
        .await;
    let names: Vec<&str> = listed.body["domainNameConfigs"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["domainName"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        names,
        vec!["cache-gate.example.com"],
        "case \"a refused listing performs nothing\": expected the account's one domain, got \
         {names:?}"
    );
}

/// RULE 2: the resource a grant has to NAME is the one the request addresses — the cache's
/// own URI path, which the API's ARN does not cover — so the same signed call is refused
/// under a grant of that action on the API and served under a grant of it on the path. A
/// listing is judged against the any-resource wildcard, and a grant naming one resource of
/// the account does not cover it.
#[tokio::test]
async fn a_grant_must_name_the_resource_the_request_addresses() {
    let emulator = Emulator::new();

    for (index, call) in calls().into_iter().enumerate() {
        // A grant of this action on a resource that is NOT the one the request addresses:
        // the API that holds the cache, and one custom domain for the listing.
        let short_api = api(&emulator, &format!("short{index}")).await;
        let elsewhere = match call.cache_path {
            Some(_) => arn(&state::api_resource(&short_api)),
            None => arn(&state::domain_name_resource("not-the-one.example.com")),
        };
        let short = emulator
            .user(
                &format!("cache_short_{index}"),
                Some(&allow(&call.action(), &elsewhere)),
            )
            .await;
        let reply = emulator
            .call(
                &short.access_key_id,
                call.method,
                &call.path(&short_api),
                call.body.as_ref(),
            )
            .await;
        assert_refused(
            "a grant naming another resource",
            call.operation,
            &default_deny_message(
                &short.arn,
                call.operation,
                &call.judged_resource(&short_api),
            ),
            &reply,
        );

        // The same call, granted on the resource it really addresses, is served the
        // operation's own answer.
        let exact_api = api(&emulator, &format!("exact{index}")).await;
        let granted = emulator
            .user(
                &format!("cache_exact_{index}"),
                Some(&allow(&call.action(), &call.judged_resource(&exact_api))),
            )
            .await;
        let served = emulator
            .call(
                &granted.access_key_id,
                call.method,
                &call.path(&exact_api),
                call.body.as_ref(),
            )
            .await;
        assert_eq!(
            served.status, call.served_status,
            "case \"a grant naming the resource addressed\" / {}: expected the operation's \
             own {} answer, got {} {:?} with {:?}",
            call.operation, call.served_status, served.status, served.error_code, served.body
        );
    }
}

/// RULE 1 and 2 together on the call the gate was missing: a flush an explicit `Deny`
/// covers is refused 403 with IAM's sentence about the flush's own resource, while the
/// unrestricted caller's identical flush reaches the operation and meets the absence of the
/// cache — the 404 `NotFoundException` AppSync words for it.
#[tokio::test]
async fn a_denied_flush_is_refused_where_the_unrestricted_one_finds_no_cache() {
    let emulator = Emulator::new();
    let api_id = api(&emulator, "flush").await;
    let path = format!("/v1/apis/{api_id}/FlushCache");
    let action = format!("{ACTION_PREFIX}:FlushApiCache");

    let denied = emulator
        .assumed_role("flush_denied", Some(&allow_all_but(&action)))
        .await;
    let refused = emulator
        .call(&denied.access_key_id, "DELETE", &path, None)
        .await;
    assert_refused(
        "an explicit deny on the flush",
        "FlushApiCache",
        &explicit_deny_message(&denied.arn, &action, &arn(&path)),
        &refused,
    );

    let unrestricted = emulator
        .call(ACCOUNT_KEY, "DELETE", &path, None)
        .await;
    assert_eq!(
        (unrestricted.status, unrestricted.error_code.as_str()),
        (404, "NotFoundException"),
        "case \"the unrestricted flush\": expected 404 NotFoundException, got {} {:?} with \
         {:?}",
        unrestricted.status,
        unrestricted.error_code,
        unrestricted.body
    );
    assert_eq!(
        unrestricted.message(),
        FLUSH_WITHOUT_A_CACHE,
        "case \"the unrestricted flush\": expected AppSync's absent-cache sentence, got {:?}",
        unrestricted.message()
    );
}
