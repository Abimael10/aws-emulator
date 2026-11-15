// SPDX-License-Identifier: CC0-1.0

//! WHICH REGION a WAFv2 resource belongs to, end to end over the wire.
//!
//! The rule asserted here: a resource of the REGIONAL scope belongs to the region of the
//! endpoint that created it — its ARN carries that region
//! (`arn:aws:wafv2:<region>:<account>:regional/<kind>/<name>/<id>`) — so only that
//! region's endpoint lists it and resolves it, another region's endpoint answers the
//! modelled `WAFNonexistentItemException` for the same `Name`+`Scope`+`Id` triple, and the
//! same NAME is free to reuse in another region for an unrelated resource, which gets its
//! own `Id` and an ARN naming that region. A CLOUDFRONT-scoped resource belongs to the one
//! global namespace instead: it is created from any endpoint and read at the one endpoint
//! that namespace is served at ("API and SDKs - For all calls, use the Region endpoint
//! us-east-1", `Scope`'s own doc), whichever endpoint minted it.
//!
//! The rows differ along what the rule generalises over: the RESOURCE KIND (every scoped
//! kind this service mints and indexes by ARN — web ACL, IP set, rule group, regex pattern
//! set), the pair of REGIONS (two pairs, neither of them the global namespace's own
//! endpoint, and one pair that includes it), and the call that reaches for the resource
//! (its listing, its read, its delete, and — for a web ACL — its update).
//!
//! A store partitioned by region is partitioned by a value the caller chooses, so the
//! number of partitions one scope holds is bounded here too, at the published bound and one
//! region past it.
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness in
//! [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK observes.

use serde_json::{json, Value};
use service_com_amazonaws_wafv2_awswaf_20190729::state::{
    MAX_REGIONS_PER_SCOPE, SCOPE_CLOUDFRONT, SCOPE_REGIONAL,
};

use crate::web_acl_lifecycle::{visibility, Waf, CLOUDFRONT_ENDPOINT_REGION};

/// `WAFNonexistentItemException`'s own sentence, as recorded from WAF: no variable part,
/// so another region's read says nothing about what it looked for.
const NONEXISTENT_ITEM: &str =
    "AWS WAF couldn’t perform the operation because your resource doesn’t exist.";

