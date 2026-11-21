// SPDX-License-Identifier: CC0-1.0

//! A stack's declared `AWS::CloudFront::CachePolicy` is a policy Amazon CloudFront really
//! holds, addressable by the physical resource id the stack publishes for it.
//!
//! The rule under test: **a resource type a stack creates by calling the OWNING service's
//! own create publishes that service's identifier as its physical resource id, so once the
//! deployment completes the owning service's own read answers for that id with the
//! configuration the template declared** — resolved to the values the owning service
//! supplies for the members a declaration leaves out — **and the identifier is usable
//! everywhere that service takes one**. For a cache policy that read is
//! `GetCachePolicyConfig` (`GET /2020-05-31/cache-policy/{Id}/config`), which answers 200
//! with the stored `CachePolicyConfig`; `NoSuchCachePolicy` (HTTP 404) is modelled ONLY for
//! an id that names no cache policy, so a 404 for an id CloudFormation itself reported is a
//! defect. A later stack UPDATE that re-declares the configuration is applied through that
//! service's own update, so the policy keeps its id and reports the new configuration.
//!
//! The rows differ along what the rule generalises over: both template spellings (YAML short
//! form and JSON long form), which TTLs the declaration states (all three, `MinTTL` alone,
//! and a `MinTTL` above the documented one-day default, so the documented defaults are
//! asserted at and past their boundary), whether the declaration carries a `Comment`, each
//! cache-key name list absent and carrying one or many names, and the behaviour values drawn
//! from CloudFront's own `CachePolicyHeaderBehavior`, `CachePolicyCookieBehavior` and
//! `CachePolicyQueryStringBehavior` catalogs. Every expected value is computed here from what
//! the row declares, never read back out of the answer being checked.
//!
//! Everything goes over the wire each service speaks — awsQuery for CloudFormation, restXml
//! for Amazon CloudFront — through the one router `harness.rs` builds, which mounts
//! CloudFront beside CloudFormation on one engine.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::settled_stack_status;
use crate::stack_lifecycle::{
    call, create_stack, member, router, xml_blocks, xml_text, xml_texts, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The API version every CloudFront route is under, from the HTTP bindings its model
/// declares (`GET /2020-05-31/cache-policy/{Id}/config`).
const CLOUDFRONT_API_VERSION: &str = "2020-05-31";

/// The host a CloudFront request is addressed to: that service's own published host.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";

/// The `DefaultTTL` CloudFront supplies for a configuration that states none:
/// `CachePolicyConfig$DefaultTTL`, "The default value for this field is 86400 seconds (one
/// day). If the value of `MinTTL` is more than 86400 seconds, then the default value for
/// this field is the same as the value of `MinTTL`"
/// (`smithy_query(query="doc",shape="CachePolicyConfig",member="DefaultTTL")`).
const DOCUMENTED_DEFAULT_TTL: i64 = 86_400;

/// The `MaxTTL` CloudFront supplies for a configuration that states none:
/// `CachePolicyConfig$MaxTTL`, "The default value for this field is 31536000 seconds (one
/// year). If the value of `MinTTL` or `DefaultTTL` is more than 31536000 seconds, then the
/// default value for this field is the same as the value of `DefaultTTL`"
/// (`smithy_query(query="doc",shape="CachePolicyConfig",member="MaxTTL")`).
const DOCUMENTED_MAX_TTL: i64 = 31_536_000;

// ---------------------------------------------------------------------------
// Calling the two services
// ---------------------------------------------------------------------------

/// One CloudFront request, over the restXml wire that service speaks, answered with the HTTP
/// status and the XML body.
///
/// The ONE CloudFront caller of this suite: a cache policy, and the distribution that names
/// it, live in Amazon CloudFront, so every read of one goes through that service's own wire
/// here, signed for `cloudfront`.
async fn cloudfront(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, String) {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/cloudfront/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    );
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{CLOUDFRONT_HOST}{path}"))
        .header("host", CLOUDFRONT_HOST)
        .header("authorization", authorization)
        .body(bytes::Bytes::new())
        .expect("a well-formed CloudFront request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, body)
}

/// `GET /2020-05-31/cache-policy/{Id}/config`.
async fn get_cache_policy_config(router: &MultiServiceRouter, id: &str) -> (u16, String) {
    cloudfront(
        router,
        "GET",
        &format!("/{CLOUDFRONT_API_VERSION}/cache-policy/{id}/config"),
    )
    .await
}

/// `GET /2020-05-31/cache-policy/{Id}`.
async fn get_cache_policy(router: &MultiServiceRouter, id: &str) -> (u16, String) {
    cloudfront(
        router,
        "GET",
        &format!("/{CLOUDFRONT_API_VERSION}/cache-policy/{id}"),
    )
    .await
}

/// `GET /2020-05-31/distribution/{Id}/config`.
async fn get_distribution_config(router: &MultiServiceRouter, id: &str) -> (u16, String) {
    cloudfront(
        router,
        "GET",
        &format!("/{CLOUDFRONT_API_VERSION}/distribution/{id}/config"),
    )
    .await
}

/// The physical resource id the stack reports for one logical id.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResource({stack}, {logical_id}) must answer 200, \
         got {code} {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: expected a physical resource id for {logical_id}, got {body}")
    })
}

