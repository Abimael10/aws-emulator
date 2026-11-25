// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ONE state precondition both tagging WRITES hold a caller to.
//!
//! The rule under test: whether a resource's state admits a change to its tags is one rule of
//! this service, not one per endpoint — so `TagResource` and `UntagResource`, asked about the
//! same resource in the same state, answer the SAME refusal: the same HTTP status, the same
//! error code, the same `Reason`, and the same sentence, which names the resource, the
//! connector whose status governs it and that status. Both writes are decided and worded by
//! the store's own precondition
//! ([`scep::state::ComAmazonawsPcaconnectorscepPcaconnectorscepState::tag_write_refusal`]), so
//! neither endpoint can refuse a state the other accepts, or state one precondition in two
//! sentences.
//!
//! Each row drives BOTH endpoints over the same resource: before the teardown is asked for,
//! where both are accepted, and inside the teardown window, where both are refused — so the
//! agreement is over an answer that is actually reached and is not vacuous.
//!
//! The second test here widens that to the rule those sentences are one instance of: whichever
//! operation of this service refuses a well-formed request because a resource's STATUS does not
//! admit the call — the `ValidationException` whose `Reason` is `INVALID_STATE` — words it
//! inside ONE scaffold, the opening `"Validation error detected: "` and a terminating `.`
//! around the clause that says which precondition failed. Its rows span the operations that
//! have such a precondition and both resource kinds, so a tag write cannot carry a sentence
//! shaped differently from a create's.
//!
//! The rows differ along what the rule generalises over: which of the two taggable resource
//! kinds the write names (a connector, and a challenge hanging off one, which the sentence
//! words differently), the status the connector's delete was accepted from, how many tags the
//! resource already carried, and how many entries or keys the refused call names.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{self, label, new_router, Answer};
use service_com_amazonaws_pcaconnectorscep_pcaconnectorscep as scep;

/// A Region that is not the emulator default, so every ARN comes from the request's own SigV4
/// credential scope.
const REGION: &str = "eu-central-1";

/// A private-CA ARN matching `CertificateAuthorityArn`'s own pattern, in this Region.
const CA_ARN: &str = "arn:aws:acm-pca:eu-central-1:000000000000:certificate-authority/11111111-2222-3333-4444-555555555555";

/// The service's own status and reason vocabulary, named from its constants so a case cannot
/// drift off them.
const STATUS_DELETING: &str = scep::state::STATUS_DELETING;
const REASON_INVALID_STATE: &str = scep::state::REASON_INVALID_STATE;
/// The two words `ResourceType` carries ("either `Connector` or `Challenge`"), which is also
/// how the refusal sentence names the kind of resource it is about.
const RESOURCE_TYPE_CHALLENGE: &str = scep::state::RESOURCE_TYPE_CHALLENGE;

/// Which of this service's two taggable resources a row's writes name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Connector,
    Challenge,
}

/// The status the connector is in when its `DeleteConnector` is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteFrom {
    Active,
    Creating,
}

#[derive(Debug)]
struct Case {
    name: &'static str,
    kind: Kind,
    delete_from: DeleteFrom,
    /// The tags the create stored on the resource the row writes to.
    initial: &'static [(&'static str, &'static str)],
    /// The entries the refused `TagResource` call sends.
    add: &'static [(&'static str, &'static str)],
    /// The keys the refused `UntagResource` call names.
    remove: &'static [&'static str],
}

