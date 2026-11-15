// SPDX-License-Identifier: CC0-1.0

//! What this service answers an untag that names NO tag key, and what it answers a request
//! that leaves a required member out where no such wire form exists.
//!
//! The rules under test:
//!
//! * `tagKeys` is bound to a REPEATED query parameter
//!   (`smithy_query(query="operation",operation="UntagResource")`: `@httpQuery("tagKeys")`
//!   over `TagKeyList`), and a repeated parameter carries a list by appearing once per entry
//!   — so a client removing an EMPTY list of keys appends no `tagKeys` parameter at all.
//!   That request is SERVED: it removes no tag, answers the `204 No Content` every
//!   members-less output of this service answers, and leaves the resource's tag set exactly
//!   as it was. The keys a request does name are still the keys it removes.
//! * The rest of the request is judged as it always is: `resourceArn`'s modelled
//!   `GenericV2ARN` pattern, the tagging route's own not-found class for an ARN that names no
//!   resource this service holds, and `TagKeyList`'s own bounds on the keys a request DOES
//!   name — a keyless request is refused by exactly the same answer a keyed one is, since the
//!   keys are the only thing that differs between them.
//! * A required member with no empty wire form is still absent when the request omits it: a
//!   `TagResource` carrying no `tags` object is the modelled `ValidationException`, naming
//!   that member.
//!
//! The rows differ along what the rules generalise over: the FORM the absent parameter takes
//! on the wire (no query string, a bare `?`, a query string holding only another operation's
//! parameter), the kind of resource addressed (a component version and a deployment), the way
//! a `resourceArn` can be refused (outside the modelled pattern, another vendor's ARN, an ARN
//! naming nothing), and the way a `tagKeys` that IS present can fall outside its modelled
//! shape (an entry below `TagKey`'s minimum length, one above its maximum, a list past
//! `TagKeyList`'s maximum, and the maximum itself).
//!
//! Every assertion drives the real `restJson1` front door through the harness
//! `tests/all/deployment_history_and_tag_removal.rs` owns, and is judged on the HTTP status,
//! the error code, the message and what the tag listing reports afterwards.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::deployment_history_and_tag_removal::{
    create_component, deploy_to, deployment_arn, emulator, encoded_label, error_code, list_tags,
    send, tag, tag_object, text, thing_target,
};

/// The code the modelled 400 carries
/// (`smithy_query(query="shape",shape="ValidationException")`: `client`, HTTP 400).
const VALIDATION_CODE: &str = "ValidationException";

/// The status an operation whose modelled output declares no member answers.
const NO_CONTENT: u16 = 204;

/// `TagKey` is `length 1..128` and `TagKeyList` `length 1..200`
/// (`smithy_query(query="shape",shape="TagKeyList,TagKey")`) — the bounds the rows below are
/// built against and walked past.
const TAG_KEY_MAX: usize = 128;
const TAG_KEYS_MAX: usize = 200;

/// The tags every resource below is created with, so a request that removed something it was
/// not asked to shows up in the listing.
const STORED: &[(&str, &str)] = &[("env", "prod"), ("owner", "fleet-team"), ("tier", "edge")];

/// `UntagResource` on `arn` with `query` as the raw query string: `None` for a request that
/// carries none at all, which is one of the forms a client removing no key sends.
async fn untag_with_query(
    router: &MultiServiceRouter,
    arn: &str,
    query: Option<&str>,
) -> (u16, Value) {
    let path = match query {
        None => format!("/tags/{}", encoded_label(arn)),
        Some(query) => format!("/tags/{}?{query}", encoded_label(arn)),
    };
    send(router, "DELETE", &path, Vec::new()).await
}

/// `UntagResource` naming `keys`, each as its own `tagKeys` parameter.
async fn untag(router: &MultiServiceRouter, arn: &str, keys: &[String]) -> (u16, Value) {
    let query = keys
        .iter()
        .map(|key| format!("tagKeys={key}"))
        .collect::<Vec<String>>()
        .join("&");
    untag_with_query(router, arn, Some(&query)).await
}

/// The three forms an untag that names no key takes on the wire.
fn keyless_forms() -> [(&'static str, Option<&'static str>); 3] {
    [
        ("no query string at all", None),
        ("a question mark and nothing after it", Some("")),
        ("only another operation's parameter", Some("maxResults=1")),
    ]
}

/// The tag set `arn` carries, as the listing reports it.
async fn listed_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, body) = list_tags(router, arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the tag listing must answer 200, got {status} {body}"
    );
    body["tags"].clone()
}

