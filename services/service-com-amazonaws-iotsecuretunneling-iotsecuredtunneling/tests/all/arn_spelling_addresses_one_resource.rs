// SPDX-License-Identifier: CC0-1.0

//! Which resource a caller-supplied `resourceArn` addresses.
//!
//! The rule, asserted over a class of strings rather than over one request: a
//! resource ARN addresses a resource through its FIELDS — the partition, the
//! namespace, the Region, the account, and the resource's type and id, written
//! in either of the `<type>/<id>` and `<type>:<id>` forms AWS writes a resource
//! in — and not through byte equality with the one string the service minted.
//! So every spelling of a live tunnel's ARN reaches that tunnel and its one tag
//! set through all three ARN-addressed operations (`TagResource`,
//! `ListTagsForResource`, `UntagResource`), while a string differing in any
//! ADDRESS field names another resource and is refused.
//!
//! Driven over the wire through the `MultiServiceRouter`, exactly as a client
//! reaches the service.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotsecuretunneling_iotsecuredtunneling::state::StoredTunnel;
use service_com_amazonaws_iotsecuretunneling_iotsecuredtunneling::{
    ComAmazonawsIotsecuretunnelingIotsecuredtunnelingState as TunnelState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the test signs for, so an ARN's Region segment is observable.
const REGION: &str = "eu-west-1";

/// The canonical emulator account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// A router carrying only this service, over the engine the test also seeds.
fn mount(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Call one operation the way an SDK does and return `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, input: &Value) -> (u16, Value) {
    let body = serde_json::to_vec(input).expect("serialize the request body");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("IoTSecuredTunneling.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/IoTSecuredTunneling/\
                 aws4_request, SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `[{"key":..,"value":..}, ..]` as a map, so a test compares tags by content.
fn tag_map(tags: Option<&Value>) -> BTreeMap<String, String> {
    tags.and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|tag| {
                    (
                        tag["key"].as_str().unwrap_or_default().to_string(),
                        tag["value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Open one tunnel in `REGION` and return `(tunnelId, tunnelArn)`.
async fn open_tunnel(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let (status, opened) = call(
        router,
        "OpenTunnel",
        &json!({
            "description": case,
            "destinationConfig": {"thingName": "thing-addressing", "services": ["SSH"]},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: OpenTunnel answered {status} {opened:?}"
    );
    (
        opened["tunnelId"].as_str().unwrap_or_default().to_string(),
        opened["tunnelArn"].as_str().unwrap_or_default().to_string(),
    )
}

/// Put a live tunnel carrying `tunnel_id` in the store, as a copy of the one
/// `seed_id` names, with the ARN this service would mint for it.
///
/// Lets the table span the id forms `TunnelId` admits, which the service's own
/// minting never produces: the id is part of the resource's ADDRESS, so the rule
/// has to hold for each of them.
fn seed_tunnel(engine: &Arc<engine::InMemoryEngine>, seed_id: &str, tunnel_id: &str) {
    let state = engine.state::<TunnelState>();
    let mut store = state.lock();
    let mut tunnel: StoredTunnel = store
        .tunnel(REGION, seed_id)
        .cloned()
        .expect("the opened tunnel was not persisted");
    tunnel.tunnel_id = tunnel_id.to_string();
    tunnel.tunnel_arn = format!("arn:aws:iot:{REGION}:{ACCOUNT}:tunnel/{tunnel_id}");
    store.insert_tunnel(REGION, tunnel);
}

/// The spellings AWS writes of the ARN of the tunnel `tunnel_id` names in
/// `REGION`: `<type>/<id>` and `<type>:<id>`, the two forms
/// `service_common::arn` — the workspace's one caller-supplied-ARN parse —
/// documents for a resource field.
fn spellings_of(tunnel_id: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "the resource in the slash form",
            format!("arn:aws:iot:{REGION}:{ACCOUNT}:tunnel/{tunnel_id}"),
        ),
        (
            "the resource in the colon form",
            format!("arn:aws:iot:{REGION}:{ACCOUNT}:tunnel:{tunnel_id}"),
        ),
    ]
}

#[tokio::test]
async fn every_spelling_of_a_live_tunnels_arn_reaches_that_tunnels_one_tag_set() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = mount(&engine);
    let (minted_id, minted_arn) = open_tunnel(&router, "the addressed tunnel").await;

    // The id forms `TunnelId` admits, from
    // `smithy_query(query="shape", shape="com.amazonaws.iotsecuretunneling#TunnelId")`:
    // `^[a-zA-Z0-9_\-+=:]{1,128}$` — so the rows walk the minted form, each
    // published length bound, and an id carrying every character class the
    // pattern admits, `:` included (which the colon-form spelling writes right
    // after the separator the type ends with).
    let ids: Vec<(&str, String)> = vec![
        ("the minted id", minted_id.clone()),
        ("the shortest id the pattern admits", "a".to_string()),
        (
            "an id using every character class the pattern admits, ':' included",
            "weird_id+with=every:char-class".to_string(),
        ),
        ("the longest id the pattern admits", "z".repeat(128)),
    ];

    for (id_label, tunnel_id) in &ids {
        if tunnel_id != &minted_id {
            seed_tunnel(&engine, &minted_id, tunnel_id);
        }
        let spellings = spellings_of(tunnel_id);
        for (written_label, written) in &spellings {
            let case = format!("{id_label} ({tunnel_id:?}) addressed by {written_label}");

            // A tag written through this spelling.
            let (status, tagged) = call(
                &router,
                "TagResource",
                &json!({"resourceArn": written, "tags": [{"key": "env", "value": written_label}]}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: TagResource through {written:?} answered {status} {tagged:?}"
            );

            // reads back through EVERY spelling of the same resource's ARN,
            // because they all address one resource and so one tag set,
            for (read_label, read) in &spellings {
                let (status, listed) = call(
                    &router,
                    "ListTagsForResource",
                    &json!({"resourceArn": read}),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: ListTagsForResource through {read_label} ({read:?}) answered \
                     {status} {listed:?}"
                );
                let listed_tags = tag_map(listed.get("tags"));
                assert_eq!(
                    listed_tags.get("env").map(String::as_str),
                    Some(*written_label),
                    "case {case:?}: ListTagsForResource through {read_label} reported \
                     {listed_tags:?}"
                );
            }

            // and through the tunnel's own read, which reports the one stored
            // set inside the `Tunnel` shape.
            let (status, described) =
                call(&router, "DescribeTunnel", &json!({"tunnelId": tunnel_id})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeTunnel answered {status} {described:?}"
            );
            let described_tags = tag_map(described["tunnel"].get("tags"));
            assert_eq!(
                described_tags.get("env").map(String::as_str),
                Some(*written_label),
                "case {case:?}: DescribeTunnel reported tags {described_tags:?}"
            );

            // The removal addresses the same one set, whichever spelling asks:
            // the key goes through the OTHER spelling and the tunnel carries no
            // tags afterwards.
            let removing = spellings
                .iter()
                .find(|(label, _)| label != written_label)
                .map_or_else(|| written.clone(), |(_, other)| other.clone());
            let (status, untagged) = call(
                &router,
                "UntagResource",
                &json!({"resourceArn": removing, "tagKeys": ["env"]}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: UntagResource through {removing:?} answered {status} {untagged:?}"
            );
            let (status, described) =
                call(&router, "DescribeTunnel", &json!({"tunnelId": tunnel_id})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeTunnel answered {status} {described:?}"
            );
            let described_tags = tag_map(described["tunnel"].get("tags"));
            assert!(
                described_tags.is_empty(),
                "case {case:?}: the removal through {removing:?} must leave no tags, got \
                 {described_tags:?}"
            );
        }
    }

    // The tunnel the create minted is still addressed by the ARN it handed
    // back, untouched by every other spelling above.
    let (status, listed) = call(
        &router,
        "ListTagsForResource",
        &json!({"resourceArn": minted_arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {minted_arn:?}: the minted ARN must still address its tunnel, answered {status} \
         {listed:?}"
    );
}

#[tokio::test]
async fn a_string_differing_in_an_address_field_names_another_resource() {
    // The companion of the rule: only the resource SEPARATOR is a spelling of
    // one address. Every other field — the partition, the namespace, the
    // Region, the account, the resource type and the id — is part of the
    // address, so a string differing in one of them names another resource and
    // reaches none of this tunnel's tags.
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = mount(&engine);
    let (tunnel_id, minted_arn) = open_tunnel(&router, "the live tunnel").await;

    let (status, tagged) = call(
        &router,
        "TagResource",
        &json!({"resourceArn": minted_arn, "tags": [{"key": "env", "value": "test"}]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "TagResource through the minted ARN answered {status} {tagged:?}"
    );

    let rows: Vec<(&str, String)> = vec![
        (
            "another Region",
            format!("arn:aws:iot:us-west-2:{ACCOUNT}:tunnel/{tunnel_id}"),
        ),
        (
            "another account",
            format!("arn:aws:iot:{REGION}:111111111111:tunnel/{tunnel_id}"),
        ),
        (
            "another partition",
            format!("arn:aws-cn:iot:{REGION}:{ACCOUNT}:tunnel/{tunnel_id}"),
        ),
        (
            "another vendor namespace",
            format!("arn:aws:s3:{REGION}:{ACCOUNT}:tunnel/{tunnel_id}"),
        ),
        (
            "another resource type",
            format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/{tunnel_id}"),
        ),
        (
            "another id, in the colon form",
            format!("arn:aws:iot:{REGION}:{ACCOUNT}:tunnel:not-{tunnel_id}"),
        ),
    ];

    for (label, arn) in &rows {
        for (operation, input) in [
            ("ListTagsForResource", json!({"resourceArn": arn})),
            (
                "TagResource",
                json!({"resourceArn": arn, "tags": [{"key": "env", "value": "other"}]}),
            ),
            (
                "UntagResource",
                json!({"resourceArn": arn, "tagKeys": ["env"]}),
            ),
        ] {
            let case = format!("{label} ({arn:?}) through {operation}");
            let (status, body) = call(&router, operation, &input).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the refusal of a resource this endpoint does not hold, \
                 answered {status} {body:?}"
            );
            assert_eq!(
                body["__type"].as_str(),
                Some("ResourceNotFoundException"),
                "case {case:?}: answered {body:?}"
            );
        }
    }

    // None of those strings read, wrote or removed the live tunnel's tags.
    let (status, listed) = call(
        &router,
        "ListTagsForResource",
        &json!({"resourceArn": minted_arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {minted_arn:?}: ListTagsForResource answered {status} {listed:?}"
    );
    assert_eq!(
        tag_map(listed.get("tags")),
        BTreeMap::from([("env".to_string(), "test".to_string())]),
        "case {minted_arn:?}: expected only the tag written through the minted ARN, got {:?}",
        listed["tags"]
    );
}
