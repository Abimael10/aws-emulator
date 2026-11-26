// SPDX-License-Identifier: CC0-1.0

//! An Elastic Load Balancing resource tagged through ELBv2's OWN tag plane is a row of the
//! Resource Groups Tagging index.
//!
//! # The rule
//!
//! The tag set the service that OWNS a resource holds for it is what the tagging index
//! reports for that resource in the Region the resource lives in, keyed by the ARN that
//! service minted — so `GetResources` selects an Elastic Load Balancing resource by its
//! `TagFilters` and by `ResourceTypeFilters` in the `service[:resourceType]` spelling the
//! resource's own ARN embeds, reports the values byte for byte, and reports a resource
//! nothing tagged in no answer at all ("GetResources does not return untagged resources",
//! that operation's own documentation). ELBv2's `AddTags` is what puts a key there and its
//! `RemoveTags` is what takes it away, so a resource whose last key is withdrawn leaves the
//! index.
//!
//! # Why this suite is here
//!
//! Two crates have to agree for the rule to hold — Elastic Load Balancing owns and stores
//! the tag set of each of its resources, the tagging API projects it — so the suite lives on
//! the assembled router with both services mounted on ONE engine, which is how the emulator
//! runs. The issue behind it recorded `AddTags` answered `501 NotImplemented`, which left
//! every Elastic Load Balancing resource invisible to tag-based discovery; one tag write is
//! one row of the rule above, so the estate below spans the taggable KINDS (load balancer,
//! target group, listener, rule), how the tag got onto the resource (its create's `Tags`, a
//! later `AddTags`, an `AddTags` that overwrites a key the create carried), the value shapes
//! that have to survive the round trip (ASCII, an empty value, a Unicode one), and a
//! resource nothing ever tagged.
//!
//! Nothing here waits on a clock: the index is projected from the owning service's store at
//! query time, so every assertion is made on the answer to the very next call.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10 as owner_elb;
use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126 as tagging_api;

// The wire helpers this test crate owns, ONCE: the form encoder every awsQuery body is built
// with and the readers that pull an element — or a keyed list's pairs, with the escaped
// entities decoded — out of an XML answer. A suite that writes its own drifts from the
// others, which is exactly what the byte-for-byte tag values below would hide.
use crate::query_wire::{element, form_encode, opt_element, pairs_of};

/// The Region the estate is created in, and the Region the index is queried in.
const REGION: &str = "us-west-2";

/// The SigV4 signing names the router routes a request to a service by.
const TAGGING: &str = "tagging";
const ELBV2: &str = "elasticloadbalancingv2";

/// The API version every awsQuery envelope below names: the version the ELBv2 model
/// declares.
const ELB_VERSION: &str = "2015-12-01";

/// The tag key the whole estate carries, so one query reaches every row.
const KEY: &str = "sim2real";

/// The VPC the estate's target groups live in.
const VPC: &str = "vpc-12345678";

/// The router carrying Elastic Load Balancing and the tagging API over ONE engine, the
/// arrangement `core/router` builds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises handler behaviour, not SigV4: the `Authorization` header below is
    // present because its credential scope is what routes a request to a signing name and
    // names the Region, not to be verified.
    router.set_request_authentication(false);
    router.register_service(owner_elb::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(tagging_api::EmulatedService::new(engine));
    router
}

/// The `Authorization` header that routes a request to `signing_name` in [`REGION`].
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Elastic Load Balancing operation, over the awsQuery wire that model publishes.
async fn elbv2(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={ELB_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{ELBV2}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ELBV2))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Elastic Load Balancing operation that must be served.
async fn elbv2_ok(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = elbv2(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {action}{params:?}: expected 200 from Elastic Load Balancing, got {status} with \
         body {body}"
    );
    body
}

/// One tagging-API operation, over the awsJson wire that API publishes.
async fn tagging(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let target = format!("ResourceGroupsTaggingAPI_20170126.{operation}");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", &target)
        .header("authorization", authorization(TAGGING))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{target} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, parsed)
}

