// SPDX-License-Identifier: CC0-1.0

//! Cross-service tag DISCOVERY through `GetResources`: a `ResourceTypeFilters` entry
//! naming the ARN namespace of a service whose resources this API indexes is a legal
//! filter, never a refusal.
//!
//! The rule under test: an entry of `ResourceTypeFilters` is `service[:resourceType]` in
//! the spelling the owning service embeds in its ARNs ("The string for each service name
//! and resource type is the same as that embedded in a resource's Amazon Resource Name
//! (ARN)" — `GetResourcesInput.ResourceTypeFilters`), so for EVERY namespace whose tags
//! this API indexes the call is served: HTTP 200 selecting exactly the currently tagged
//! resources of that namespace that the request's `TagFilters` also select, and an empty
//! `ResourceTagMappingList` when nothing is selected — a filter that matches nothing is a
//! query, not a validation failure.
//!
//! The tables span what the rule generalises over: which namespace the filter names
//! (every owning service this API links, not only the one the report cited), whether the
//! entry is bare or qualified, whether it arrives alone or beside another namespace's
//! entry, how many tags the resource carries and what shape its tag value has, whether
//! the tag filter names values or a key alone, and whether the tag is still there or was
//! removed again.
//!
//! The suite drives the real wire path over ONE engine carrying this API and the
//! `health-agent` owner, which is how the emulator runs.

use serde_json::{json, Value};

// The helpers this crate's `health-agent` suite already publishes: the two-service
// router, the awsJson call into this API, the restJson1 call into the owning service, and
// the readers for a `GetResources` page.
use crate::health_agent_tagged_resource_index::{
    connecthealth, domain, emulator, get_resources, owner_tags, page_arns, page_tags, tags_path,
    HEALTH_AGENT,
};

/// Every service namespace this API indexes the tags of, as an ARN spells it.
///
/// Source: the arms of `visit` / `tagged_resources` in
/// `src/state/owners.rs` — the owning services this crate links. Each one is a namespace a
/// client doing cross-service tag discovery can legally name in `ResourceTypeFilters`, so
/// each one has to be served rather than refused.
const INDEXED_NAMESPACES: &[&str] = &[
    "apigateway",
    "ecs",
    "elasticloadbalancing",
    "events",
    "glue",
    "health-agent",
    "kms",
    "lambda",
    "secretsmanager",
    "sns",
    "sqs",
    "states",
];

/// A resource type to qualify a namespace with that no resource in an empty store answers
/// to, so a qualified entry is exercised without depending on one service's type names.
const UNUSED_TYPE: &str = "resource";

#[tokio::test]
async fn every_namespace_this_api_indexes_is_a_legal_resource_type_filter() {
    let router = emulator();

    // Nothing is tagged, so every row's answer is the same: served, with an empty list.
    // The rows differ in the namespace named, in the bare-versus-qualified form of the
    // entry, in whether a `TagFilters` element accompanies it, and in whether the entry
    // arrives alone or beside another namespace's.
    for namespace in INDEXED_NAMESPACES {
        let qualified = format!("{namespace}:{UNUSED_TYPE}");
        let rows: &[(&str, Value)] = &[
            ("the bare namespace", json!({ "ResourceTypeFilters": [namespace] })),
            (
                "the qualified namespace",
                json!({ "ResourceTypeFilters": [qualified] }),
            ),
            (
                "the namespace with a tag filter",
                json!({
                    "ResourceTypeFilters": [namespace],
                    "TagFilters": [{ "Key": "sweep-probe", "Values": ["sized"] }],
                }),
            ),
            (
                "the namespace beside another one",
                json!({ "ResourceTypeFilters": ["s3", namespace] }),
            ),
        ];
        for (form, request) in rows {
            let case = format!("{namespace}: {form}");
            let page = get_resources(&router, &case, request.clone()).await;
            assert!(
                page_arns(&page).is_empty(),
                "case {case:?}: an empty store must select nothing, got {page}"
            );
        }
    }
}

/// One row of the discovery table: the domain's name, the tag set written to it through
/// the owning service, the tag key the sweep filters on, the `TagFilters` element that
/// names it, and the `ResourceTypeFilters` array the sweep sends.
struct DiscoveryRow {
    case: &'static str,
    name: &'static str,
    tags: Value,
    probe_key: &'static str,
    tag_filter: Value,
    type_filters: Value,
}