// ---------------------------------------------------------------------------
// The declarations under test
// ---------------------------------------------------------------------------

/// What a row's `AWS::CloudFront::CachePolicy` declares, and which spelling it is written
/// in.
#[derive(Debug, Clone)]
struct Declared {
    case: &'static str,
    stack: &'static str,
    /// Whether the template is written in the YAML short form or the JSON long form.
    yaml: bool,
    name: &'static str,
    comment: Option<&'static str>,
    min_ttl: i64,
    default_ttl: Option<i64>,
    max_ttl: Option<i64>,
    gzip: bool,
    /// A value of `CachePolicyHeaderBehavior`.
    header_behavior: &'static str,
    headers: &'static [&'static str],
    /// A value of `CachePolicyCookieBehavior`.
    cookie_behavior: &'static str,
    cookies: &'static [&'static str],
    /// A value of `CachePolicyQueryStringBehavior`.
    query_string_behavior: &'static str,
    query_strings: &'static [&'static str],
}

impl Declared {
    /// The `DefaultTTL` the policy runs under: the one the declaration states, or the
    /// documented default for a declaration that states none ([`DOCUMENTED_DEFAULT_TTL`]).
    fn effective_default_ttl(&self) -> i64 {
        self.default_ttl
            .unwrap_or_else(|| self.min_ttl.max(DOCUMENTED_DEFAULT_TTL))
    }

    /// The `MaxTTL` the policy runs under, by the same rule ([`DOCUMENTED_MAX_TTL`]).
    fn effective_max_ttl(&self) -> i64 {
        self.max_ttl
            .unwrap_or_else(|| self.effective_default_ttl().max(DOCUMENTED_MAX_TTL))
    }

