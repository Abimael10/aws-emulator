// SPDX-License-Identifier: CC0-1.0

//! What the ARN-addressed resource-tagging plane — `POST`, `DELETE` and `GET` on
//! `/tags/{ResourceArn}` — ANSWERS, end to end over HTTP.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is a
//! separate HTTP request against one shared engine. What the plane STORES is
//! `crate::resource_tags`' subject; the two rules here are about the answer itself:
//!
//! 1. a served mutation of this plane answers 204 No Content with an empty body.
//!    `TagResourceResponse` and `UntagResourceResponse` model no members, so the answer
//!    has nothing for a caller to parse and the status is the whole of it. The plane's
//!    READ, whose output carries `Tags`, keeps 200 — the status is per operation, not per
//!    route.
//! 2. a tag-plane call whose `ResourceArn` resolves to no resource of this service is
//!    refused 404 with the shared tagging frontend's own error code (the plane is one
//!    route in front of every taggable resource, and that code is in no operation's
//!    modelled error set) and ONE fixed sentence, which carries nothing the request
//!    supplied: the refusal names neither the kind the ARN carried nor the name parsed out
//!    of it.
//!
//! The tables span what the rules generalise over: the resource kind the ARN names
//! (every taggable kind the crate resolves), how many keys one mutation carries, whether
//! a key is new, already held or absent, repeating the identical call, and the ways an
//! ARN of this vendor resolves to nothing — a name nothing is stored under, a case
//! variant of one that is, a modelled resource kind the store holds none of, a kind no
//! model names, a resource part with no name, one with no separator, and a resource
//! retired by its own delete.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one HTTP harness this crate's suites share.
use crate::harness::{call, emulator, encode, Answer, ACCOUNT, REGION};

/// The status a served mutation of this plane answers with, and nothing else: no body
/// for a caller to parse.
const NO_CONTENT: u16 = 204;

/// The status the plane's READ answers with, since `ListTagsForResourceResponse` carries
/// the `Tags` member.
const READ_OK: u16 = 200;

/// The status, the wire error CODE and the whole message this plane answers for an ARN
/// that resolves to no resource. Written out here rather than read off the crate, so this
/// suite pins the wire contract itself: the ARN-addressed tag operations are served by the
/// shared tagging frontend, whose refusal is its own `NotFoundException` rather than the
/// `ResourceNotFoundException` the model declares for them, and whose sentence is FIXED.
const NOT_FOUND_STATUS: u16 = 404;
const NOT_FOUND_CODE: &str = "NotFoundException";
const NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// One taggable resource kind: the ARN `resourcetype` segment its ARN carries, how a
/// test creates one, and where the create reports its ARN.
struct Kind {
    arn_resource_type: &'static str,
    create_path: &'static str,
    create_body: fn(&str) -> Value,
    arn_member: &'static str,
}

fn tracker_body(name: &str) -> Value {
    json!({ "TrackerName": name })
}

fn collection_body(name: &str) -> Value {
    json!({ "CollectionName": name })
}

fn map_body(name: &str) -> Value {
    // `MapConfiguration$Style`, from the model's own catalog.
    json!({ "MapName": name, "Configuration": { "Style": "VectorEsriStreets" } })
}

fn calculator_body(name: &str) -> Value {
    json!({ "CalculatorName": name, "DataSource": "Esri" })
}

fn index_body(name: &str) -> Value {
    json!({ "IndexName": name, "DataSource": "Esri" })
}

/// Every resource kind this plane resolves an ARN to, with the create that makes one.
const KINDS: &[Kind] = &[
    Kind {
        arn_resource_type: "tracker",
        create_path: "/tracking/v0/trackers",
        create_body: tracker_body,
        arn_member: "TrackerArn",
    },
    Kind {
        arn_resource_type: "geofence-collection",
        create_path: "/geofencing/v0/collections",
        create_body: collection_body,
        arn_member: "CollectionArn",
    },
    Kind {
        arn_resource_type: "map",
        create_path: "/maps/v0/maps",
        create_body: map_body,
        arn_member: "MapArn",
    },
    Kind {
        arn_resource_type: "route-calculator",
        create_path: "/routes/v0/calculators",
        create_body: calculator_body,
        arn_member: "CalculatorArn",
    },
    Kind {
        arn_resource_type: "place-index",
        create_path: "/places/v0/indexes",
        create_body: index_body,
        arn_member: "IndexArn",
    },
];

/// Create one resource of `kind` and answer the ARN its create reported.
async fn create(router: &MultiServiceRouter, kind: &Kind, name: &str) -> String {
    let created = call(
        router,
        "POST",
        kind.create_path,
        Some((kind.create_body)(name)),
    )
    .await;
    assert_eq!(
        created.status, READ_OK,
        "case {:?}: creating the resource must succeed, got {} {:?}",
        kind.arn_resource_type, created.status, created.body
    );
    created
        .json()
        .get(kind.arn_member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: the create must report {}, got {:?}",
                kind.arn_resource_type, kind.arn_member, created.body
            )
        })
        .to_string()
}