/// The rows: both resource kinds, both statuses a delete can be accepted from, resources that
/// carried no tags and resources that carried several, and calls naming one key, several keys,
/// or none at all.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "a connector that carries no tags",
            kind: Kind::Connector,
            delete_from: DeleteFrom::Active,
            initial: &[],
            add: &[("owner", "platform")],
            remove: &["owner"],
        },
        Case {
            name: "a connector that carries several tags",
            kind: Kind::Connector,
            delete_from: DeleteFrom::Active,
            initial: &[("environment", "staging"), ("owner", "platform")],
            add: &[("environment", "production"), ("tier", "gold")],
            remove: &["environment", "owner"],
        },
        Case {
            name: "a connector torn down from CREATING, tagged with an empty map",
            kind: Kind::Connector,
            delete_from: DeleteFrom::Creating,
            initial: &[("keep", "me")],
            add: &[],
            remove: &["keep"],
        },
        Case {
            name: "a challenge of the connector being torn down",
            kind: Kind::Challenge,
            delete_from: DeleteFrom::Active,
            initial: &[],
            add: &[("owner", "platform")],
            remove: &["owner"],
        },
        Case {
            name: "a challenge that carries several tags",
            kind: Kind::Challenge,
            delete_from: DeleteFrom::Active,
            initial: &[("stage", "one"), ("zone", "a")],
            add: &[("zone", "b"), ("app", "checkout")],
            remove: &["zone", "absent"],
        },
    ]
}

async fn create_connector(router: &MultiServiceRouter, tags: &Value) -> Answer {
    harness::call(
        router,
        REGION,
        "POST",
        "/connectors",
        Some(json!({ "CertificateAuthorityArn": CA_ARN, "Tags": tags })),
    )
    .await
}

async fn create_challenge(router: &MultiServiceRouter, connector: &str, tags: &Value) -> Answer {
    harness::call(
        router,
        REGION,
        "POST",
        "/challenges",
        Some(json!({ "ConnectorArn": connector, "Tags": tags })),
    )
    .await
}

async fn delete_connector(router: &MultiServiceRouter, arn: &str) -> Answer {
    harness::call(
        router,
        REGION,
        "DELETE",
        &format!("/connectors/{}", label(arn)),
        None,
    )
    .await
}

async fn get_connector(router: &MultiServiceRouter, arn: &str) -> Answer {
    harness::call(
        router,
        REGION,
        "GET",
        &format!("/connectors/{}", label(arn)),
        None,
    )
    .await
}

async fn list_tags(router: &MultiServiceRouter, arn: &str) -> Answer {
    harness::call(router, REGION, "GET", &format!("/tags/{}", label(arn)), None).await
}

/// One `TagResource` call: `Tags` is a body member.
async fn tag_resource(router: &MultiServiceRouter, arn: &str, tags: &[(&str, &str)]) -> Answer {
    harness::call(
        router,
        REGION,
        "POST",
        &format!("/tags/{}", label(arn)),
        Some(json!({ "Tags": tags_member(tags) })),
    )
    .await
}

/// One `UntagResource` call: `TagKeys` is bound to the `tagKeys` query parameter, so each key
/// is one repetition of it, percent-encoded the way an SDK encodes a query value.
async fn untag_resource(router: &MultiServiceRouter, arn: &str, keys: &[&str]) -> Answer {
    let query: Vec<String> = keys
        .iter()
        .map(|key| format!("tagKeys={}", label(key)))
        .collect();
    let path = if query.is_empty() {
        format!("/tags/{}", label(arn))
    } else {
        format!("/tags/{}?{}", label(arn), query.join("&"))
    };
    harness::call(router, REGION, "DELETE", &path, None).await
}

/// A `Tags` member for a request body.
fn tags_member(tags: &[(&str, &str)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
            .collect(),
    )
}

/// The `Tags` body a map should come back as from a listing.
fn expected_body(tags: &BTreeMap<String, String>) -> Map<String, Value> {
    tags.iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect()
}

/// What the row's resource carries once both pre-teardown probes have landed: its initial tags
/// plus the key the accepted `TagResource` wrote and the accepted `UntagResource` did not name.
fn expected_tags(case: &Case) -> BTreeMap<String, String> {
    let mut tags: BTreeMap<String, String> = case
        .initial
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    tags.insert(PROBE.0.to_string(), PROBE.1.to_string());
    tags
}

