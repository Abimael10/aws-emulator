// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a `CreateAPIKey` request gets for its token domains, end to end over
//! the wire.
//!
//! The rule asserted here: asking one key to cover more token domains than the
//! documented ceiling allows is a QUOTA failure, not a malformed parameter. WAF
//! answers HTTP 400 `WAFLimitsExceededException` whose message is the resource-limit
//! sentence ending in the name of the quota that was hit, and carries no `Field` or
//! `Parameter` naming a domain — nothing about one domain is wrong. The per-domain
//! judgement keeps its own class: a value that is no usable token domain is the
//! `WAFInvalidParameterException` with `Field` TOKEN_DOMAIN, `Parameter` that domain
//! and the parameter-format reason, and a value the model's own constraints refuse is
//! the request validator's `ValidationException`.
//!
//! The two read differently to a client — a quota is raised or the key is split,
//! where a malformed domain is rewritten — so which class answers decides which
//! branch a client takes.
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness
//! in [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK
//! observes.

use serde_json::{json, Value};
use service_com_amazonaws_wafv2_awswaf_20190729::state::{
    named_limit_exceeded_message, MAX_TOKEN_DOMAINS_PER_API_KEY,
    NUM_TOKEN_DOMAINS_IN_API_KEY_QUOTA, SCOPE_CLOUDFRONT, SCOPE_REGIONAL,
};

use crate::parameter_validation::error_message;
use crate::web_acl_lifecycle::Waf;

/// The operation under test.
const CREATE: &str = "CreateAPIKey";