/// One tagging-API operation that must answer 200.
async fn tagging_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = tagging(router, operation, body.clone()).await;
    assert_eq!(
        status, 200,
        "{operation} with {body} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// `Tags.member.N.Key` / `.Value` fields for an awsQuery tag list.
fn tag_params(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for (index, (key, value)) in tags.iter().enumerate() {
        let position = index + 1;
        fields.push((format!("Tags.member.{position}.Key"), (*key).to_string()));
        fields.push((format!("Tags.member.{position}.Value"), (*value).to_string()));
    }
    fields
}

/// One form field.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// Write `tags` to `arn` through Elastic Load Balancing's own tag plane.
async fn add_tags(router: &MultiServiceRouter, arn: &str, tags: &[(&str, &str)]) {
    let mut fields = vec![p("ResourceArns.member.1", arn)];
    fields.extend(tag_params(tags));
    let _ = elbv2_ok(router, "AddTags", &fields).await;
}

/// Withdraw `keys` from `arn` through Elastic Load Balancing's own tag plane.
async fn remove_tags(router: &MultiServiceRouter, arn: &str, keys: &[&str]) {
    let mut fields = vec![p("ResourceArns.member.1", arn)];
    for (index, key) in keys.iter().enumerate() {
        fields.push(p(&format!("TagKeys.member.{}", index + 1), key));
    }
    let _ = elbv2_ok(router, "RemoveTags", &fields).await;
}

/// The tag set Elastic Load Balancing's OWN `DescribeTags` reports for `arn` — the owning
/// service's reader, so what it holds is read where it is held as well as through the index.
async fn described_tags(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let body = elbv2_ok(router, "DescribeTags", &[p("ResourceArns.member.1", arn)]).await;
    // Each pair is read from the `<member>` that carries BOTH halves, with the escaped
    // entities decoded, so a Unicode or empty value compares as the service wrote it.
    opt_element(&body, "Tags")
        .map(|block| pairs_of(&block, "Key", "Value"))
        .unwrap_or_default()
}

/// The `(ARN -> tags)` map one `GetResources` page reports.
fn reported(body: &Value) -> BTreeMap<String, BTreeMap<String, String>> {
    body["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("a GetResources body with no ResourceTagMappingList: {body}"))
        .iter()
        .map(|mapping| {
            let arn = mapping["ResourceARN"]
                .as_str()
                .unwrap_or_else(|| panic!("a mapping with no ResourceARN: {mapping}"))
                .to_string();
            let tags = mapping["Tags"]
                .as_array()
                .unwrap_or_else(|| panic!("a mapping with no Tags: {mapping}"))
                .iter()
                .map(|tag| {
                    (
                        tag["Key"]
                            .as_str()
                            .unwrap_or_else(|| panic!("a tag with no Key: {tag}"))
                            .to_string(),
                        tag["Value"]
                            .as_str()
                            .unwrap_or_else(|| panic!("a tag with no Value: {tag}"))
                            .to_string(),
                    )
                })
                .collect();
            (arn, tags)
        })
        .collect()
}

/// The ARNs one `GetResources` request selects, in ARN order.
async fn selected(router: &MultiServiceRouter, request: Value) -> Vec<String> {
    let body = tagging_ok(router, "GetResources", request).await;
    let mut arns: Vec<String> = reported(&body).into_keys().collect();
    arns.sort();
    arns
}

/// One resource of the estate: the kind, how its tags got onto it, and the set it carries
/// once every write has landed.
struct Row {
    case: &'static str,
    /// The `service:resourceType` key the tagging API names this resource by, which is the
    /// leading segment of the resource field of the ARN Elastic Load Balancing minted for it
    /// (`GetResources$ResourceTypeFilters`: "the same as that embedded in a resource's ARN").
    resource_type: &'static str,
    /// The pairs the resource's own create declares, where its create takes `Tags`.
    seeded: &'static [(&'static str, &'static str)],
    /// The pairs a later `AddTags` writes.
    written: &'static [(&'static str, &'static str)],
    /// The set the resource carries afterwards, by the rule `AddTags` states ("If a resource
    /// already has a tag with the same key, AddTags updates its value"), computed from the
    /// declarations rather than read back out of an answer.
    settled: &'static [(&'static str, &'static str)],
}

/// A Unicode tag value, so the index is asserted to carry a value byte for byte rather than
/// a value it could have transliterated.
///
/// Letters of three scripts and the separator the key pattern allows: `Tag$Value` is
/// `@pattern ^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$`, so a value is Unicode LETTERS, marks of
/// separation, digits and that punctuation — a symbol such as an emoji is outside it and is
/// refused, which the service's own tag suites pin.
const UNICODE_VALUE: &str = "naïve-日本語-Ωμέγα";

/// The estate: one resource of each taggable kind an ELBv2 create or tag write can reach,
/// each tagged a different way, beside a target group nothing ever tagged.
const ROWS: &[Row] = &[
    Row {
        case: "a load balancer tagged by its own create",
        resource_type: "elasticloadbalancing:loadbalancer",
        seeded: &[(KEY, "true")],
        written: &[],
        settled: &[(KEY, "true")],
    },
    Row {
        case: "a target group whose create's value a later write overwrites",
        resource_type: "elasticloadbalancing:targetgroup",
        seeded: &[(KEY, "true"), ("Owner", "platform")],
        written: &[(KEY, "overwritten"), ("Owner", "")],
        settled: &[("Owner", ""), (KEY, "overwritten")],
    },
    Row {
        case: "a listener tagged only after the fact",
        resource_type: "elasticloadbalancing:listener",
        seeded: &[],
        written: &[(KEY, UNICODE_VALUE)],
        settled: &[(KEY, UNICODE_VALUE)],
    },
    Row {
        case: "a rule tagged by its own create",
        resource_type: "elasticloadbalancing:listener-rule",
        seeded: &[(KEY, "true"), ("Tier", "gold")],
        written: &[],
        settled: &[(KEY, "true"), ("Tier", "gold")],
    },
    Row {
        case: "a target group nothing ever tagged",
        resource_type: "elasticloadbalancing:targetgroup",
        seeded: &[],
        written: &[],
        settled: &[],
    },
];

/// The estate the rule is read over, with the ARNs Elastic Load Balancing minted.
struct Estate {
    router: MultiServiceRouter,
    /// `case -> ARN`, for every row of [`ROWS`].
    arns: BTreeMap<&'static str, String>,
}

impl Estate {
    /// Create the estate through Elastic Load Balancing's own API.
    async fn create() -> Self {
        let router = emulator();
        let mut arns: BTreeMap<&'static str, String> = BTreeMap::new();

        // The load balancer every listener and rule below hangs off, which is itself one row.
        let load_balancer_row = &ROWS[0];
        let mut fields = vec![
            p("Name", "tagindex-alb"),
            p("Subnets.member.1", "subnet-11111111"),
            p("Subnets.member.2", "subnet-22222222"),
        ];
        fields.extend(tag_params(load_balancer_row.seeded));
        let created = elbv2_ok(&router, "CreateLoadBalancer", &fields).await;
        let load_balancer = element(&created, "LoadBalancerArn");
        arns.insert(load_balancer_row.case, load_balancer.clone());

        // The tagged target group, which the listener and the rule forward to.
        let group_row = &ROWS[1];
        let mut fields = vec![
            p("Name", "tagindex-tg"),
            p("Protocol", "HTTP"),
            p("Port", "80"),
            p("VpcId", VPC),
        ];
        fields.extend(tag_params(group_row.seeded));
        let created = elbv2_ok(&router, "CreateTargetGroup", &fields).await;
        let target_group = element(&created, "TargetGroupArn");
        arns.insert(group_row.case, target_group.clone());

        // The listener, tagged only afterwards.
        let listener_row = &ROWS[2];
        let created = elbv2_ok(
            &router,
            "CreateListener",
            &[
                p("LoadBalancerArn", &load_balancer),
                p("Protocol", "HTTP"),
                p("Port", "80"),
                p("DefaultActions.member.1.Type", "forward"),
                p("DefaultActions.member.1.TargetGroupArn", &target_group),
            ],
        )
        .await;
        let listener = element(&created, "ListenerArn");
        arns.insert(listener_row.case, listener.clone());

        // The rule, tagged by its own create.
        let rule_row = &ROWS[3];
        let mut fields = vec![
            p("ListenerArn", &listener),
            p("Priority", "10"),
            p("Conditions.member.1.Field", "path-pattern"),
            p("Conditions.member.1.Values.member.1", "/index"),
            p("Actions.member.1.Type", "forward"),
            p("Actions.member.1.TargetGroupArn", &target_group),
        ];
        fields.extend(tag_params(rule_row.seeded));
        let created = elbv2_ok(&router, "CreateRule", &fields).await;
        let rule = element(&created, "RuleArn");
        arns.insert(rule_row.case, rule);

        // The target group nothing tags.
        let untagged_row = &ROWS[4];
        let created = elbv2_ok(
            &router,
            "CreateTargetGroup",
            &[
                p("Name", "tagindex-untagged-tg"),
                p("Protocol", "HTTP"),
                p("Port", "8080"),
                p("VpcId", VPC),
            ],
        )
        .await;
        arns.insert(untagged_row.case, element(&created, "TargetGroupArn"));

        for row in ROWS {
            if !row.written.is_empty() {
                add_tags(&router, &arns[row.case], row.written).await;
            }
        }

        Self { router, arns }
    }

    /// The ARN of the row whose case is `case`.
    fn arn(&self, case: &str) -> String {
        self.arns
            .get(case)
            .unwrap_or_else(|| panic!("the estate holds no row {case:?}"))
            .clone()
    }

    /// The ARNs of the rows that carry at least one tag, in ARN order.
    fn tagged(&self) -> Vec<String> {
        let mut arns: Vec<String> = ROWS
            .iter()
            .filter(|row| !row.settled.is_empty())
            .map(|row| self.arn(row.case))
            .collect();
        arns.sort();
        arns
    }
}

#[tokio::test]
async fn elbv2_tag_writes_are_what_the_tagging_index_reports() {
    let estate = Estate::create().await;
    let router = &estate.router;

    // 1. Every tagged resource is reported with the set its owner holds — read from the
    //    index and from Elastic Load Balancing's own `DescribeTags`, which must agree.
    let body = tagging_ok(
        router,
        "GetResources",
        json!({ "TagFilters": [{ "Key": KEY }] }),
    )
    .await;
    let indexed = reported(&body);
    for row in ROWS {
        let arn = estate.arn(row.case);
        let expected: BTreeMap<String, String> = row
            .settled
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let owned = described_tags(router, &arn).await;
        assert_eq!(
            owned, expected,
            "case {:?}: Elastic Load Balancing must hold {expected:?} for {arn}, got {owned:?}",
            row.case
        );
        if row.settled.is_empty() {
            // "GetResources does not return untagged resources."
            assert!(
                !indexed.contains_key(&arn),
                "case {:?}: an untagged resource is in no answer, got {indexed:?}",
                row.case
            );
            continue;
        }
        assert_eq!(
            indexed.get(&arn),
            Some(&expected),
            "case {:?}: the index must report the owner's own set for {arn}, got {indexed:?}",
            row.case
        );
    }

    // 2. The selections: the key alone, the key with a value, the bare service token, each
    //    qualified resource type, and the resource list — the forms a discovering client
    //    uses, each over the estate above.
    let tagged = estate.tagged();
    let load_balancer = estate.arn("a load balancer tagged by its own create");
    let group = estate.arn("a target group whose create's value a later write overwrites");
    let listener = estate.arn("a listener tagged only after the fact");
    let rule = estate.arn("a rule tagged by its own create");
    let untagged = estate.arn("a target group nothing ever tagged");

    // (case, the request, the ARNs it must select in ARN order)
    let queries: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "the key alone",
            json!({ "TagFilters": [{ "Key": KEY }] }),
            tagged.clone(),
        ),
        (
            "the key with the value only the load balancer and the rule carry",
            json!({ "TagFilters": [{ "Key": KEY, "Values": ["true"] }] }),
            sorted(&[load_balancer.clone(), rule.clone()]),
        ),
        (
            "the key with the Unicode value only the listener carries",
            json!({ "TagFilters": [{ "Key": KEY, "Values": [UNICODE_VALUE] }] }),
            vec![listener.clone()],
        ),
        (
            "the key with a value nothing carries",
            json!({ "TagFilters": [{ "Key": KEY, "Values": ["no-such-value"] }] }),
            Vec::new(),
        ),
        (
            "two filters, both of which the target group satisfies",
            json!({ "TagFilters": [
                { "Key": KEY, "Values": ["overwritten"] },
                { "Key": "Owner" },
            ] }),
            vec![group.clone()],
        ),
        (
            "the bare service token",
            json!({ "ResourceTypeFilters": ["elasticloadbalancing"] }),
            tagged.clone(),
        ),
        (
            "the load balancer resource type",
            json!({ "ResourceTypeFilters": ["elasticloadbalancing:loadbalancer"] }),
            vec![load_balancer.clone()],
        ),
        (
            "the target group resource type",
            json!({ "ResourceTypeFilters": ["elasticloadbalancing:targetgroup"] }),
            vec![group.clone()],
        ),
        (
            "the listener resource type",
            json!({ "ResourceTypeFilters": ["elasticloadbalancing:listener"] }),
            vec![listener.clone()],
        ),
        (
            "the rule resource type",
            json!({ "ResourceTypeFilters": ["elasticloadbalancing:listener-rule"] }),
            vec![rule.clone()],
        ),
        (
            "the resource list naming one tagged and one untagged resource",
            json!({ "ResourceARNList": [group.clone(), untagged.clone()] }),
            vec![group.clone()],
        ),
    ];

    for (case, request, expected) in &queries {
        let answered = selected(router, request.clone()).await;
        assert_eq!(
            &answered, expected,
            "case {case:?}: expected {expected:?}, got {answered:?}"
        );
    }

    // Every qualified type a row declares is one the index answers to, so no kind of this
    // service is reachable only through the bare token.
    for row in ROWS {
        if row.settled.is_empty() {
            continue;
        }
        let answered = selected(
            router,
            json!({ "ResourceTypeFilters": [row.resource_type] }),
        )
        .await;
        assert!(
            answered.contains(&estate.arn(row.case)),
            "case {:?}: {} must select it, got {answered:?}",
            row.case,
            row.resource_type
        );
    }

    // 3. The withdrawal: a resource whose last key the owner takes away leaves the index,
    //    while a resource that keeps a key stays — and the key that stayed is reported.
    remove_tags(router, &listener, &[KEY]).await;
    remove_tags(router, &group, &[KEY]).await;
    let after = reported(
        &tagging_ok(
            router,
            "GetResources",
            json!({ "ResourceTypeFilters": ["elasticloadbalancing"] }),
        )
        .await,
    );
    assert!(
        !after.contains_key(&listener),
        "case a resource whose last key was withdrawn: expected it out of the index, got \
         {after:?}"
    );
    let kept: BTreeMap<String, String> =
        [("Owner".to_string(), String::new())].into_iter().collect();
    assert_eq!(
        after.get(&group),
        Some(&kept),
        "case a resource that kept one key: expected {kept:?}, got {after:?}"
    );
    assert_eq!(
        described_tags(router, &group).await,
        kept,
        "case a resource that kept one key: the owner must hold exactly what the index reports"
    );
}

/// `arns`, sorted — the order [`selected`] answers in.
fn sorted(arns: &[String]) -> Vec<String> {
    let mut sorted = arns.to_vec();
    sorted.sort();
    sorted
}