#[tokio::test]
async fn a_tag_written_by_the_owning_service_is_discovered_by_its_namespace_filter() {
    let router = emulator();

    // The rows differ in how many tags the resource carries, in the shape of the tag
    // value the sweep matches on (a plain token, a punctuated one, the empty string), in
    // whether the tag filter names values or a key alone, and in the form and company of
    // the resource-type entry (bare, qualified, and second behind another namespace's).
    let rows = [
        DiscoveryRow {
            case: "one tag, matched by key and value under the bare namespace",
            name: "sweep-single-tag",
            tags: json!({ "sweep-one": "alpha" }),
            probe_key: "sweep-one",
            tag_filter: json!({ "Key": "sweep-one", "Values": ["alpha"] }),
            type_filters: json!([HEALTH_AGENT]),
        },
        DiscoveryRow {
            case: "three tags, one of them matched under the qualified namespace",
            name: "sweep-three-tags",
            tags: json!({ "sweep-two": "beta", "env": "prod", "team": "cardiology" }),
            probe_key: "sweep-two",
            tag_filter: json!({ "Key": "sweep-two", "Values": ["beta", "gamma"] }),
            type_filters: json!([format!("{HEALTH_AGENT}:domain")]),
        },
        DiscoveryRow {
            case: "a punctuated tag value, behind another namespace's entry",
            name: "sweep-punctuated-value",
            tags: json!({ "sweep-three": "cost-centre/42.7:eu" }),
            probe_key: "sweep-three",
            tag_filter: json!({ "Key": "sweep-three", "Values": ["cost-centre/42.7:eu"] }),
            type_filters: json!(["kms", HEALTH_AGENT]),
        },
        DiscoveryRow {
            case: "an empty tag value, matched by key alone",
            name: "sweep-empty-value",
            tags: json!({ "sweep-four": "" }),
            probe_key: "sweep-four",
            tag_filter: json!({ "Key": "sweep-four" }),
            type_filters: json!([HEALTH_AGENT]),
        },
    ];

    for row in &rows {
        let case = row.case;
        let (_, arn) = domain(&router, row.name, Value::Null).await;

        // The write goes through the OWNING service, as a client tagging its own resource
        // does, and that service's own reader is what confirms it landed.
        let (status, body) = connecthealth(
            &router,
            "POST",
            &tags_path(&arn),
            Some(json!({ "tags": row.tags })),
        )
        .await;
        assert_eq!(
            status, 204,
            "case {case:?}: the owning service's TagResource must answer 204, got {status} \
             with {body}"
        );
        let held = owner_tags(&router, &arn).await;
        assert!(
            held.contains_key(row.probe_key),
            "case {case:?}: the owning service must hold {:?}, got {held:?}",
            row.probe_key
        );

        // The sweep: the tag filter AND the namespace filter in one request, which is the
        // query a cross-service inventory sends.
        let page = get_resources(
            &router,
            case,
            json!({
                "TagFilters": [row.tag_filter.clone()],
                "ResourceTypeFilters": row.type_filters.clone(),
            }),
        )
        .await;
        assert!(
            page_arns(&page).contains(&arn),
            "case {case:?}: {arn} must be discovered, got {page}"
        );
        assert_eq!(
            page_tags(&page, &arn),
            held,
            "case {case:?}: the discovered mapping must carry the owning service's own tags"
        );

        // And the same query stops returning it once the owning service removes the key,
        // while still being served.
        let (status, body) = connecthealth(
            &router,
            "DELETE",
            &format!("{}?tagKeys={}", tags_path(&arn), row.probe_key),
            None,
        )
        .await;
        assert_eq!(
            status, 204,
            "case {case:?}: the owning service's UntagResource must answer 204, got {status} \
             with {body}"
        );
        let page = get_resources(
            &router,
            case,
            json!({
                "TagFilters": [row.tag_filter.clone()],
                "ResourceTypeFilters": row.type_filters.clone(),
            }),
        )
        .await;
        assert!(
            !page_arns(&page).contains(&arn),
            "case {case:?}: {arn} must be gone after the untag, got {page}"
        );
    }
}

#[tokio::test]
async fn a_namespace_filter_that_selects_nothing_is_an_empty_page() {
    let router = emulator();
    let (_, arn) = domain(&router, "sweep-unmatched", json!({ "env": "prod" })).await;

    // Each row is a legal query that happens to select nothing: a namespace holding no
    // tagged resource, a resource type nothing of that namespace answers to, a tag key no
    // resource carries, and a tag value no resource carries. None of them is a refusal.
    let rows: &[(&str, Value)] = &[
        (
            "a namespace with nothing tagged under it",
            json!({ "ResourceTypeFilters": ["sqs"] }),
        ),
        (
            "a resource type nothing answers to",
            json!({ "ResourceTypeFilters": [format!("{HEALTH_AGENT}:subscription")] }),
        ),
        (
            "a tag key nothing carries",
            json!({
                "ResourceTypeFilters": [HEALTH_AGENT],
                "TagFilters": [{ "Key": "absent-key" }],
            }),
        ),
        (
            "a tag value nothing carries",
            json!({
                "ResourceTypeFilters": [HEALTH_AGENT],
                "TagFilters": [{ "Key": "env", "Values": ["nowhere"] }],
            }),
        ),
    ];
    for (case, request) in rows {
        let page = get_resources(&router, case, request.clone()).await;
        assert!(
            !page_arns(&page).contains(&arn),
            "case {case:?}: {arn} must not be selected, got {page}"
        );
    }

    // The control: the namespace filter alone, with no tag filter, still reports the one
    // tagged resource — so the rows above select nothing because of their filters, not
    // because the index is empty.
    let page = get_resources(
        &router,
        "the namespace filter alone",
        json!({ "ResourceTypeFilters": [HEALTH_AGENT] }),
    )
    .await;
    assert!(
        page_arns(&page).contains(&arn),
        "case the namespace filter alone: {arn} must be selected, got {page}"
    );

    // And nothing above was a refusal: the service answered every one of them, which
    // `get_resources` asserted, including the pair that names a namespace this emulator
    // links no owner for at all.
    for namespace in ["resource-explorer-2", "mediatailor"] {
        let page = get_resources(
            &router,
            namespace,
            json!({ "ResourceTypeFilters": [namespace] }),
        )
        .await;
        assert!(
            page_arns(&page).is_empty(),
            "case {namespace:?}: an unindexed namespace must select nothing, got {page}"
        );
    }
}