/// The error body's message member, whichever casing the shape models.
fn error_message(body: &Value) -> &str {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// One scoped resource kind: how it is created, read and listed, and the ARN segment and
/// listing member it answers under.
struct Kind {
    case: &'static str,
    create: &'static str,
    get: &'static str,
    delete: &'static str,
    list: &'static str,
    /// The member a listing reports its summaries under.
    listed: &'static str,
    /// The resource-kind segment of this kind's ARNs.
    arn_kind: &'static str,
}

/// Every scoped kind, with the members its create needs beyond `Name` and `Scope`.
fn kinds() -> Vec<(Kind, Value)> {
    vec![
        (
            Kind {
                case: "a web ACL",
                create: "CreateWebACL",
                get: "GetWebACL",
                delete: "DeleteWebACL",
                list: "ListWebACLs",
                listed: "WebACLs",
                arn_kind: "webacl",
            },
            json!({ "DefaultAction": { "Allow": {} } }),
        ),
        (
            Kind {
                case: "an IP set",
                create: "CreateIPSet",
                get: "GetIPSet",
                delete: "DeleteIPSet",
                list: "ListIPSets",
                listed: "IPSets",
                arn_kind: "ipset",
            },
            json!({ "IPAddressVersion": "IPV4", "Addresses": ["10.0.0.0/8"] }),
        ),
        (
            Kind {
                case: "a rule group",
                create: "CreateRuleGroup",
                get: "GetRuleGroup",
                delete: "DeleteRuleGroup",
                list: "ListRuleGroups",
                listed: "RuleGroups",
                arn_kind: "rulegroup",
            },
            json!({ "Capacity": 100, "Rules": [] }),
        ),
        (
            Kind {
                case: "a regex pattern set",
                create: "CreateRegexPatternSet",
                get: "GetRegexPatternSet",
                delete: "DeleteRegexPatternSet",
                list: "ListRegexPatternSets",
                listed: "RegexPatternSets",
                arn_kind: "regexpatternset",
            },
            json!({ "RegularExpressionList": [{ "RegexString": "^/health$" }] }),
        ),
    ]
}

/// The create body for `kind` under `name`: its own members, plus the ones every scoped
/// create carries. `VisibilityConfig` is required by the two kinds that publish metrics
/// and ignored by the others, so it is only added where the kind's own members do not say
/// otherwise.
fn create_body(kind: &Kind, extra: &Value, name: &str, scope: &str) -> Value {
    let mut body = json!({ "Name": name, "Scope": scope });
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    if matches!(kind.create, "CreateWebACL" | "CreateRuleGroup") {
        body["VisibilityConfig"] = visibility(name);
    }
    body
}

/// The names `kind`'s listing reports at `region`.
async fn names_listed(waf: &Waf, kind: &Kind, region: &str, scope: &str) -> Vec<String> {
    let (status, listed) = waf
        .call_from(region, kind.list, json!({ "Scope": scope }))
        .await;
    assert_eq!(
        status, 200,
        "case {:?}: {} at {region} must be served, got status {status} and body {listed}",
        kind.case, kind.list
    );
    listed[kind.listed]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["Name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A REGIONAL resource lives in the region that created it: its inventory, its reads and
/// its name are that region's, and the same name in another region is a different
/// resource.
#[tokio::test]
async fn a_regional_resource_is_reached_only_through_the_region_that_created_it() {
    // Two pairs of endpoints, one of them including the endpoint the global namespace is
    // served at, so the partition is not an artefact of that one region.
    for (home, other) in [
        ("us-west-2", "eu-central-1"),
        ("ap-southeast-2", CLOUDFRONT_ENDPOINT_REGION),
    ] {
        for (kind, extra) in kinds() {
            let waf = Waf::new();
            let case = format!("{} created at {home}, reached from {other}", kind.case);
            let name = "shared-name";

            let (status, created) = waf
                .call_from(
                    home,
                    kind.create,
                    create_body(&kind, &extra, name, SCOPE_REGIONAL),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the create must be served at its own endpoint, got status \
                 {status} and body {created}"
            );
            let home_id = created["Summary"]["Id"]
                .as_str()
                .expect("Summary.Id")
                .to_string();
            let home_arn = created["Summary"]["ARN"]
                .as_str()
                .expect("Summary.ARN")
                .to_string();
            assert!(
                home_arn.starts_with(&format!("arn:aws:wafv2:{home}:"))
                    && home_arn.contains(&format!(":regional/{}/", kind.arn_kind)),
                "case {case:?}: the ARN must name the region that created it, got \
                 {home_arn:?}"
            );

            // The home region lists it; the other region does not.
            assert_eq!(
                names_listed(&waf, &kind, home, SCOPE_REGIONAL).await,
                vec![name.to_string()],
                "case {case:?}: the creating region's inventory must hold the name"
            );
            assert_eq!(
                names_listed(&waf, &kind, other, SCOPE_REGIONAL).await,
                Vec::<String>::new(),
                "case {case:?}: another region's inventory must not hold it"
            );

            // The home region resolves the triple; the other region answers not-found,
            // with the exception type's own sentence and nothing about what was asked for.
            let triple = json!({ "Name": name, "Scope": SCOPE_REGIONAL, "Id": &home_id });
            let (read_status, read) = waf.call_from(home, kind.get, triple.clone()).await;
            assert_eq!(
                read_status, 200,
                "case {case:?}: the creating region must resolve the triple, got status \
                 {read_status} and body {read}"
            );
            let (missing_status, missing) = waf.call_from(other, kind.get, triple).await;
            assert_eq!(
                missing_status, 400,
                "case {case:?}: another region must not resolve the triple, got status \
                 {missing_status} and body {missing}"
            );
            assert!(
                missing["__type"]
                    .as_str()
                    .unwrap_or_default()
                    .ends_with("WAFNonexistentItemException"),
                "case {case:?}: expected the nonexistent-item refusal, got {missing}"
            );
            assert_eq!(
                error_message(&missing),
                NONEXISTENT_ITEM,
                "case {case:?}: expected WAF's not-found sentence, got {missing}"
            );

            // The same NAME is free in the other region, and what it names there is a
            // different resource: its own id, and an ARN naming its own region.
            let (reused_status, reused) = waf
                .call_from(
                    other,
                    kind.create,
                    create_body(&kind, &extra, name, SCOPE_REGIONAL),
                )
                .await;
            assert_eq!(
                reused_status, 200,
                "case {case:?}: the name must be free to reuse in another region, got \
                 status {reused_status} and body {reused}"
            );
            let other_id = reused["Summary"]["Id"]
                .as_str()
                .expect("Summary.Id")
                .to_string();
            let other_arn = reused["Summary"]["ARN"]
                .as_str()
                .expect("Summary.ARN")
                .to_string();
            assert_ne!(
                other_id, home_id,
                "case {case:?}: the reused name must name a resource of its own"
            );
            assert!(
                other_arn.starts_with(&format!("arn:aws:wafv2:{other}:")),
                "case {case:?}: the ARN must name the region that created it, got \
                 {other_arn:?}"
            );

            // Each region now resolves its OWN resource by the shared name, and neither
            // resolves the other's id.
            for (region, own, foreign) in [(home, &home_id, &other_id), (other, &other_id, &home_id)]
            {
                let (status, read) = waf
                    .call_from(
                        region,
                        kind.get,
                        json!({ "Name": name, "Scope": SCOPE_REGIONAL, "Id": own }),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: {region} must resolve its own resource, got status \
                     {status} and body {read}"
                );
                let (status, read) = waf
                    .call_from(
                        region,
                        kind.get,
                        json!({ "Name": name, "Scope": SCOPE_REGIONAL, "Id": foreign }),
                    )
                    .await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: {region} must not resolve the other region's id, got \
                     status {status} and body {read}"
                );
            }

            // A DELETE is keyed by the same namespace: deleting the home region's
            // resource leaves the other region's — the one sharing its name — exactly
            // where it was.
            let home_lock = created["Summary"]["LockToken"]
                .as_str()
                .expect("Summary.LockToken")
                .to_string();
            let (status, deleted) = waf
                .call_from(
                    home,
                    kind.delete,
                    json!({
                        "Name": name,
                        "Scope": SCOPE_REGIONAL,
                        "Id": &home_id,
                        "LockToken": &home_lock,
                    }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the delete must be served in its own region, got status \
                 {status} and body {deleted}"
            );
            assert_eq!(
                names_listed(&waf, &kind, home, SCOPE_REGIONAL).await,
                Vec::<String>::new(),
                "case {case:?}: the deleted resource must be gone from its own region"
            );
            assert_eq!(
                names_listed(&waf, &kind, other, SCOPE_REGIONAL).await,
                vec![name.to_string()],
                "case {case:?}: the other region's resource of the same name must survive"
            );
            let (status, survivor) = waf
                .call_from(
                    other,
                    kind.get,
                    json!({ "Name": name, "Scope": SCOPE_REGIONAL, "Id": &other_id }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the other region must still resolve its own resource, got \
                 status {status} and body {survivor}"
            );
        }
    }
}

/// A web ACL's MUTATIONS are its own region's too: another region's endpoint refuses the
/// update and the delete as a nonexistent item, and the web ACL at home is untouched —
/// including its `LockToken`.
#[tokio::test]
async fn another_region_cannot_update_or_delete_a_web_acl_it_does_not_hold() {
    let waf = Waf::new();
    let home = "us-west-2";
    let other = "eu-central-1";
    let name = "regional-mutations";

    let created = waf
        .call_from(
            home,
            "CreateWebACL",
            json!({
                "Name": name,
                "Scope": SCOPE_REGIONAL,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(name),
            }),
        )
        .await;
    assert_eq!(
        created.0, 200,
        "the create must be served, got status {} and body {}",
        created.0, created.1
    );
    let id = created.1["Summary"]["Id"]
        .as_str()
        .expect("Summary.Id")
        .to_string();
    let lock = created.1["Summary"]["LockToken"]
        .as_str()
        .expect("Summary.LockToken")
        .to_string();

    // (case, operation, the body it carries beside the triple)
    let mutations: Vec<(&str, &str, Value)> = vec![
        (
            "an update from another region",
            "UpdateWebACL",
            json!({
                "DefaultAction": { "Block": {} },
                "VisibilityConfig": visibility(name),
            }),
        ),
        ("a delete from another region", "DeleteWebACL", json!({})),
    ];
    for (case, operation, extra) in mutations {
        let mut request = json!({
            "Name": name,
            "Scope": SCOPE_REGIONAL,
            "Id": &id,
            "LockToken": &lock,
        });
        if let Some(members) = extra.as_object() {
            for (member, value) in members {
                request[member] = value.clone();
            }
        }
        let (status, body) = waf.call_from(other, operation, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: another region holds no such web ACL, got status {status} and \
             body {body}"
        );
        assert_eq!(
            error_message(&body),
            NONEXISTENT_ITEM,
            "case {case:?}: expected WAF's not-found sentence, got {body}"
        );
    }

    // The web ACL at home is exactly as it was: its default action, and its lock token.
    let held = waf
        .call_from(
            home,
            "GetWebACL",
            json!({ "Name": name, "Scope": SCOPE_REGIONAL, "Id": &id }),
        )
        .await;
    assert_eq!(
        held.1["LockToken"].as_str(),
        Some(lock.as_str()),
        "a refused mutation from another region must not rotate the LockToken, got {}",
        held.1
    );
    assert_eq!(
        held.1["WebACL"]["DefaultAction"],
        json!({ "Allow": {} }),
        "a refused update from another region must leave the stored document, got {}",
        held.1
    );
}

/// A store partitioned by region is partitioned by a value the CALLER chooses — the
/// region of the credential scope its request carries — so the number of partitions one
/// scope's store holds is bounded: a create in a region beyond the bound is refused with
/// the modelled `WAFLimitsExceededException`, while a create in a region the scope already
/// holds a resource in is still served.
#[tokio::test]
async fn the_number_of_regions_one_scope_is_partitioned_into_is_bounded() {
    let waf = Waf::new();
    let name = "partition-bound";

    // Fill the bound with one web ACL per region, each region named by its position so
    // every create lands in a partition of its own.
    for position in 0..MAX_REGIONS_PER_SCOPE {
        let region = format!("eu-test-{position}");
        let (status, body) = waf
            .call_from(
                &region,
                "CreateWebACL",
                json!({
                    "Name": name,
                    "Scope": SCOPE_REGIONAL,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(name),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {region:?}: a region within the bound must be served, got status \
             {status} and body {body}"
        );
    }

    // One region past the bound is refused, and nothing is stored for it.
    let beyond = format!("eu-test-{MAX_REGIONS_PER_SCOPE}");
    let (status, body) = waf
        .call_from(
            &beyond,
            "CreateWebACL",
            json!({
                "Name": name,
                "Scope": SCOPE_REGIONAL,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(name),
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "a region past the bound must be refused, got status {status} and body {body}"
    );
    assert!(
        body["__type"]
            .as_str()
            .unwrap_or_default()
            .ends_with("WAFLimitsExceededException"),
        "expected the limits refusal, got {body}"
    );
    let (_status, listed) = waf
        .call_from(&beyond, "ListWebACLs", json!({ "Scope": SCOPE_REGIONAL }))
        .await;
    assert_eq!(
        listed["WebACLs"],
        json!([]),
        "a refused create must store nothing in that region, got {listed}"
    );

    // A region the scope already holds a resource in is still served: the bound limits
    // the partitions, not the resources.
    let (status, body) = waf
        .call_from(
            "eu-test-0",
            "CreateWebACL",
            json!({
                "Name": "partition-bound-second",
                "Scope": SCOPE_REGIONAL,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility("partition-bound-second"),
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "a region already partitioned must still be served, got status {status} and body \
         {body}"
    );
}

/// The other half of the rule: a CLOUDFRONT-scoped resource belongs to the ONE global
/// namespace, so one created from a regional endpoint is listed and resolved at the
/// endpoint that namespace is served at — the partition is the REGIONAL scope's, not every
/// scope's.
#[tokio::test]
async fn the_global_namespace_is_one_namespace_whichever_endpoint_minted_it() {
    for minted_at in ["us-west-2", "ap-southeast-2", CLOUDFRONT_ENDPOINT_REGION] {
        let waf = Waf::new();
        let name = "global-name";
        let created = waf
            .call_from(
                minted_at,
                "CreateWebACL",
                json!({
                    "Name": name,
                    "Scope": SCOPE_CLOUDFRONT,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(name),
                }),
            )
            .await;
        assert_eq!(
            created.0, 200,
            "case {minted_at:?}: the create must be served, got status {} and body {}",
            created.0, created.1
        );
        let id = created.1["Summary"]["Id"]
            .as_str()
            .expect("Summary.Id")
            .to_string();
        let arn = created.1["Summary"]["ARN"]
            .as_str()
            .expect("Summary.ARN")
            .to_string();
        assert!(
            arn.starts_with(&format!("arn:aws:wafv2:{CLOUDFRONT_ENDPOINT_REGION}:"))
                && arn.contains(":global/webacl/"),
            "case {minted_at:?}: a global resource's ARN names the region its namespace \
             lives in, got {arn:?}"
        );

        let (status, read) = waf
            .call_from(
                CLOUDFRONT_ENDPOINT_REGION,
                "GetWebACL",
                json!({ "Name": name, "Scope": SCOPE_CLOUDFRONT, "Id": &id }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {minted_at:?}: the global namespace is read where it lives, got status \
             {status} and body {read}"
        );
        assert_eq!(
            read["WebACL"]["ARN"].as_str(),
            Some(arn.as_str()),
            "case {minted_at:?}: the read must answer the web ACL that was minted, got \
             {read}"
        );

        let (status, listed) = waf
            .call_from(
                CLOUDFRONT_ENDPOINT_REGION,
                "ListWebACLs",
                json!({ "Scope": SCOPE_CLOUDFRONT }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {minted_at:?}: the global listing is served where the namespace lives, \
             got status {status} and body {listed}"
        );
        let names: Vec<&str> = listed["WebACLs"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["Name"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            names,
            vec![name],
            "case {minted_at:?}: the global namespace's own listing must hold the name, \
             got {listed}"
        );
    }
}
