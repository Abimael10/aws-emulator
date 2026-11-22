// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW this service classifies a namespace create that asks for a
//! `Name` another namespace already answers to.
//!
//! RULE: a name is held by at most one namespace, whichever create stored it, so a create
//! asking for a held name is a NAME conflict — refused with the error the model declares
//! for exactly that (`NamespaceAlreadyExists`, HTTP 400), carrying the id of the namespace
//! that owns the name and the `CreatorRequestId` of the create that made it. The
//! duplicate-request class (409) is a conflict between OPERATIONS on one resource (a second
//! delete of a namespace whose delete is still running, which `async_namespace_operations`
//! covers) and is never how a name collision is reported: the two tell a client different
//! things — "poll your own retry" against "someone else owns this name, pick another".
//!
//! The rows differ along what the rule generalises over: which create took the name first,
//! which create asks for it, whether the asking call carries a `CreatorRequestId` at all
//! and whether the winner did, and whether the two calls are made one after the other or
//! concurrently (the race the rule is about). The last table is the control: the refusal
//! stores nothing, and a retry of the winner's OWN request is still a replay rather than a
//! conflict.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production; the harness lives in `cloud_map_core`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloud_map_core::{call, emulator, ok, refusal};

/// The VPC a private DNS namespace is created for; nothing in these cases turns on it.
const VPC: &str = "vpc-0a1b2c3d";

/// The error the model declares for a name another namespace already answers to.
const NAMESPACE_ALREADY_EXISTS: &str = "NamespaceAlreadyExists";

/// The sentence that refusal carries, which reports the CLASS of conflict and no request
/// value: the members of the shape identify the namespace that owns the name.
const ALREADY_EXISTS_MESSAGE: &str = "Namespace already exists.";

/// The members a namespace create of `operation` needs beside its `Name`.
fn body(operation: &str, name: &str, token: Option<&str>) -> Value {
    let mut body = json!({ "Name": name });
    if operation == "CreatePrivateDnsNamespace" {
        body["Vpc"] = json!(VPC);
    }
    if let Some(token) = token {
        body["CreatorRequestId"] = json!(token);
    }
    body
}

/// The id of the namespace a create's operation made.
async fn created_namespace(router: &MultiServiceRouter, created: &Value) -> String {
    let operation_id = created["OperationId"]
        .as_str()
        .expect("a namespace create returns an OperationId")
        .to_string();
    ok(router, "GetOperation", json!({ "OperationId": operation_id })).await["Operation"]["Targets"]
        ["NAMESPACE"]
        .as_str()
        .expect("the CREATE_NAMESPACE operation targets the namespace it created")
        .to_string()
}

/// A name of the form each create's own `Name` pattern admits, so a row tests the conflict
/// rule and not a name grammar: the public DNS create's pattern is a label pattern.
fn name_for(operation: &str, stem: &str) -> String {
    match operation {
        "CreateHttpNamespace" => stem.to_string(),
        _ => format!("{stem}.example.com"),
    }
}

/// The three creates, every one of which takes a name out of the one name index.
const CREATES: [&str; 3] = [
    "CreateHttpNamespace",
    "CreatePublicDnsNamespace",
    "CreatePrivateDnsNamespace",
];

/// RULE: whichever create holds the name and whichever create asks for it, the asking call
/// is refused with `NamespaceAlreadyExists` naming the namespace that owns the name — over
/// every (winner, loser) pair of the three creates, and over the tokens each call carries.
#[tokio::test]
async fn a_name_another_namespace_owns_is_refused_as_a_namespace_conflict() {
    let router = emulator();

    // (case, the winner's CreatorRequestId, the loser's)
    let tokens: [(&str, Option<&str>, Option<&str>); 3] = [
        ("neither call carries a token", None, None),
        ("both carry their own token", Some("crid-a"), Some("crid-b")),
        // An SDK fills a token in for the caller, so the winner carrying one and the loser
        // none is a real pairing rather than a curiosity.
        ("only the winner carries one", Some("crid-only"), None),
    ];

    for (index, (case, winner_token, loser_token)) in tokens.into_iter().enumerate() {
        for took in CREATES {
            for asks in CREATES {
                let stem = format!("conflict-{index}-{}-{}", short(took), short(asks));
                // The name has to satisfy BOTH creates' own `Name` patterns, so a dotted
                // label name is used whenever either side is a DNS create.
                let name = name_for(
                    if took == "CreateHttpNamespace" { asks } else { took },
                    &stem,
                );

                let created = ok(&router, took, body(took, &name, winner_token)).await;
                let owner = created_namespace(&router, &created).await;

                let (status, answer) = call(&router, asks, body(asks, &name, loser_token)).await;
                let code = answer["__type"].as_str().unwrap_or_default().to_string();
                assert!(
                    status == 400 && code.ends_with(NAMESPACE_ALREADY_EXISTS),
                    "case {case:?}: {asks} on a name {took} owns must be refused as the name \
                     conflict, got {status} {answer}"
                );
                assert_eq!(
                    answer["Message"],
                    json!(ALREADY_EXISTS_MESSAGE),
                    "case {case:?}: expected the conflict sentence, got {answer}"
                );
                assert_eq!(
                    answer["NamespaceId"],
                    json!(owner),
                    "case {case:?}: the refusal names the namespace that owns {name:?}, got \
                     {answer}"
                );
                assert_eq!(
                    answer["CreatorRequestId"],
                    winner_token.map_or(Value::Null, |token| json!(token)),
                    "case {case:?}: the refusal reports the CreatorRequestId that created \
                     {name:?}, got {answer}"
                );
                // The duplicate-operation member of the OTHER conflict class is not what
                // this refusal carries.
                assert_eq!(
                    answer["DuplicateOperationId"],
                    Value::Null,
                    "case {case:?}: a name conflict names no operation, got {answer}"
                );
            }
        }
    }
}

