// SPDX-License-Identifier: CC0-1.0

//! The network record the two readers of a network answer with, driven over HTTP against
//! one running emulator.
//!
//! The rules under test, each asserted over a SET of networks rather than over one:
//!
//! * a read of a network reports the network's TAG SET, the service-set entry naming the
//!   network's own ARN among them — the same ARN the record reports as `networkArn`, so the
//!   value is derived from the account, region and id of the network rather than fixed;
//! * a network that exists and is usable reports the one standing a live network carries,
//!   and reports the SSO redirect URI migration as completed, whatever else differs about
//!   it;
//! * a listed network carries a FIXED member set: a member the list form serializes is
//!   present whatever the network holds, as an explicit JSON null where it holds nothing,
//!   which is what tells a client that distinguishes "present and null" from "absent" apart
//!   from a record of another shape;
//! * both readers answer from one record, so a member they both report cannot disagree.
//!
//! Every case goes through the real router — route resolution, restJson1 deserialization,
//! modelled-constraint validation, the handler and response serialization — so what it
//! asserts is the wire behaviour a client sees.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The tag key the service sets itself on every network, holding the network's own ARN.
/// RECORDED behaviour: the key is fixed, the value is the network's ARN.
const RESOURCE_ARN_TAG: &str = "resourceArn";

/// `Network$standing` a live, usable network reports. RECORDED behaviour: the member is
/// documented with no value list, and the wire reports this value for such a network.
const LIVE_STANDING: i64 = 2;

/// `Network$migrationState` a network this API created reports: 2 is "completed" of the
/// member's documented value list (0 not started, 1 in progress, 2 completed), which is
/// what the recorded wire reports for a newly created network.
const COMPLETED_MIGRATION_STATE: i64 = 2;

/// A fresh emulator holding only this service, over its own engine, so one test's networks
/// are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the shape of a served record is.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_wickr_wickradminapi::EmulatedService::new(
        engine,
    ));
    router
}

/// One request/response round trip: the status and the parsed body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "admin.wickr.us-east-1.amazonaws.com")
        // The SigV4 credential scope is how the router picks the service, and the region
        // the network's ARN is built in.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260102/us-east-1/wickr/aws4_request, \
             SignedHeaders=host, Signature=00",
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("build the request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, body)
}

/// One network to create: the case name and the `CreateNetwork` request body.
struct Creating {
    case: &'static str,
    request: Value,
}

/// The networks every rule below is asserted over. They differ along what a network can
/// differ along at creation: its access level, whether it asked for the premium free trial
/// (the one member that decides whether `freeTrialExpiration` holds a value), whether it
/// names a customer managed key, and the name itself.
fn creating() -> Vec<Creating> {
    vec![
        Creating {
            case: "standard, no trial, no key",
            request: json!({ "networkName": "plain-net", "accessLevel": "STANDARD" }),
        },
        Creating {
            case: "premium with the free trial",
            request: json!({
                "networkName": "trial net",
                "accessLevel": "PREMIUM",
                "enablePremiumFreeTrial": true
            }),
        },
        Creating {
            case: "premium, trial declined explicitly",
            request: json!({
                "networkName": "no_trial-net",
                "accessLevel": "PREMIUM",
                "enablePremiumFreeTrial": false
            }),
        },
        Creating {
            case: "standard with a customer managed key",
            request: json!({
                "networkName": "keyed-net",
                "accessLevel": "STANDARD",
                "encryptionKeyArn": "arn:aws:kms:us-east-1:111122223333:key/abcd"
            }),
        },
        Creating {
            case: "the shortest name the service accepts",
            request: json!({ "networkName": "abcd", "accessLevel": "STANDARD" }),
        },
    ]
}

/// Create every network of [`creating`] and answer the `(case, networkId)` pairs.
async fn create_all(router: &MultiServiceRouter) -> Vec<(&'static str, String)> {
    let mut created = Vec::new();
    for Creating { case, request } in creating() {
        let (status, body) = call(router, "POST", "/networks", Some(request)).await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: CreateNetwork expected a success, got {status} with {body}"
        );
        let network_id = body["networkId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no networkId in {body}"))
            .to_string();
        created.push((case, network_id));
    }
    created
}

/// The `networks` list of a `ListNetworks` answer, keyed by `networkId`, so a case can
/// reach its own record whatever order the listing came back in.
async fn listed(router: &MultiServiceRouter) -> Map<String, Value> {
    let (status, body) = call(router, "GET", "/networks", None).await;
    assert_eq!(
        status, 200,
        "ListNetworks: expected 200, got {status} with {body}"
    );
    let networks = body["networks"]
        .as_array()
        .unwrap_or_else(|| panic!("ListNetworks: expected a networks list, got {body}"))
        .clone();
    networks
        .into_iter()
        .map(|network| {
            let id = network["networkId"]
                .as_str()
                .unwrap_or_else(|| panic!("ListNetworks: a listed network has no id: {network}"))
                .to_string();
            (id, network)
        })
        .collect()
}