/// The keys a scope holds, by their stored value.
async fn listed_keys(waf: &Waf, scope: &str) -> Vec<String> {
    let page = waf.ok("ListAPIKeys", json!({ "Scope": scope })).await;
    page["APIKeySummaries"]
        .as_array()
        .map(|summaries| {
            summaries
                .iter()
                .filter_map(|summary| summary["APIKey"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `count` distinct token domains, each a registrable name.
fn domains(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("app{index}.example.com"))
        .collect()
}

/// A request past the documented ceiling is the quota refusal, in either scope.
///
/// The rows differ along how FAR past the ceiling the request is — one over, several
/// over, and a list several times the ceiling — because the quota is the same one
/// whatever the overshoot, and along the scope, because the ceiling is a property of
/// a key rather than of a namespace. Each row also checks that nothing was minted.
#[tokio::test]
async fn more_token_domains_than_one_key_may_carry_is_the_quota_refusal() {
    let waf = Waf::new();
    for scope in [SCOPE_REGIONAL, SCOPE_CLOUDFRONT] {
        let before = listed_keys(&waf, scope).await;
        for count in [
            MAX_TOKEN_DOMAINS_PER_API_KEY + 1,
            MAX_TOKEN_DOMAINS_PER_API_KEY + 3,
            MAX_TOKEN_DOMAINS_PER_API_KEY * 4,
        ] {
            let case = format!("{scope}, {count} token domains");
            let body = waf
                .refused(
                    CREATE,
                    json!({ "Scope": scope, "TokenDomains": domains(count) }),
                    "WAFLimitsExceededException",
                    &case,
                )
                .await;
            assert_eq!(
                error_message(&body),
                named_limit_exceeded_message(NUM_TOKEN_DOMAINS_IN_API_KEY_QUOTA),
                "case {case:?}: the refusal must name the quota that was hit, got {body}"
            );
            // The quota refusal describes the account's limits, not one member of the
            // request: it names no field, no parameter and no reason.
            assert_eq!(
                (
                    body["Field"].as_str(),
                    body["Parameter"].as_str(),
                    body["Reason"].as_str()
                ),
                (None, None, None),
                "case {case:?}: a quota refusal must carry no parameter detail, got \
                 {body}"
            );
            assert!(
                body["APIKey"].is_null(),
                "case {case:?}: a refused mint must answer no key, got {body}"
            );
        }
        assert_eq!(
            listed_keys(&waf, scope).await,
            before,
            "case {scope:?}: a refused mint must store nothing"
        );
    }
}

/// The ceiling ITSELF is served: a request naming exactly as many domains as one key
/// may carry mints a key, which reads back carrying all of them.
#[tokio::test]
async fn the_documented_ceiling_itself_is_served() {
    let waf = Waf::new();
    let wanted = domains(MAX_TOKEN_DOMAINS_PER_API_KEY);
    let minted = waf
        .ok(
            CREATE,
            json!({ "Scope": SCOPE_REGIONAL, "TokenDomains": wanted.clone() }),
        )
        .await;
    let api_key = minted["APIKey"]
        .as_str()
        .unwrap_or_else(|| panic!("the mint at the ceiling answers an APIKey, got {minted}"))
        .to_string();
    let decrypted = waf
        .ok(
            "GetDecryptedAPIKey",
            json!({ "Scope": SCOPE_REGIONAL, "APIKey": &api_key }),
        )
        .await;
    let mut reported: Vec<String> = decrypted["TokenDomains"]
        .as_array()
        .unwrap_or_else(|| panic!("the decrypted key reports TokenDomains, got {decrypted}"))
        .iter()
        .filter_map(|domain| domain.as_str().map(str::to_string))
        .collect();
    reported.sort();
    let mut expected = wanted;
    expected.sort();
    assert_eq!(
        reported, expected,
        "a key minted at the ceiling must carry every domain it was given, got \
         {decrypted}"
    );
}

/// One value that is no usable token domain, with the class its refusal takes.
struct PerDomain {
    /// What the case is called in an assertion.
    case: &'static str,
    /// The value the request names among otherwise usable domains.
    domain: &'static str,
    /// The error code WAF answers: the service's own parameter refusal, or the
    /// request validator's.
    code: &'static str,
}

/// The per-domain rows: a value at fault keeps the parameter classes, whether the
/// list is short or at the ceiling.
///
/// The rows span the ways ONE domain can be unusable — a public suffix (the rule
/// `TokenDomains` states), a value carrying a path segment rather than being a name,
/// an empty label — beside a value the model's own `@pattern` refuses, which the
/// request validator answers before the operation sees it
/// (`smithy_query(query="shape",shape="TokenDomain")` → `@length(1..253)`,
/// `@pattern ^[\w\.\-/]+$`).
fn per_domain() -> Vec<PerDomain> {
    vec![
        PerDomain {
            case: "a public suffix",
            domain: "co.uk",
            code: "WAFInvalidParameterException",
        },
        PerDomain {
            case: "a name carrying a path segment",
            domain: "app.example.com/login",
            code: "WAFInvalidParameterException",
        },
        PerDomain {
            case: "a bare path",
            domain: "/login",
            code: "WAFInvalidParameterException",
        },
        PerDomain {
            case: "a name with an empty label",
            domain: "app..example.com",
            code: "WAFInvalidParameterException",
        },
        PerDomain {
            case: "a domain written with a scheme",
            domain: "https://app.example.com",
            code: "ValidationException",
        },
    ]
}

/// The `Reason` WAF carries for a parameter whose text is not in the form that
/// parameter takes, as the recorded refusals of that class word it.
const FORMAT_NOT_VALID: &str = "The parameter contains formatting that is not valid.";

/// A value that is no usable token domain keeps the parameter class, at every list
/// length — including a list already at the ceiling, where the quota is NOT what is
/// wrong.
#[tokio::test]
async fn a_domain_that_is_no_usable_name_keeps_the_parameter_class() {
    let waf = Waf::new();
    for PerDomain { case, domain, code } in per_domain() {
        // Alongside one usable domain, and in a list filled to the ceiling: neither
        // length is a quota failure.
        for fill in [1, MAX_TOKEN_DOMAINS_PER_API_KEY - 1] {
            let case = format!("{case}, beside {fill} usable domains");
            let mut requested = domains(fill);
            requested.push(domain.to_string());
            let body = waf
                .refused(
                    CREATE,
                    json!({ "Scope": SCOPE_REGIONAL, "TokenDomains": requested }),
                    code,
                    &case,
                )
                .await;
            if code == "WAFInvalidParameterException" {
                assert_eq!(
                    (
                        body["Field"].as_str(),
                        body["Parameter"].as_str(),
                        body["Reason"].as_str()
                    ),
                    (Some("TOKEN_DOMAIN"), Some(domain), Some(FORMAT_NOT_VALID)),
                    "case {case:?}: the refusal must name the token-domain field, the \
                     value at fault and the format reason, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    format!(
                        "Error reason: {FORMAT_NOT_VALID}, field: TOKEN_DOMAIN, \
                         parameter: {domain}"
                    ),
                    "case {case:?}: the message must be composed from Reason, Field \
                     and Parameter, got {body}"
                );
            }
            assert!(
                body["APIKey"].is_null(),
                "case {case:?}: a refused mint must answer no key, got {body}"
            );
        }
    }
    assert!(
        listed_keys(&waf, SCOPE_REGIONAL).await.is_empty(),
        "no refused mint may store a key"
    );
}

/// A list that is BOTH past the ceiling and carrying an unusable domain is one
/// request with two faults; whichever WAF reports, it reports exactly one refusal and
/// mints nothing.
#[tokio::test]
async fn a_request_at_fault_twice_is_refused_once_and_mints_nothing() {
    let waf = Waf::new();
    let mut requested = domains(MAX_TOKEN_DOMAINS_PER_API_KEY + 2);
    requested.push("co.uk".to_string());
    let (status, body) = waf
        .call(
            CREATE,
            json!({ "Scope": SCOPE_REGIONAL, "TokenDomains": requested }),
        )
        .await;
    assert_eq!(
        status, 400,
        "a request at fault twice must be refused, got status {status} and body {body}"
    );
    let code: &str = body["__type"].as_str().unwrap_or_default();
    assert!(
        code.ends_with("WAFLimitsExceededException")
            || code.ends_with("WAFInvalidParameterException"),
        "the refusal must be one of the two classes the request earned, got {body}"
    );
    assert!(
        body["APIKey"].is_null(),
        "a refused mint must answer no key, got {body}"
    );
    assert!(
        listed_keys(&waf, SCOPE_REGIONAL).await.is_empty(),
        "a refused mint must store nothing, got {body}"
    );
}

/// The quota sentence is a property of the quota, not of the request: it carries no
/// value the request named.
#[tokio::test]
async fn the_quota_sentence_names_the_quota_and_nothing_from_the_request() {
    let waf = Waf::new();
    let secret_domain = "do-not-echo-me.example.com";
    let mut requested = domains(MAX_TOKEN_DOMAINS_PER_API_KEY);
    requested.push(secret_domain.to_string());
    let body: Value = waf
        .refused(
            CREATE,
            json!({ "Scope": SCOPE_REGIONAL, "TokenDomains": requested }),
            "WAFLimitsExceededException",
            "a sixth domain",
        )
        .await;
    let message = error_message(&body);
    assert!(
        !message.contains(secret_domain) && !message.contains("app0.example.com"),
        "the quota sentence must name no domain from the request, got {message:?}"
    );
    assert!(
        message.ends_with(NUM_TOKEN_DOMAINS_IN_API_KEY_QUOTA),
        "the quota sentence must end in the quota's own name, got {message:?}"
    );
}