/// The request body a tag write carries, from a list of pairs.
fn tag_map(pairs: &[(&str, &str)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect(),
    )
}

/// `TagResource`: `POST /tags/{ResourceArn}`.
async fn tag(router: &MultiServiceRouter, arn: &str, pairs: &[(&str, &str)]) -> Answer {
    call(
        router,
        "POST",
        &format!("/tags/{}", encode(arn)),
        Some(json!({ "Tags": tag_map(pairs) })),
    )
    .await
}

/// `UntagResource`: `DELETE /tags/{ResourceArn}?tagKeys=…`, one `tagKeys` pair per key as
/// the model binds them.
async fn untag(router: &MultiServiceRouter, arn: &str, keys: &[&str]) -> Answer {
    let query = keys
        .iter()
        .map(|key| format!("tagKeys={}", encode(key)))
        .collect::<Vec<_>>()
        .join("&");
    call(
        router,
        "DELETE",
        &format!("/tags/{}?{query}", encode(arn)),
        None,
    )
    .await
}

/// `ListTagsForResource`: `GET /tags/{ResourceArn}`.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> Answer {
    call(router, "GET", &format!("/tags/{}", encode(arn)), None).await
}

/// Assert one served mutation: the plane's success status, and no body at all.
fn assert_no_content(case: &str, what: &str, answer: &Answer) {
    assert_eq!(
        answer.status, NO_CONTENT,
        "case {case:?}: {what} must answer {NO_CONTENT}, got {} {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.body.is_empty(),
        "case {case:?}: {what} must answer with no body, got {:?}",
        answer.body
    );
}

// ---------------------------------------------------------------------------
// 1. A served mutation answers 204 No Content
// ---------------------------------------------------------------------------

/// One row of the mutation table: what the resource already holds, the map the write
/// under test carries, and the keys the removal under test names.
struct MutationCase {
    label: &'static str,
    seed: &'static [(&'static str, &'static str)],
    write: &'static [(&'static str, &'static str)],
    remove: &'static [&'static str],
}

/// The table. Rows differ in how many keys a mutation carries (one, two, three), in
/// whether each key is new, already held or absent, and in the punctuation the modelled
/// `TagKey`/`TagValue` patterns admit (`^([\p{L}\p{Z}\p{N}_.,:/=+\-@]*)$`), including the
/// empty value `TagValue`'s `@length 0..256` allows.
const MUTATION_CASES: &[MutationCase] = &[
    MutationCase {
        label: "one new key, the only held key removed",
        seed: &[],
        write: &[("env", "dev")],
        remove: &["env"],
    },
    MutationCase {
        label: "a held key replaced, an absent key removed",
        seed: &[("env", "dev")],
        write: &[("env", "prod")],
        remove: &["never-set"],
    },
    MutationCase {
        label: "three keys carrying the documented punctuation, two of them removed",
        seed: &[("kept", "1")],
        write: &[
            ("cost+center", "a=b"),
            ("path/key", "ns:value"),
            ("dotted.key_1", "at@sign-here"),
        ],
        remove: &["cost+center", "path/key"],
    },
    MutationCase {
        label: "an empty value written, every key removed",
        seed: &[("empty", "not yet")],
        write: &[("empty", "")],
        remove: &["empty"],
    },
];