/// The tag both endpoints are probed with BEFORE the delete is accepted: `TagResource` writes
/// it and `UntagResource` drops a key that is not there, so each row proves both calls were
/// accepted on the same resource until the teardown was asked for.
const PROBE: (&str, &str) = ("before-teardown", "accepted");

/// The key the accepted `UntagResource` probe names: one the resource does not carry, so the
/// probe changes nothing a later assertion depends on.
const PROBE_ABSENT_KEY: &str = "never-set";

/// The opening every precondition refusal of this service carries, spelled here as a wire
/// expectation: a request whose input is well-formed but names a resource whose state does not
/// admit the call is refused with this scaffold around the clause that says which precondition
/// failed, and a terminating `.` closes the sentence.
const PRECONDITION_PREFIX: &str = "Validation error detected: ";

/// The sentence this service refuses a tag change on `resource_arn` with while the connector
/// `connector_arn` is in `status`, spelled here as a wire expectation rather than read back out
/// of the implementation. Two forms, because a connector ARN names the governing record itself
/// while a challenge ARN names a resource that belongs to one — each inside the one
/// precondition scaffold every refusal of a resource's state carries.
fn expected_message(kind: Kind, resource_arn: &str, connector_arn: &str, status: &str) -> String {
    match kind {
        Kind::Connector => format!(
            "{PRECONDITION_PREFIX}Connector {connector_arn} has status {status}; its tags cannot \
             be changed."
        ),
        Kind::Challenge => format!(
            "{PRECONDITION_PREFIX}{RESOURCE_TYPE_CHALLENGE} {resource_arn} belongs to connector \
             {connector_arn}, which has status {status}; its tags cannot be changed."
        ),
    }
}

/// Stand the row's resource up and answer `(connector ARN, the ARN the row writes to)`.
///
/// A challenge can only be minted by an `ACTIVE` connector, so a challenge row's connector is
/// settled first; a connector row's connector is left `CREATING` when that is the status its
/// delete is to be accepted from.
async fn resources_of(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    case: &Case,
) -> (String, String) {
    let name = case.name;
    let connector_tags = match case.kind {
        Kind::Connector => tags_member(case.initial),
        Kind::Challenge => tags_member(&[]),
    };
    let created = create_connector(router, &connector_tags).await;
    assert_eq!(
        created.status, 202,
        "case {name:?}: expected 202 from CreateConnector, got {} / {:?}",
        created.status, created.body
    );
    let connector = created.text("ConnectorArn");

    if case.kind == Kind::Challenge || case.delete_from == DeleteFrom::Active {
        harness::release_pending(engine, &connector);
    } else {
        // The row's delete is accepted from `CREATING`, so hold that transition rather than
        // race the service's own transition span.
        harness::hold_pending(engine, &connector);
    }

    let resource = match case.kind {
        Kind::Connector => connector.clone(),
        Kind::Challenge => {
            let created = create_challenge(router, &connector, &tags_member(case.initial)).await;
            assert_eq!(
                created.status, 202,
                "case {name:?}: expected 202 from CreateChallenge, got {} / {:?}",
                created.status, created.body
            );
            created
                .challenge("Arn")
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {name:?}: expected a challenge Arn in {:?}", created.body)
                })
                .to_string()
        }
    };
    (connector, resource)
}

/// What one refused write answered, in the four values a caller branches on: the status, the
/// error code, the `Reason` and the sentence.
fn refusal_of(answer: &Answer) -> (u16, String, Value, String) {
    (
        answer.status,
        answer.error_type.clone(),
        answer.body["Reason"].clone(),
        answer.message().to_string(),
    )
}

