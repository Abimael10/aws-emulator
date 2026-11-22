// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW this service classifies a namespace create that asks for a
//! `Name` another namespace already answers to.
//!
//! RULE: a name is held by at most one namespace, whichever create stored it, so a create
//! asking for a held name is refused — and WHICH refusal it earns follows the create that
//! is asking. The two DNS creates claim a domain name, so a name another namespace holds is
//! a NAME conflict for them: `NamespaceAlreadyExists` (HTTP 400), carrying the id of the
//! namespace that owns the name and the `CreatorRequestId` of the create that made it. The
//! HTTP create claims no DNS name — an HTTP namespace holds no hosted zone — and recognises
//! a repeat of ITSELF by the create operation already on file for the name: a name another
//! HTTP namespace holds is the duplicate-REQUEST class (`DuplicateRequest`, modelled
//! `@httpError 409` and answered with this service's one client status like every other
//! client refusal of it), carrying that operation's id, while a name a DNS namespace holds
//! was filed by no create
//! of its own and stays the name conflict. The two tell a client different things — "poll
//! the operation that already did this" against "someone else owns this name, pick
//! another".
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

use crate::async_namespace_operations::set_operation_due;
use crate::cloud_map_core::{call, emulator, emulator_with_engine, ok, refusal};

/// The VPC a private DNS namespace is created for; nothing in these cases turns on it.
const VPC: &str = "vpc-0a1b2c3d";

/// The error the model declares for a name another namespace already answers to.
const NAMESPACE_ALREADY_EXISTS: &str = "NamespaceAlreadyExists";

/// The error the model declares for a request that repeats an operation of its own: what
/// the HTTP create answers a name another HTTP namespace already holds with.
const DUPLICATE_REQUEST: &str = "DuplicateRequest";

/// The create that claims no DNS name, and so reports a repeat of itself as the duplicate
/// request rather than as the name conflict.
const HTTP_CREATE: &str = "CreateHttpNamespace";

/// The ONE status every client-class refusal of this service answers, whatever `@httpError`
/// the model pins on the shape (`client_error_wire_shape` states that rule).
const CLIENT_STATUS: u16 = 400;

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
/// is refused — as the duplicate request naming the create operation it repeats when an
/// HTTP create asks for a name another HTTP namespace holds, and as the name conflict
/// naming the namespace that owns the name otherwise — over every (winner, loser) pair of
/// the three creates, and over the tokens each call carries.
#[tokio::test]
async fn a_name_another_namespace_owns_is_refused_for_the_create_that_asks() {
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
                let name = name_for(if took == HTTP_CREATE { asks } else { took }, &stem);

                let created = ok(&router, took, body(took, &name, winner_token)).await;
                let filed = created["OperationId"]
                    .as_str()
                    .expect("a namespace create returns an OperationId")
                    .to_string();
                let owner = created_namespace(&router, &created).await;

                let (status, answer) = call(&router, asks, body(asks, &name, loser_token)).await;
                let code = answer["__type"].as_str().unwrap_or_default().to_string();

                if (took, asks) == (HTTP_CREATE, HTTP_CREATE) {
                    assert!(
                        status == CLIENT_STATUS && code.ends_with(DUPLICATE_REQUEST),
                        "case {case:?}: {asks} on a name {took} owns must be refused as the \
                         duplicate request, got {status} {answer}"
                    );
                    assert_eq!(
                        answer["DuplicateOperationId"],
                        json!(filed),
                        "case {case:?}: the refusal names the create operation on file for \
                         {name:?}, got {answer}"
                    );
                    // The sentence names that operation; whether the clock has settled it
                    // yet is what `the_duplicate_request_reports_what_became_of_the_operation`
                    // pins, so only the id is asserted here.
                    let message = answer["Message"].as_str().unwrap_or_default();
                    assert!(
                        message.contains(&filed),
                        "case {case:?}: the sentence names the operation it duplicates, got \
                         {answer}"
                    );
                    // The members of the OTHER conflict class are not what this refusal
                    // carries.
                    assert_eq!(
                        (&answer["NamespaceId"], &answer["CreatorRequestId"]),
                        (&Value::Null, &Value::Null),
                        "case {case:?}: a duplicate request names no namespace, got {answer}"
                    );
                    continue;
                }

                assert!(
                    status == CLIENT_STATUS && code.ends_with(NAMESPACE_ALREADY_EXISTS),
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
/// exactly one is served and the other is refused — as the duplicate request for the HTTP
/// create, naming the operation the winner filed, and as the name conflict naming the
/// winner for a DNS create.
#[tokio::test]
async fn one_of_two_concurrent_creates_for_one_name_wins_and_the_other_is_refused() {
    let router = emulator();

    for (case, operation) in [
        ("two HTTP creates", HTTP_CREATE),
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
        let filed = served[0].1["OperationId"]
            .as_str()
            .expect("the served racer returns an OperationId")
            .to_string();
        let owner = created_namespace(&router, &served[0].1).await;

        for (status, answer) in answers.iter().filter(|(status, _)| *status != 200) {
            let code = answer["__type"].as_str().unwrap_or_default();
            if operation == HTTP_CREATE {
                assert!(
                    *status == CLIENT_STATUS && code.ends_with(DUPLICATE_REQUEST),
                    "case {case:?}: the refused racer carries the duplicate request, got \
                     {status} {answer}"
                );
                assert_eq!(
                    answer["DuplicateOperationId"],
                    json!(filed),
                    "case {case:?}: the refused racer names the operation the winner filed, \
                     got {answer}"
                );
                continue;
            }
            assert!(
                *status == CLIENT_STATUS && code.ends_with(NAMESPACE_ALREADY_EXISTS),
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

/// RULE: the duplicate request reports WHAT BECAME of the operation it names — still
/// running while the clock has not settled it, settled once it has — so a caller that polls
/// that operation knows whether to wait for it.
///
/// The due instant of the winner's operation is MOVED rather than waited for, so the test
/// decides which side of the transition each row is on without depending on the wall clock.
#[tokio::test]
async fn the_duplicate_request_reports_what_became_of_the_operation() {
    let (router, engine) = emulator_with_engine();

    // (case, whether the clock already owes the operation its terminal status, the tail of
    // the sentence that states it)
    let rows: [(&str, bool, &str); 2] = [
        ("the create is still running", false, "is in progress"),
        ("the create has settled", true, "has completed"),
    ];

    for (index, (case, settled, outcome)) in rows.into_iter().enumerate() {
        let name = format!("owed-{index}");
        let created = ok(&router, HTTP_CREATE, body(HTTP_CREATE, &name, None)).await;
        let filed = created["OperationId"]
            .as_str()
            .expect("a namespace create returns an OperationId")
            .to_string();
        set_operation_due(&engine, &filed, settled);

        let (status, code, message) =
            refusal(&router, HTTP_CREATE, body(HTTP_CREATE, &name, None)).await;
        assert_eq!(
            (status, code.as_str()),
            (CLIENT_STATUS, DUPLICATE_REQUEST),
            "case {case:?}: expected the duplicate request, got {status} {code} {message:?}"
        );
        assert_eq!(
            message.as_deref(),
            Some(
                format!("Another operation of type {HTTP_CREATE} and id {filed} {outcome}")
                    .as_str()
            ),
            "case {case:?}: expected the sentence naming the operation and {outcome:?}, got \
             {message:?}"
        );
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
