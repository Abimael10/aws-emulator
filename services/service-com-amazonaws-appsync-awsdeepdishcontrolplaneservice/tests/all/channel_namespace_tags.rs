// SPDX-License-Identifier: CC0-1.0

//! The tags of a channel namespace, read and written through the one tagging route, end to
//! end over HTTP.
//!
//! RULE: the tagging surface (`/v1/tags/{resourceArn}`) addresses every resource this
//! service ISSUES an ARN for that carries tags — a `GraphqlApi`, an `Api` and a
//! `ChannelNamespace` of one, which are exactly the resources whose create takes `tags` —
//! so an ARN this service minted is served (200, or the absent RESOURCE where this account
//! holds none under it) and only a value of no such form is refused as a malformed ARN.
//! `ResourceArn`'s modelled `@pattern`/`@length` describe the API form alone, which is the
//! one form the member itself can state: a namespace's ARN is that ARN followed by the path
//! the service names the namespace under, and is longer than the member's window.
//!
//! The tables span what the rule generalises over rather than the one ARN that exposed the
//! defect:
//!
//! 1. the SERVED forms — a GraphQL API's ARN, an Event API's, and the ARNs of namespaces
//!    whose names span `Namespace`'s own expression (one character, a hyphenated name, the
//!    longest name it admits) — each read back, written to and cleared through the same
//!    route, with every other resource's map left alone;
//! 2. a well-formed ARN of a served form that names nothing — a namespace of an API that
//!    exists, a namespace under an API id nothing created, and a DELETED namespace's ARN —
//!    which is the absent resource (404), not a malformed ARN;
//! 3. the REFUSED forms — a namespace path with no name, a name one character past the
//!    longest admitted, a child path this service publishes no tag map for, and an ARN of
//!    another vendor — each the surface's own 400 `BadRequestException`.
//!
//! No expected ARN is written by hand: every served row reads the ARN the service answered
//! for the resource it created.
//!
//! Every assertion goes through the router the production emulator mounts, so these exercise
//! routing, the `@httpLabel` ARN binding, restJson1 (de)serialization, the handlers and the
//! shared engine together. The harness is the error-contract suite's `call`/`router` with
//! the channel-namespace suite's create helpers and the tag suites' request helpers.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::channel_namespaces::{create_event_api, create_namespace};
use crate::error_contract::{call, create_api, router};
use crate::graphql_api_lifecycle::REGION;
use crate::resource_tags::{ABSENT_API_ID, ACCOUNT};
use crate::tag_resource::{list_tags, tag_resource, RESOURCE_NOT_FOUND, TAG_WRITE_STATUS};
use crate::untag_resource::untag_resource;

/// `Namespace`'s published maximum length (`@length` 1..50, `smithy_query(query="shape",
/// shape="Namespace")`), which is the longest name a namespace ARN can carry.
const NAMESPACE_MAX_LEN: usize = 50;

/// The authorization type the `Api` these namespaces hang off is configured with.
const AUTH_TYPES: &[&str] = &["API_KEY"];

/// The fixed sentence the surface refuses a value of no issued ARN form with.
const INVALID_RESOURCE_ARN: &str = "Invalid input resource arn";

/// The same sentence for a value that IS an ARN, of a vendor this service does not serve.
const EXPECTED_VENDOR: &str = "Invalid input resource arn: expected vendor appsync";

/// Create one channel namespace with `tags` and answer the ARN the service minted for it.
async fn namespace_arn(
    router: &MultiServiceRouter,
    api_id: &str,
    name: &str,
    tags: Value,
) -> String {
    let created = create_namespace(
        router,
        api_id,
        json!({ "name": name, "tags": tags.clone() }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case create the namespace {name:?}: expected 200, got {created:?}"
    );
    assert_eq!(
        created.body["channelNamespace"]["tags"], tags,
        "case create the namespace {name:?}: expected the requested tags back, got {created:?}"
    );
    created.body["channelNamespace"]["channelNamespaceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case create the namespace {name:?}: expected an ARN"))
        .to_string()
}