#[tokio::test]
async fn both_tagging_writes_answer_one_state_precondition_identically() {
    for case in cases() {
        let name = case.name;
        let (router, engine) = new_router();
        let (connector, resource) = resources_of(&router, &engine, &case).await;

        // Until the delete is asked for, BOTH writes are accepted on the very resource whose
        // state refuses them below, so the agreement asserted after it is not an agreement on
        // refusing everything.
        let tagged = tag_resource(&router, &resource, &[PROBE]).await;
        assert_eq!(
            tagged.status, 204,
            "case {name:?}: a TagResource before the delete must be accepted, got {} / {:?}",
            tagged.status, tagged.body
        );
        let untagged = untag_resource(&router, &resource, &[PROBE_ABSENT_KEY]).await;
        assert_eq!(
            untagged.status, 204,
            "case {name:?}: an UntagResource before the delete must be accepted, got {} / {:?}",
            untagged.status, untagged.body
        );

        let deleted = delete_connector(&router, &connector).await;
        assert_eq!(
            deleted.status, 202,
            "case {name:?}: expected 202 from DeleteConnector, got {} / {:?}",
            deleted.status, deleted.body
        );
        // From here the case is about the teardown window, so pin it.
        harness::hold_pending(&engine, &connector);
        let read = get_connector(&router, &connector).await;
        assert_eq!(
            read.connector("Status"),
            &json!(STATUS_DELETING),
            "case {name:?}: expected the connector to report {STATUS_DELETING}, got {:?}",
            read.body
        );

        // The two writes the one precondition refuses.
        let refused_tag = tag_resource(&router, &resource, case.add).await;
        let refused_untag = untag_resource(&router, &resource, case.remove).await;
        let expected = (
            400u16,
            "ValidationException".to_string(),
            json!(REASON_INVALID_STATE),
            expected_message(case.kind, &resource, &connector, STATUS_DELETING),
        );
        for (operation, answer) in [
            ("TagResource", &refused_tag),
            ("UntagResource", &refused_untag),
        ] {
            assert_eq!(
                refusal_of(answer),
                expected,
                "case {name:?}: expected {operation} to be refused with {expected:?}, got {:?} \
                 / {:?}",
                refusal_of(answer),
                answer.body
            );
        }
        assert_eq!(
            refusal_of(&refused_tag),
            refusal_of(&refused_untag),
            "case {name:?}: the two tagging writes must answer one state identically, got {:?} \
             and {:?}",
            refusal_of(&refused_tag),
            refusal_of(&refused_untag)
        );

        // Neither refused call changed anything, and the record is still REPORTED with what it
        // carried.
        let expected_listing = expected_body(&expected_tags(&case));
        let listed = list_tags(&router, &resource).await;
        assert_eq!(
            listed.status, 200,
            "case {name:?}: a resource under teardown is still reported, got {} / {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            listed.tags(),
            &expected_listing,
            "case {name:?}: expected the refused calls to leave {expected_listing:?}, got {:?}",
            listed.tags()
        );

        // Once the teardown comes due the record is gone, so BOTH writes answer the tagging
        // layer's own not-found refusal instead of the state one — the same agreement on the
        // other side of the window.
        harness::release_pending(&engine, &connector);
        let gone_tag = tag_resource(&router, &resource, case.add).await;
        harness::assert_tagging_not_found(&gone_tag, name);
        let gone_untag = untag_resource(&router, &resource, case.remove).await;
        harness::assert_tagging_not_found(&gone_untag, name);
    }
}

/// One `CreateChallenge` call, which is refused for the connector's status the same way a tag
/// write is: the contrast row of the table below, from an operation that is not a tag write.
async fn create_challenge_for(router: &MultiServiceRouter, connector: &str) -> Answer {
    create_challenge(router, connector, &tags_member(&[])).await
}

/// A connector of this Region, left in the status its create starts it in because the
/// transition the clock owes it is pinned undue rather than waited for.
async fn creating_connector(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    case: &str,
) -> String {
    let created = create_connector(router, &tags_member(&[])).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected 202 from CreateConnector, got {} / {:?}",
        created.status, created.body
    );
    let arn = created.text("ConnectorArn");
    harness::hold_pending(engine, &arn);
    arn
}