    /// The whole template this row deploys, in the spelling it names.
    fn template(&self) -> String {
        if self.yaml {
            self.yaml_template()
        } else {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Policy": {
                        "Type": "AWS::CloudFront::CachePolicy",
                        "Properties": {"CachePolicyConfig": self.config_value()},
                    }
                }
            })
            .to_string()
        }
    }

    /// The declared `CachePolicyConfig`, as a template object: the members the declaration
    /// states and no others, with each cache-key name list spelled the way the type
    /// publishes it (a plain list of names, which the provisioner wraps in the
    /// `{Quantity, Items}` container CloudFront's own model declares).
    fn config_value(&self) -> Value {
        let mut config = Map::new();
        if let Some(comment) = self.comment {
            config.insert("Comment".to_string(), json!(comment));
        }
        config.insert("Name".to_string(), json!(self.name));
        config.insert("MinTTL".to_string(), json!(self.min_ttl));
        if let Some(ttl) = self.default_ttl {
            config.insert("DefaultTTL".to_string(), json!(ttl));
        }
        if let Some(ttl) = self.max_ttl {
            config.insert("MaxTTL".to_string(), json!(ttl));
        }
        config.insert(
            "ParametersInCacheKeyAndForwardedToOrigin".to_string(),
            json!({
                "EnableAcceptEncodingGzip": self.gzip,
                "HeadersConfig": names_value("HeaderBehavior", self.header_behavior, "Headers", self.headers),
                "CookiesConfig": names_value("CookieBehavior", self.cookie_behavior, "Cookies", self.cookies),
                "QueryStringsConfig": names_value(
                    "QueryStringBehavior",
                    self.query_string_behavior,
                    "QueryStrings",
                    self.query_strings,
                ),
            }),
        );
        Value::Object(config)
    }

    /// The same declaration in the YAML short form, so both template spellings of one
    /// declaration reach the same provisioner.
    fn yaml_template(&self) -> String {
        let mut out = String::from(
            "Resources:\n  Policy:\n    Type: AWS::CloudFront::CachePolicy\n    \
             Properties:\n      CachePolicyConfig:\n",
        );
        if let Some(comment) = self.comment {
            out.push_str(&format!("        Comment: {comment}\n"));
        }
        out.push_str(&format!("        Name: {}\n", self.name));
        out.push_str(&format!("        MinTTL: {}\n", self.min_ttl));
        if let Some(ttl) = self.default_ttl {
            out.push_str(&format!("        DefaultTTL: {ttl}\n"));
        }
        if let Some(ttl) = self.max_ttl {
            out.push_str(&format!("        MaxTTL: {ttl}\n"));
        }
        out.push_str("        ParametersInCacheKeyAndForwardedToOrigin:\n");
        out.push_str(&format!(
            "          EnableAcceptEncodingGzip: {}\n",
            self.gzip
        ));
        out.push_str(&names_yaml(
            "HeadersConfig",
            "HeaderBehavior",
            self.header_behavior,
            "Headers",
            self.headers,
        ));
        out.push_str(&names_yaml(
            "CookiesConfig",
            "CookieBehavior",
            self.cookie_behavior,
            "Cookies",
            self.cookies,
        ));
        out.push_str(&names_yaml(
            "QueryStringsConfig",
            "QueryStringBehavior",
            self.query_string_behavior,
            "QueryStrings",
            self.query_strings,
        ));
        out
    }
}

/// One cache-key block as a template object: its behaviour member, and the names it lists
/// where it lists any.
fn names_value(
    behavior_member: &str,
    behavior: &str,
    names_member: &str,
    names: &[&str],
) -> Value {
    let mut block = Map::new();
    block.insert(behavior_member.to_string(), json!(behavior));
    if !names.is_empty() {
        block.insert(names_member.to_string(), json!(names));
    }
    Value::Object(block)
}

/// The same block in the YAML short form.
fn names_yaml(
    block: &str,
    behavior_member: &str,
    behavior: &str,
    names_member: &str,
    names: &[&str],
) -> String {
    let mut out = format!("          {block}:\n            {behavior_member}: {behavior}\n");
    if !names.is_empty() {
        out.push_str(&format!("            {names_member}:\n"));
        for name in names {
            out.push_str(&format!("              - {name}\n"));
        }
    }
    out
}