#[tokio::test]
async fn a_served_tag_plane_mutation_answers_no_content_on_every_resource_kind() {
    for kind in KINDS {
        for case in MUTATION_CASES {
            let label = format!("{} / {}", kind.arn_resource_type, case.label);
            let router = emulator();
            let arn = create(&router, kind, "answered").await;

            if !case.seed.is_empty() {
                assert_no_content(&label, "the seeding TagResource", &tag(&router, &arn, case.seed).await);
            }

            // The write under test, and the identical write again: a repeated mutation
            // answers exactly as the first one did.
            assert_no_content(&label, "TagResource", &tag(&router, &arn, case.write).await);
            assert_no_content(
                &label,
                "a repeated TagResource",
                &tag(&router, &arn, case.write).await,
            );

            // The READ between the two mutations keeps its own status and carries a
            // body: the 204 belongs to the mutations, not to the route.
            let listed = list_tags(&router, &arn).await;
            assert_eq!(
                listed.status, READ_OK,
                "case {label:?}: ListTagsForResource must answer {READ_OK}, got {} {:?}",
                listed.status, listed.body
            );
            assert!(
                listed.json().get("Tags").is_some(),
                "case {label:?}: the read must carry the Tags member, got {:?}",
                listed.body
            );

            assert_no_content(
                &label,
                "UntagResource",
                &untag(&router, &arn, case.remove).await,
            );
            assert_no_content(
                &label,
                "a repeated UntagResource",
                &untag(&router, &arn, case.remove).await,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 2. One fixed refusal sentence, carrying nothing the request supplied
// ---------------------------------------------------------------------------

/// Assert one refused tag-plane call: the status, the shape in `x-amzn-errortype`, the
/// plane's one fixed sentence, and that nothing the request supplied reached it.
fn assert_fixed_not_found(case: &str, answer: &Answer, supplied: &[&str]) {
    assert_eq!(
        answer.status, NOT_FOUND_STATUS,
        "case {case:?}: expected HTTP {NOT_FOUND_STATUS}, got {} {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.error_type.is_some(),
        "case {case:?}: the refusal must name its shape in x-amzn-errortype, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.error_code(),
        NOT_FOUND_CODE,
        "case {case:?}: expected {NOT_FOUND_CODE}, got {:?} ({:?})",
        answer.error_code(),
        answer.body
    );
    let message = answer.message();
    assert_eq!(
        message, NOT_FOUND_MESSAGE,
        "case {case:?}: expected the plane's fixed sentence {NOT_FOUND_MESSAGE:?}, got \
         {message:?}"
    );
    for value in supplied {
        assert!(
            !message.contains(value),
            "case {case:?}: the refusal must not carry {value:?} from the request, got \
             {message:?}"
        );
    }
}

/// Assert all three operations of the plane refuse `arn` the same way.
async fn assert_every_operation_refuses(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    supplied: &[&str],
) {
    assert_fixed_not_found(
        &format!("{case} / TagResource"),
        &tag(router, arn, &[("env", "dev")]).await,
        supplied,
    );
    assert_fixed_not_found(
        &format!("{case} / ListTagsForResource"),
        &list_tags(router, arn).await,
        supplied,
    );
    assert_fixed_not_found(
        &format!("{case} / UntagResource"),
        &untag(router, arn, &["env"]).await,
        supplied,
    );
}

#[tokio::test]
async fn an_arn_resolving_to_nothing_is_refused_with_one_sentence_on_every_kind() {
    for kind in KINDS {
        let router = emulator();
        // One resource of this kind exists under another name, so a refusal cannot be
        // "the store is empty", and the lookup is by the exact name the ARN carries.
        create(&router, kind, "Exists").await;

        for name in ["missing", "exists", "EXISTS", "Exists.2"] {
            let arn = format!(
                "arn:aws:geo:{REGION}:{ACCOUNT}:{}/{name}",
                kind.arn_resource_type
            );
            let case = format!("{} named {name}", kind.arn_resource_type);
            assert_every_operation_refuses(
                &router,
                &case,
                &arn,
                &[name, kind.arn_resource_type, &arn],
            )
            .await;
        }
    }
}

#[tokio::test]
async fn an_arn_of_this_vendor_resolving_to_no_kind_is_refused_with_the_same_sentence() {
    // Rows: a `resourcetype` the model names but the store holds no resources of, a type
    // no model names at all, a resource part with no name, and one with no separator.
    // Each is a well-formed ARN of this service's vendor, so each is a not-found rather
    // than a bad request — and each earns the SAME sentence the rows above do.
    let arns = [
        format!("arn:aws:geo:{REGION}:{ACCOUNT}:key/anything"),
        format!("arn:aws:geo:{REGION}:{ACCOUNT}:job/anything"),
        format!("arn:aws:geo:{REGION}:{ACCOUNT}:not-a-location-type/anything"),
        format!("arn:aws:geo:{REGION}:{ACCOUNT}:tracker/"),
        format!("arn:aws:geo:{REGION}:{ACCOUNT}:tracker"),
    ];

    let router = emulator();
    // A tracker exists, so no refusal below is about an empty store.
    create(&router, &KINDS[0], "exists").await;

    for arn in &arns {
        assert_every_operation_refuses(&router, arn, arn, &[arn, "anything"]).await;
    }
}

#[tokio::test]
async fn the_arn_of_a_resource_its_own_delete_retired_earns_the_same_sentence() {
    let router = emulator();
    let name = "retired";
    let arn = create(&router, &KINDS[1], name).await;

    // Held tags first, so the refusal below is about the resource being gone rather than
    // about the plane never having seen the ARN.
    assert_no_content(name, "TagResource before the delete", &tag(&router, &arn, &[("env", "dev")]).await);

    let deleted = call(
        &router,
        "DELETE",
        &format!("/geofencing/v0/collections/{name}"),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, READ_OK,
        "the delete must succeed, got {} {:?}",
        deleted.status, deleted.body
    );

    assert_every_operation_refuses(&router, "a retired resource", &arn, &[name, &arn]).await;
}