#[tokio::test]
async fn every_precondition_refusal_opens_with_the_service_scaffold() {
    let (router, engine) = new_router();

    // A connector accepted for teardown, with a challenge of its own hanging off it: the two
    // taggable kinds, both governed by that connector's status. Every status below is pinned
    // with the clock move, so no row waits for a transition span.
    let connector = creating_connector(&router, &engine, "the connector under teardown").await;
    harness::release_pending(&engine, &connector);
    let created = create_challenge_for(&router, &connector).await;
    assert_eq!(
        created.status, 202,
        "expected 202 from CreateChallenge over the settled connector, got {} / {:?}",
        created.status, created.body
    );
    let challenge = created
        .challenge("Arn")
        .as_str()
        .unwrap_or_else(|| panic!("expected a challenge Arn in {:?}", created.body))
        .to_string();
    let deleted = delete_connector(&router, &connector).await;
    assert_eq!(
        deleted.status, 202,
        "expected 202 from DeleteConnector, got {} / {:?}",
        deleted.status, deleted.body
    );
    harness::hold_pending(&engine, &connector);

    // A second connector that never settled, so the refusal of a create over it is about a
    // status no delete was asked for.
    let still_creating = creating_connector(&router, &engine, "the connector still creating").await;

    // The rows: each a well-formed request that one of this service's operations refuses
    // because the status of the resource it names does not admit the call, with the ARN of the
    // resource whose status refused it. They differ along what the rule generalises over —
    // which operation refused the call, which of the two taggable kinds it named, and which
    // non-serving status the governing connector was in.
    let rows: Vec<(&str, &str, Answer)> = vec![
        (
            "TagResource over the connector under teardown",
            &connector,
            tag_resource(&router, &connector, &[("owner", "platform")]).await,
        ),
        (
            "UntagResource over the connector under teardown",
            &connector,
            untag_resource(&router, &connector, &["owner"]).await,
        ),
        (
            "TagResource over a challenge of that connector",
            &challenge,
            tag_resource(&router, &challenge, &[("owner", "platform")]).await,
        ),
        (
            "UntagResource over a challenge of that connector",
            &challenge,
            untag_resource(&router, &challenge, &["owner", "absent"]).await,
        ),
        (
            "CreateChallenge over the connector under teardown",
            &connector,
            create_challenge_for(&router, &connector).await,
        ),
        (
            "CreateChallenge over a connector that is still creating",
            &still_creating,
            create_challenge_for(&router, &still_creating).await,
        ),
    ];

    for (name, governing_arn, answer) in &rows {
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (400, "ValidationException"),
            "case {name:?}: a precondition refusal is a 400 ValidationException, got {} / {:?}",
            answer.status,
            answer.body
        );
        assert_eq!(
            &answer.body["Reason"],
            &json!(REASON_INVALID_STATE),
            "case {name:?}: expected Reason {REASON_INVALID_STATE}, got {:?}",
            answer.body
        );
        let message = answer.message();
        assert!(
            message.starts_with(PRECONDITION_PREFIX),
            "case {name:?}: expected the sentence to open with {PRECONDITION_PREFIX:?}, got \
             {message:?}"
        );
        assert!(
            message.ends_with('.'),
            "case {name:?}: expected the sentence to close with a period, got {message:?}"
        );
        // What is left once the scaffold is taken off: the one clause this refusal states.
        let clause = &message[PRECONDITION_PREFIX.len()..message.len() - 1];
        assert!(
            !clause.is_empty() && !clause.ends_with('.'),
            "case {name:?}: expected one clause inside the scaffold, got {clause:?}"
        );
        assert!(
            clause.contains(*governing_arn),
            "case {name:?}: expected the clause to name {governing_arn:?}, got {clause:?}"
        );
    }
}