/// The declarations the rule above is asserted over.
fn declarations() -> Vec<Declared> {
    vec![
        Declared {
            case: "yaml short form, all three TTLs declared, no name list",
            stack: "cfn-cache-policy-yaml",
            yaml: true,
            name: "cfn-declared-every-ttl",
            comment: Some("the policy the template declared"),
            min_ttl: 1,
            default_ttl: Some(60),
            max_ttl: Some(120),
            gzip: true,
            // Every behaviour value here is one of `CachePolicyHeaderBehavior`,
            // `CachePolicyCookieBehavior` and `CachePolicyQueryStringBehavior`
            // (`service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types`'s
            // catalogs, which the cache policy suite of that crate asserts exhaustively).
            header_behavior: "none",
            headers: &[],
            cookie_behavior: "none",
            cookies: &[],
            query_string_behavior: "none",
            query_strings: &[],
        },
        Declared {
            case: "json long form, MinTTL alone, one name per list",
            stack: "cfn-cache-policy-json",
            yaml: false,
            name: "cfn-declared-min-only",
            comment: None,
            min_ttl: 0,
            default_ttl: None,
            max_ttl: None,
            gzip: false,
            header_behavior: "whitelist",
            headers: &["Accept"],
            cookie_behavior: "whitelist",
            cookies: &["session"],
            query_string_behavior: "whitelist",
            query_strings: &["page"],
        },
        Declared {
            case: "json long form, MinTTL past the documented one-day default, many names",
            stack: "cfn-cache-policy-long-min",
            yaml: false,
            name: "cfn-declared-long-min",
            comment: Some("a policy whose MinTTL exceeds one day"),
            // Past `DefaultTTL`'s documented default, so the policy's `DefaultTTL` is this
            // `MinTTL` rather than one day.
            min_ttl: DOCUMENTED_DEFAULT_TTL + 1,
            default_ttl: None,
            max_ttl: None,
            gzip: true,
            header_behavior: "whitelist",
            headers: &["Accept", "Authorization", "CloudFront-Viewer-Country"],
            cookie_behavior: "all",
            cookies: &[],
            query_string_behavior: "all",
            query_strings: &[],
        },
        Declared {
            case: "yaml short form, MaxTTL declared and DefaultTTL left out",
            stack: "cfn-cache-policy-max-only",
            yaml: true,
            name: "cfn-declared-max-only",
            comment: None,
            min_ttl: 2,
            default_ttl: None,
            // Above the documented default `DefaultTTL`, so the declaration is admitted and
            // the resolved `DefaultTTL` is the documented one day.
            max_ttl: Some(DOCUMENTED_DEFAULT_TTL * 2),
            gzip: false,
            // `CachePolicyHeaderBehavior` admits only `none` and `whitelist`, while the
            // cookie and query string behaviours admit `allExcept` and `all` as well.
            header_behavior: "whitelist",
            headers: &["Authorization"],
            cookie_behavior: "allExcept",
            cookies: &["session", "tracking"],
            query_string_behavior: "allExcept",
            query_strings: &["debug"],
        },
    ]
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// Every declared cache policy EXISTS in Amazon CloudFront under the physical resource id
/// the stack reports for it, and that policy carries the configuration the template
/// declared.
#[tokio::test]
async fn declared_cache_policies_are_readable_in_cloudfront_under_the_reported_physical_id() {
    let router = router();
    let mut ids: BTreeSet<String> = BTreeSet::new();

    for declared in declarations() {
        let case = declared.case;
        let template = declared.template();
        let (code, body) = create_stack(&router, declared.stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted, got {code} {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, declared.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {case:?}: the stack must reach CREATE_COMPLETE, got {settled:?}"
        );

        let id = physical_id(&router, case, declared.stack, "Policy").await;
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: every policy must get an identifier of its own, {id} was \
             already published"
        );

        // The issue's own observable: the read CloudFormation's physical id is FOR.
        let (code, config) = get_cache_policy_config(&router, &id).await;
        assert_eq!(
            code, 200,
            "case {case:?}: GetCachePolicyConfig({id}) must answer 200, got {code} {config}"
        );
        assert_declared_config(case, &declared, &config);

        // The resource read reports the same policy under the same id, so the identifier
        // CloudFormation published is the one CloudFront holds the record under.
        let (code, policy) = get_cache_policy(&router, &id).await;
        assert_eq!(
            code, 200,
            "case {case:?}: GetCachePolicy({id}) must answer 200, got {code} {policy}"
        );
        assert_eq!(
            xml_text(&policy, "Id").as_deref(),
            Some(id.as_str()),
            "case {case:?}: GetCachePolicy must report the published id, got {policy}"
        );
        assert_declared_config(case, &declared, &policy);
    }
}

/// Assert `document` reports the configuration `declared` states, with the TTLs resolved by
/// the documented rule.
fn assert_declared_config(case: &str, declared: &Declared, document: &str) {
    assert_eq!(
        xml_text(document, "Name").as_deref(),
        Some(declared.name),
        "case {case:?}: the policy must carry the declared Name, got {document}"
    );
    assert_eq!(
        xml_text(document, "Comment").as_deref(),
        declared.comment,
        "case {case:?}: the policy must carry the declared Comment, got {document}"
    );
    for (member, expected) in [
        ("MinTTL", declared.min_ttl),
        ("DefaultTTL", declared.effective_default_ttl()),
        ("MaxTTL", declared.effective_max_ttl()),
    ] {
        assert_eq!(
            xml_text(document, member).as_deref(),
            Some(expected.to_string().as_str()),
            "case {case:?}: the policy's {member} must be {expected}, got {document}"
        );
    }
    assert_eq!(
        xml_text(document, "EnableAcceptEncodingGzip").as_deref(),
        Some(declared.gzip.to_string().as_str()),
        "case {case:?}: the cache key must carry the declared gzip flag, got {document}"
    );
    for (behavior_member, behavior, block, names_member, names) in [
        (
            "HeaderBehavior",
            declared.header_behavior,
            "HeadersConfig",
            "Headers",
            declared.headers,
        ),
        (
            "CookieBehavior",
            declared.cookie_behavior,
            "CookiesConfig",
            "Cookies",
            declared.cookies,
        ),
        (
            "QueryStringBehavior",
            declared.query_string_behavior,
            "QueryStringsConfig",
            "QueryStrings",
            declared.query_strings,
        ),
    ] {
        assert_eq!(
            xml_text(document, behavior_member).as_deref(),
            Some(behavior),
            "case {case:?}: {behavior_member} must be {behavior:?}, got {document}"
        );
        let inner = xml_blocks(document, block)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected a {block}, got {document}"));
        let listed = xml_blocks(&inner, names_member)
            .into_iter()
            .next()
            .map(|names| xml_texts(&names, "Name"))
            .unwrap_or_default();
        assert_eq!(
            listed,
            names
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: {names_member} must list the declared names, got {document}"
        );
    }
}

/// The policy a stack declares is usable as a cache behaviour's `CachePolicyId` in the same
/// template, and a stack UPDATE that re-declares its TTLs is applied IN PLACE: the policy
/// keeps the id the stack published and reports the new TTLs.
#[tokio::test]
async fn a_declared_cache_policy_is_referenceable_and_updates_in_place() {
    let case = "a cache policy enforced on a distribution the same template declares";
    let router = router();
    let stack = "cfn-cache-policy-on-a-distribution";
    let template = |min_ttl: i64, default_ttl: i64, max_ttl: i64| {
        json!({
            "Resources": {
                "Policy": {
                    "Type": "AWS::CloudFront::CachePolicy",
                    "Properties": {"CachePolicyConfig": {
                        "Name": "cfn-enforced-policy",
                        "MinTTL": min_ttl,
                        "DefaultTTL": default_ttl,
                        "MaxTTL": max_ttl,
                        "ParametersInCacheKeyAndForwardedToOrigin": {
                            "EnableAcceptEncodingGzip": false,
                            "HeadersConfig": {"HeaderBehavior": "none"},
                            "CookiesConfig": {"CookieBehavior": "none"},
                            "QueryStringsConfig": {"QueryStringBehavior": "none"},
                        },
                    }},
                },
                "Cdn": {
                    "Type": "AWS::CloudFront::Distribution",
                    "Properties": {"DistributionConfig": {
                        "Enabled": true,
                        "Comment": "the distribution the policy is enforced on",
                        "Origins": [{
                            "Id": "the-origin",
                            "DomainName": "origin.example.com",
                            "CustomOriginConfig": {
                                "HTTPPort": 80,
                                "HTTPSPort": 443,
                                "OriginProtocolPolicy": "https-only",
                            },
                        }],
                        "DefaultCacheBehavior": {
                            "TargetOriginId": "the-origin",
                            "ViewerProtocolPolicy": "allow-all",
                            "CachePolicyId": {"Ref": "Policy"},
                        },
                    }},
                },
            }
        })
        .to_string()
    };

    let (code, body) = create_stack(&router, stack, &template(1, 60, 120), &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must be accepted, got {code} {body}"
    );
    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "case {case:?}: the stack must reach CREATE_COMPLETE, got {settled:?}"
    );

    let policy_id = physical_id(&router, case, stack, "Policy").await;
    let distribution_id = physical_id(&router, case, stack, "Cdn").await;
    let (code, config) = get_cache_policy_config(&router, &policy_id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetCachePolicyConfig({policy_id}) must answer 200, got {code} {config}"
    );

    // The reference: the cache behaviour the same template declared names the policy by the
    // id CloudFront minted for it, so the distribution CloudFront holds carries that id.
    let (code, distribution) = get_distribution_config(&router, &distribution_id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetDistributionConfig({distribution_id}) must answer 200, \
         got {code} {distribution}"
    );
    assert_eq!(
        xml_text(&distribution, "CachePolicyId").as_deref(),
        Some(policy_id.as_str()),
        "case {case:?}: the cache behaviour must name the declared policy, got {distribution}"
    );

    // The update: re-declared TTLs reach the policy through CloudFront's own update, so the
    // id is unchanged and the configuration is the new one.
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &template(2, 600, 1_200)),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: UpdateStack must be accepted, got {code} {body}"
    );
    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::UPDATE_COMPLETE,
        "case {case:?}: the stack must reach UPDATE_COMPLETE, got {settled:?}"
    );

    let updated_id = physical_id(&router, case, stack, "Policy").await;
    assert_eq!(
        updated_id, policy_id,
        "case {case:?}: an in-place update must keep the policy's id, got {updated_id}"
    );
    let (code, config) = get_cache_policy_config(&router, &policy_id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetCachePolicyConfig({policy_id}) must answer 200 after the update, \
         got {code} {config}"
    );
    for (member, expected) in [("MinTTL", 2), ("DefaultTTL", 600), ("MaxTTL", 1_200)] {
        assert_eq!(
            xml_text(&config, member).as_deref(),
            Some(expected.to_string().as_str()),
            "case {case:?}: the updated policy's {member} must be {expected}, got {config}"
        );
    }
}

/// The complement, so the refusal the issue reported is still the refusal for an id that
/// really names nothing: an id no cache policy holds is `NoSuchCachePolicy` with HTTP 404.
#[tokio::test]
async fn an_id_no_policy_holds_is_still_refused() {
    let router = router();
    // Rows: an id of the form CloudFront mints (a version-4 UUID) that nothing holds, and
    // one outside that form.
    for (case, id) in [
        (
            "a well-formed policy id nothing holds",
            "4f2f0d9e-0a6f-4c6e-9d0a-5b2a7c1e8f30",
        ),
        ("an id outside the minted form", "not-a-policy"),
    ] {
        let (code, body) = get_cache_policy_config(&router, id).await;
        assert_eq!(
            code, 404,
            "case {case:?}: GetCachePolicyConfig({id}) must answer 404, got {code} {body}"
        );
        assert_eq!(
            xml_text(&body, "Code").as_deref(),
            Some("NoSuchCachePolicy"),
            "case {case:?}: the refusal must be NoSuchCachePolicy, got {body}"
        );
    }
}