/// Rule 1: every ARN this service issues for a taggable resource is read, written and
/// cleared through the one route, and a write reaches only the resource its ARN names.
#[tokio::test]
async fn every_issued_resource_arn_round_trips_its_tags() {
    let router = router();

    // One resource of every taggable kind, each created with tags of its own.
    let graphql_api = call(
        &router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": "tagged-graphql",
            "authenticationType": "API_KEY",
            "tags": { "kind": "graphql" },
        })),
    )
    .await;
    assert_eq!(
        graphql_api.status, 200,
        "case create the GraphQL API: expected 200, got {graphql_api:?}"
    );
    let graphql_arn = graphql_api.body["graphqlApi"]["arn"]
        .as_str()
        .expect("the created GraphQL API carries an ARN")
        .to_string();

    let event_api = call(
        &router,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": "tagged-events",
            "tags": { "kind": "event" },
            "eventConfig": {
                "authProviders": [{ "authType": "API_KEY" }],
                "connectionAuthModes": [{ "authType": "API_KEY" }],
                "defaultPublishAuthModes": [{ "authType": "API_KEY" }],
                "defaultSubscribeAuthModes": [{ "authType": "API_KEY" }],
            },
        })),
    )
    .await;
    assert_eq!(
        event_api.status, 200,
        "case create the Event API: expected 200, got {event_api:?}"
    );
    let event_arn = event_api.body["api"]["apiArn"]
        .as_str()
        .expect("the created Event API carries an ARN")
        .to_string();
    let api_id = event_api.body["api"]["apiId"]
        .as_str()
        .expect("the created Event API carries an id")
        .to_string();

    // The namespace rows span `Namespace`'s own expression: the shortest name it admits,
    // a hyphenated one, and the longest it admits.
    let mut rows: Vec<(String, String, Value)> = vec![
        (
            "the GraphQL API's own ARN".to_string(),
            graphql_arn,
            json!({ "kind": "graphql" }),
        ),
        (
            "the Event API's own ARN".to_string(),
            event_arn,
            json!({ "kind": "event" }),
        ),
    ];
    for (case, name) in [
        ("a namespace named as AppSync's default", "default".to_string()),
        ("a namespace whose name is one character", "n".to_string()),
        ("a namespace whose name carries a hyphen", "a-1".to_string()),
        (
            "a namespace whose name is the longest admitted",
            "n".repeat(NAMESPACE_MAX_LEN),
        ),
    ] {
        let tags = json!({ "ns": name.clone() });
        let arn = namespace_arn(&router, &api_id, &name, tags.clone()).await;
        rows.push((case.to_string(), arn, tags));
    }

    // Every row's created tags are what the listing reports.
    for (case, arn, created_with) in &rows {
        let listed = list_tags(&router, arn).await;
        assert_eq!(
            (listed.status, &listed.body["tags"]),
            (200, created_with),
            "case {case:?} ({arn}): expected 200 with the tags it was created with, got {listed:?}"
        );
    }

    // A write through the route reaches the resource its ARN names and no other.
    for (index, (case, arn, _created_with)) in rows.iter().enumerate() {
        let written = tag_resource(&router, arn, json!({ "owner": "platform" })).await;
        assert_eq!(
            written.status, TAG_WRITE_STATUS,
            "case {case:?} ({arn}): expected TagResource {TAG_WRITE_STATUS}, got {written:?}"
        );

        for (other, (other_case, other_arn, other_created)) in rows.iter().enumerate() {
            let listed = list_tags(&router, other_arn).await;
            let mut expected = other_created.clone();
            if other <= index {
                expected["owner"] = json!("platform");
            }
            assert_eq!(
                (listed.status, &listed.body["tags"]),
                (200, &expected),
                "case {case:?} then {other_case:?} ({other_arn}): expected {expected}, got \
                 {listed:?}"
            );
        }
    }

    // And a removal through the route clears exactly the key it names.
    for (case, arn, created_with) in &rows {
        let removed = untag_resource(&router, arn, &["owner".to_string()]).await;
        assert_eq!(
            removed.status, TAG_WRITE_STATUS,
            "case {case:?} ({arn}): expected UntagResource {TAG_WRITE_STATUS}, got {removed:?}"
        );
        let listed = list_tags(&router, arn).await;
        assert_eq!(
            (listed.status, &listed.body["tags"]),
            (200, created_with),
            "case {case:?} ({arn}): expected the removal to leave the created tags, got {listed:?}"
        );
    }
}