/// RULE, under the race it is about: when two creates for ONE name are in flight together,
/// exactly one is served and the other is refused as the name conflict naming the winner.
#[tokio::test]
async fn one_of_two_concurrent_creates_for_one_name_wins_and_the_other_is_the_conflict() {
    let router = emulator();

    for (case, operation) in [
        ("two HTTP creates", "CreateHttpNamespace"),
        ("two private DNS creates", "CreatePrivateDnsNamespace"),
    ] {
        let name = name_for(operation, &format!("racer-{}", short(operation)));
        // Distinct `CreatorRequestId`s: each racer is its own request, so neither call is a
        // retry of the other.
        let (first, second) = tokio::join!(
            call(&router, operation, body(operation, &name, Some("racer-a"))),
            call(&router, operation, body(operation, &name, Some("racer-b"))),
        );

        let answers = [first, second];
        let served: Vec<&(u16, Value)> = answers
            .iter()
            .filter(|(status, _)| *status == 200)
            .collect();
        assert_eq!(
            served.len(),
            1,
            "case {case:?}: exactly one racer for {name:?} is served, got {answers:?}"
        );
        let owner = created_namespace(&router, &served[0].1).await;

        for (status, answer) in answers.iter().filter(|(status, _)| *status != 200) {
            let code = answer["__type"].as_str().unwrap_or_default();
            assert!(
                *status == 400 && code.ends_with(NAMESPACE_ALREADY_EXISTS),
                "case {case:?}: the refused racer carries the name conflict, got {status} \
                 {answer}"
            );
            assert_eq!(
                answer["NamespaceId"],
                json!(owner),
                "case {case:?}: the refused racer names the namespace that won, got {answer}"
            );
        }
    }
}

/// RULE, controls: the refusal CHANGES NOTHING — the namespace that owns the name still
/// reads back as it was and no second namespace was stored — and a retry of the winner's
/// own request is a replay of its operation rather than a conflict, since it asks for the
/// namespace that is already there.
#[tokio::test]
async fn the_conflict_stores_nothing_and_the_winners_own_retry_still_replays() {
    let router = emulator();

    let name = "control-ns.example.com";
    let created = ok(
        &router,
        "CreatePublicDnsNamespace",
        body("CreatePublicDnsNamespace", name, Some("control-token")),
    )
    .await;
    let owner = created_namespace(&router, &created).await;
    let before = ok(&router, "GetNamespace", json!({ "Id": owner })).await;

    // A different token on the same name is the conflict ...
    let (status, code, _) = refusal(
        &router,
        "CreatePublicDnsNamespace",
        body("CreatePublicDnsNamespace", name, Some("another-token")),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, NAMESPACE_ALREADY_EXISTS),
        "a second creator of {name:?} is refused, got {status} {code}"
    );

    // ... and the winner's OWN request is still idempotent: same token, same name, so the
    // operation the first call filed is replayed.
    let replayed = ok(
        &router,
        "CreatePublicDnsNamespace",
        body("CreatePublicDnsNamespace", name, Some("control-token")),
    )
    .await;
    assert_eq!(
        replayed["OperationId"], created["OperationId"],
        "the winner's own retry replays its operation, got {replayed} after {created}"
    );

    let after = ok(&router, "GetNamespace", json!({ "Id": owner })).await;
    assert_eq!(
        after["Namespace"], before["Namespace"],
        "the refusal left the stored namespace untouched, got {after} after {before}"
    );
    let listed = ok(&router, "ListNamespaces", json!({})).await;
    let holding: Vec<&Value> = listed["Namespaces"]
        .as_array()
        .expect("ListNamespaces reports its namespaces")
        .iter()
        .filter(|summary| summary["Name"] == json!(name))
        .collect();
    assert_eq!(
        holding.len(),
        1,
        "exactly one namespace holds {name:?}, got {listed}"
    );
}

/// A short tag for one create, so a row's generated name stays inside the published
/// `@length` maximum of every namespace name (253 characters for a DNS namespace).
fn short(operation: &str) -> &str {
    match operation {
        "CreateHttpNamespace" => "http",
        "CreatePublicDnsNamespace" => "public",
        "CreatePrivateDnsNamespace" => "private",
        other => other,
    }
}