/// RULE: a read of a network reports the network's tag set, holding the service-set entry
/// that names the network's own ARN.
#[tokio::test]
async fn a_read_of_a_network_reports_its_tag_set() {
    let router = emulator();
    let created = create_all(&router).await;
    let listing = listed(&router).await;

    for (case, network_id) in &created {
        let (status, read) = call(&router, "GET", &format!("/networks/{network_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetNetwork expected 200, got {status} with {read}"
        );
        let listed_record = listing
            .get(network_id)
            .unwrap_or_else(|| panic!("case {case:?}: {network_id} is missing from the listing"));

        for (reader, record) in [("GetNetwork", &read), ("ListNetworks", listed_record)] {
            let arn = record["networkArn"].as_str().unwrap_or_default();
            let tagged = record["tags"][RESOURCE_ARN_TAG].as_str();
            assert_eq!(
                tagged,
                Some(arn),
                "case {case:?} via {reader}: expected the {RESOURCE_ARN_TAG} tag to hold \
                 the network's own ARN {arn:?}, got {tagged:?} in {record}"
            );
            // The ARN the tag carries is derived from the network it names, not a fixed
            // value: it ends in this network's own id.
            assert!(
                arn.ends_with(&format!("network/{network_id}")),
                "case {case:?} via {reader}: expected an ARN naming network/{network_id}, got \
                 {arn:?}"
            );
        }
    }

    // Two networks never share a tag value, since each tag holds its own network's ARN.
    let mut arns: Vec<String> = Vec::new();
    for (case, network_id) in &created {
        let record = listing.get(network_id).expect("the listed network");
        let arn = record["tags"][RESOURCE_ARN_TAG]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no tag in {record}"))
            .to_string();
        assert!(
            !arns.contains(&arn),
            "case {case:?}: every network's tag names its own ARN, but {arn:?} was seen twice"
        );
        arns.push(arn);
    }
}

/// RULE: a network that exists and is usable reports the standing of a live network and
/// reports the SSO redirect URI migration as completed, whatever else differs about it.
#[tokio::test]
async fn a_live_network_reports_its_standing_and_migration_state() {
    let router = emulator();
    let created = create_all(&router).await;
    let listing = listed(&router).await;

    for (case, network_id) in &created {
        let (status, read) = call(&router, "GET", &format!("/networks/{network_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetNetwork expected 200, got {status} with {read}"
        );
        let listed_record = listing
            .get(network_id)
            .unwrap_or_else(|| panic!("case {case:?}: {network_id} is missing from the listing"));

        for (reader, record) in [("GetNetwork", &read), ("ListNetworks", listed_record)] {
            let standing = record["standing"].as_i64();
            assert_eq!(
                standing,
                Some(LIVE_STANDING),
                "case {case:?} via {reader}: expected standing {LIVE_STANDING}, got \
                 {standing:?} in {record}"
            );
            let migration = record["migrationState"].as_i64();
            assert_eq!(
                migration,
                Some(COMPLETED_MIGRATION_STATE),
                "case {case:?} via {reader}: expected migrationState \
                 {COMPLETED_MIGRATION_STATE}, got {migration:?} in {record}"
            );
        }
    }
}

/// RULE: a listed network carries a fixed member set — a member the list form serializes is
/// present whatever the network holds, as an explicit null where it holds nothing.
#[tokio::test]
async fn a_listed_network_carries_its_unset_members_as_explicit_nulls() {
    let router = emulator();
    let created = create_all(&router).await;
    let listing = listed(&router).await;

    // The members the list form always serializes. `freeTrialExpiration` is the one a
    // network can hold a value for (the premium free trial), so it exercises both halves of
    // the rule; nothing in this API configures message translation, so the other two are
    // null for every network.
    let always_serialized = [
        "freeTrialExpiration",
        "translationStatus",
        "translationIamRoleArn",
    ];

    for (case, network_id) in &created {
        let record = listing
            .get(network_id)
            .unwrap_or_else(|| panic!("case {case:?}: {network_id} is missing from the listing"));
        let object = record
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected a network object, got {record}"));
        for member in always_serialized {
            assert!(
                object.contains_key(member),
                "case {case:?}: expected {member} present on the listed record, got {record}"
            );
        }
        for member in ["translationStatus", "translationIamRoleArn"] {
            assert!(
                object[member].is_null(),
                "case {case:?}: expected {member} serialized as null, got {}",
                object[member]
            );
        }
    }

    // The one member of that set a network can hold a value for: present and null where the
    // network has no trial, present and a string where it does.
    let trialling = created
        .iter()
        .find(|(case, _)| *case == "premium with the free trial")
        .expect("the free-trial case");
    let record = listing.get(&trialling.1).expect("the listed network");
    assert!(
        record["freeTrialExpiration"].is_string(),
        "case {:?}: expected freeTrialExpiration to hold the expiry, got {record}",
        trialling.0
    );
    for case in [
        "standard, no trial, no key",
        "premium, trial declined explicitly",
    ] {
        let (_, network_id) = created
            .iter()
            .find(|(name, _)| *name == case)
            .unwrap_or_else(|| panic!("case {case:?} was not created"));
        let record = listing.get(network_id).expect("the listed network");
        assert!(
            record["freeTrialExpiration"].is_null(),
            "case {case:?}: expected freeTrialExpiration present and null, got {record}"
        );
    }
}

/// RULE: both readers answer from one record, so every member they both report agrees.
#[tokio::test]
async fn the_two_readers_of_a_network_agree() {
    let router = emulator();
    let created = create_all(&router).await;
    let listing = listed(&router).await;

    for (case, network_id) in &created {
        let (status, read) = call(&router, "GET", &format!("/networks/{network_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetNetwork expected 200, got {status} with {read}"
        );
        let listed_record = listing
            .get(network_id)
            .unwrap_or_else(|| panic!("case {case:?}: {network_id} is missing from the listing"));
        let read_object = read
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected an object, got {read}"));
        for (member, value) in read_object {
            let listed_value = &listed_record[member];
            assert_eq!(
                value, listed_value,
                "case {case:?}: the two readers disagree about {member}: GetNetwork reports \
                 {value}, ListNetworks reports {listed_value}"
            );
        }
    }
}