/// One resource of each kind this service holds, tagged with [`STORED`].
async fn tagged_resources(router: &MultiServiceRouter) -> Vec<(&'static str, String)> {
    let component = create_component(router, "com.example.Required", "1.0.0").await;
    let deployment = deployment_arn(&deploy_to(router, &thing_target("RequiredCore")).await);
    let resources = vec![
        ("a component version", component),
        ("a deployment", deployment),
    ];
    for (kind, arn) in &resources {
        let (status, _, body) = tag(router, arn, STORED).await;
        assert_eq!(
            status, NO_CONTENT,
            "setting up {kind}: the tag write must answer {NO_CONTENT}, got {status} {body}"
        );
    }
    resources
}

/// RULE: an untag that names no tag key removes none and is served — the empty list a client
/// sends by appending no `tagKeys` parameter is a request for the tag set the resource
/// already has, whatever form that absence takes on the wire.
#[tokio::test]
async fn an_untag_that_names_no_key_removes_none_and_is_served() {
    let router = emulator();
    let resources = tagged_resources(&router).await;
    let stored = tag_object(STORED);

    for (case, query) in keyless_forms() {
        for (kind, arn) in &resources {
            // Twice: a request that changes nothing changes nothing when it is repeated.
            for attempt in 1..=2 {
                let (status, body) = untag_with_query(&router, arn, query).await;
                assert_eq!(
                    (status, body.clone()),
                    (NO_CONTENT, Value::Null),
                    "case {case:?} on {kind}, attempt {attempt}: expected {NO_CONTENT} with no \
                     body, got {status} {body}"
                );
                let after = listed_tags(&router, case, arn).await;
                assert_eq!(
                    after, stored,
                    "case {case:?} on {kind}, attempt {attempt}: naming no key must leave \
                     {stored}, got {after}"
                );
            }
        }
    }

    // And the keys a request DOES name are still the keys it removes, so nothing above was
    // served by a route that stopped removing tags.
    for (kind, arn) in &resources {
        let (status, body) = untag(&router, arn, &["env".to_string()]).await;
        assert_eq!(
            status, NO_CONTENT,
            "case \"one key named\" on {kind}: expected {NO_CONTENT}, got {status} {body}"
        );
        let after = listed_tags(&router, "one key named", arn).await;
        assert_eq!(
            after,
            tag_object(&STORED[1..]),
            "case \"one key named\" on {kind}: expected exactly that key removed, got {after}"
        );
    }
}

/// RULE: the keys are the only thing a keyless untag leaves out, so every refusal of the
/// `resourceArn` it addresses is the one a keyed untag of the same ARN answers — the modelled
/// `ValidationException` for an ARN outside `GenericV2ARN`, and the tagging route's own
/// not-found class for one that names no resource this service holds.
#[tokio::test]
async fn a_keyless_untag_refuses_a_resource_arn_exactly_as_a_keyed_one_does() {
    let router = emulator();
    // One live resource, so no row below is refused because the store is empty.
    let present = create_component(&router, "com.example.Present", "1.0.0").await;
    tag(&router, &present, STORED).await;

    // The ways a `resourceArn` is refused: outside the modelled pattern (no ARN at all, and
    // another vendor's ARN), and inside it but naming nothing.
    let rows = [
        ("a label that is no ARN at all", "not-an-arn".to_string()),
        (
            "another vendor's ARN",
            "arn:aws:iot:us-west-2:000000000000:thing/RequiredCore".to_string(),
        ),
        (
            "a component version never published",
            "arn:aws:greengrass:us-west-2:000000000000:components:com.example.Absent:versions:1.0.0"
                .to_string(),
        ),
        (
            "a deployment never created",
            "arn:aws:greengrass:us-west-2:000000000000:deployments:00000000-0000-4000-8000-000000000000"
                .to_string(),
        ),
    ];

    for (case, arn) in rows {
        let (keyed_status, keyed_body) = untag(&router, &arn, &["env".to_string()]).await;
        assert_ne!(
            keyed_status, NO_CONTENT,
            "case {case:?}: the row must be one a keyed untag refuses, got {keyed_status} \
             {keyed_body}"
        );
        for (form, query) in keyless_forms() {
            let (status, body) = untag_with_query(&router, &arn, query).await;
            assert_eq!(
                (status, error_code(&body), text(&body, "message")),
                (
                    keyed_status,
                    error_code(&keyed_body),
                    text(&keyed_body, "message")
                ),
                "case {case:?} with {form:?}: a keyless untag must be refused exactly as the \
                 keyed one is ({keyed_status} {keyed_body}), got {status} {body}"
            );
        }
    }

    // The live resource still untags, so every row above was refused for the ARN it named.
    let (status, body) = untag_with_query(&router, &present, None).await;
    assert_eq!(
        status, NO_CONTENT,
        "case \"the published component version\": the live resource must still untag, got \
         {status} {body}"
    );
    let after = listed_tags(&router, "the published component version", &present).await;
    assert_eq!(
        after,
        tag_object(STORED),
        "case \"the published component version\": naming no key must leave the stored tags, \
         got {after}"
    );
}