/// Rule 2: an ARN of a served form that names no resource is the absent resource, on every
/// operation of the route.
#[tokio::test]
async fn an_issued_form_naming_no_resource_is_not_found() {
    let router = router();
    let api_id = create_event_api(&router, "Events_absent_namespaces", AUTH_TYPES).await;
    let kept = namespace_arn(&router, &api_id, "kept", json!({ "ns": "kept" })).await;
    let doomed = namespace_arn(&router, &api_id, "doomed", json!({ "ns": "doomed" })).await;
    let deleted = call(
        &router,
        "DELETE",
        &format!("/v2/apis/{api_id}/channelNamespaces/doomed"),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case delete the namespace: expected 200, got {deleted:?}"
    );

    let cases = [
        (
            "a namespace name this Api never held",
            format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{api_id}/channelNamespace/absent"),
        ),
        (
            "a namespace under an Api id nothing created",
            format!(
                "arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{ABSENT_API_ID}/channelNamespace/default"
            ),
        ),
        ("a deleted namespace's own ARN", doomed),
        (
            "the namespace's name under another account",
            format!("arn:aws:appsync:{REGION}:123456789012:apis/{api_id}/channelNamespace/kept"),
        ),
    ];

    for (case, candidate) in &cases {
        let listed = list_tags(&router, candidate).await;
        assert_eq!(
            (
                listed.status,
                listed.error_type.as_str(),
                listed.message.as_str()
            ),
            (404, "NotFoundException", RESOURCE_NOT_FOUND),
            "case {case:?} ({candidate}): expected 404 NotFoundException \
             {RESOURCE_NOT_FOUND:?}, got {listed:?}"
        );
        let written = tag_resource(&router, candidate, json!({ "owner": "platform" })).await;
        assert_eq!(
            (written.status, written.error_type.as_str()),
            (404, "NotFoundException"),
            "case {case:?} ({candidate}): expected TagResource to report the absent resource, \
             got {written:?}"
        );
        let removed = untag_resource(&router, candidate, &["ns".to_string()]).await;
        assert_eq!(
            (removed.status, removed.error_type.as_str()),
            (404, "NotFoundException"),
            "case {case:?} ({candidate}): expected UntagResource to report the absent resource, \
             got {removed:?}"
        );
    }

    // The namespace that IS there is untouched, so the refusals above are about the ARN
    // rather than about the route.
    let listed = list_tags(&router, &kept).await;
    assert_eq!(
        (listed.status, &listed.body["tags"]),
        (200, &json!({ "ns": "kept" })),
        "case the stored namespace: expected its tags unchanged, got {listed:?}"
    );
}

/// Rule 3: a value of no issued form is refused as a malformed ARN, on every operation of
/// the route.
#[tokio::test]
async fn a_value_of_no_issued_form_is_a_bad_request() {
    let router = router();
    // A GraphQL API exists throughout, so nothing below is refused for want of a store.
    let api_id = create_api(&router, "tagging-forms").await;

    let cases = [
        (
            "a namespace path with no name",
            format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{api_id}/channelNamespace/"),
            INVALID_RESOURCE_ARN,
        ),
        (
            "a namespace name one character past the longest admitted",
            format!(
                "arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{api_id}/channelNamespace/{}",
                "n".repeat(NAMESPACE_MAX_LEN + 1)
            ),
            INVALID_RESOURCE_ARN,
        ),
        (
            "a namespace name outside the expression its name is written in",
            format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{api_id}/channelNamespace/bad name"),
            INVALID_RESOURCE_ARN,
        ),
        (
            "a child path this service publishes no tag map for",
            format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apis/{api_id}/datasources/orders"),
            INVALID_RESOURCE_ARN,
        ),
        (
            "the namespace path spelled under another resource type",
            format!("arn:aws:appsync:{REGION}:{ACCOUNT}:apiz/{api_id}/channelNamespace/default"),
            INVALID_RESOURCE_ARN,
        ),
        ("no ARN at all", "not-an-arn".to_string(), INVALID_RESOURCE_ARN),
        (
            "a namespace-shaped ARN of another vendor",
            format!(
                "arn:aws:s3:{REGION}:{ACCOUNT}:apis/{api_id}/channelNamespace/default"
            ),
            EXPECTED_VENDOR,
        ),
    ];

    for (case, candidate, expected) in &cases {
        let listed = list_tags(&router, candidate).await;
        assert_eq!(
            (
                listed.status,
                listed.error_type.as_str(),
                listed.message.as_str()
            ),
            (400, "BadRequestException", *expected),
            "case {case:?} ({candidate}) on ListTagsForResource: expected 400 \
             BadRequestException {expected:?}, got {listed:?}"
        );
        let written = tag_resource(&router, candidate, json!({ "owner": "platform" })).await;
        assert_eq!(
            (
                written.status,
                written.error_type.as_str(),
                written.message.as_str()
            ),
            (400, "BadRequestException", *expected),
            "case {case:?} ({candidate}) on TagResource: expected 400 BadRequestException \
             {expected:?}, got {written:?}"
        );
        let removed = untag_resource(&router, candidate, &["owner".to_string()]).await;
        assert_eq!(
            (
                removed.status,
                removed.error_type.as_str(),
                removed.message.as_str()
            ),
            (400, "BadRequestException", *expected),
            "case {case:?} ({candidate}) on UntagResource: expected 400 BadRequestException \
             {expected:?}, got {removed:?}"
        );
    }
}