/// RULE: a required member with no empty wire form is absent when the request omits it — a
/// `TagResource` carrying no `tags` object is the modelled `ValidationException`, naming that
/// member, and writes nothing. `tags` is `@required`
/// (`smithy_query(query="operation",operation="TagResource")`) and is read from the BODY, so
/// unlike a repeated query parameter it has no form that says "an empty set".
#[tokio::test]
async fn an_absent_required_body_member_is_the_modelled_validation_refusal() {
    let router = emulator();
    let resources = tagged_resources(&router).await;
    let stored = tag_object(STORED);
    let expected = "1 validation error detected: Value null at 'tags' failed to satisfy \
                    constraint: Member must not be null";

    for (kind, arn) in &resources {
        let body = serde_json::to_vec(&json!({})).expect("a serializable request body");
        let (status, answered) =
            send(&router, "POST", &format!("/tags/{}", encoded_label(arn)), body).await;
        assert_eq!(
            status, 400,
            "case \"no tags at all\" on {kind}: expected 400, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            VALIDATION_CODE,
            "case \"no tags at all\" on {kind}: expected {VALIDATION_CODE}, got {answered}"
        );
        assert_eq!(
            text(&answered, "message"),
            expected,
            "case \"no tags at all\" on {kind}: expected the validation envelope naming the \
             absent member, got {answered}"
        );
        let after = listed_tags(&router, "no tags at all", arn).await;
        assert_eq!(
            after, stored,
            "case \"no tags at all\" on {kind}: a refused write must leave {stored}, got {after}"
        );
    }
}

/// RULE: a `tagKeys` the request DOES carry but outside its modelled shape is refused with
/// the modelled `ValidationException` and removes nothing, while the published bound itself is
/// served.
#[tokio::test]
async fn tag_keys_outside_the_modelled_shape_are_refused_and_remove_nothing() {
    let router = emulator();
    let resources = tagged_resources(&router).await;
    let stored = tag_object(STORED);

    // Each row walks one published bound: `TagKey`'s minimum and maximum length, and
    // `TagKeyList`'s maximum entry count.
    let rows: Vec<(String, Vec<String>)> = vec![
        (
            "an empty key, one short of TagKey's modelled minimum".to_string(),
            vec![String::new()],
        ),
        (
            "a key one character past TagKey's modelled maximum".to_string(),
            vec!["k".repeat(TAG_KEY_MAX + 1)],
        ),
        (
            "one key past TagKeyList's modelled maximum".to_string(),
            (0..=TAG_KEYS_MAX).map(|n| format!("key-{n}")).collect(),
        ),
        (
            "an empty key beside one the resource carries".to_string(),
            vec![String::new(), "env".to_string()],
        ),
    ];

    for (case, keys) in rows {
        let case = case.as_str();
        for (kind, arn) in &resources {
            let (status, body) = untag(&router, arn, &keys).await;
            assert_eq!(
                status, 400,
                "case {case:?} on {kind}: expected 400, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                VALIDATION_CODE,
                "case {case:?} on {kind}: expected {VALIDATION_CODE}, got {body}"
            );
            let after = listed_tags(&router, case, arn).await;
            assert_eq!(
                after, stored,
                "case {case:?} on {kind}: a refused removal must leave {stored}, got {after}"
            );
        }
    }

    // Exactly `TagKeyList`'s maximum is a request the model admits: it is served, and since
    // none of those keys is stored it removes nothing.
    let at_the_bound: Vec<String> = (0..TAG_KEYS_MAX).map(|n| format!("key-{n}")).collect();
    for (kind, arn) in &resources {
        let (status, body) = untag(&router, arn, &at_the_bound).await;
        assert_eq!(
            (status, body.clone()),
            (NO_CONTENT, Value::Null),
            "case \"exactly TagKeyList's modelled maximum\" on {kind}: expected {NO_CONTENT} \
             with no body, got {status} {body}"
        );
        let after = listed_tags(&router, "exactly TagKeyList's modelled maximum", arn).await;
        assert_eq!(
            after, stored,
            "case \"exactly TagKeyList's modelled maximum\" on {kind}: naming only keys the \
             resource never carried must leave {stored}, got {after}"
        );
    }

    // And every key the resource carries is removed, so none of the refusals above came from
    // a route that stopped serving.
    for (kind, arn) in &resources {
        let keys: Vec<String> = STORED.iter().map(|(key, _)| (*key).to_string()).collect();
        let (status, body) = untag(&router, arn, &keys).await;
        assert_eq!(
            status, NO_CONTENT,
            "case \"every key the resource carries\" on {kind}: expected {NO_CONTENT}, got \
             {status} {body}"
        );
        let after = listed_tags(&router, "every key the resource carries", arn).await;
        assert_eq!(
            after,
            tag_object(&[]),
            "case \"every key the resource carries\" on {kind}: expected every key removed, \
             got {after}"
        );
    }
}
