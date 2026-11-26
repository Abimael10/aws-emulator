// SPDX-License-Identifier: CC0-1.0

//! WHICH error class the ARN-addressed tagging surface is refused with when its
//! `resourceArn` resolves to no resource.
//!
//! The rule this suite asserts: **an operation addressed only by the ARN of any taggable
//! resource is served by the shared tagging frontend in front of the service, so an ARN that
//! resolves to nothing is refused with that frontend's own `404 NotFoundException` — the
//! class leading `x-amzn-errortype` and naming the body's `__type` — carrying the frontend's
//! fixed sentence `Resource was not found` and no member naming the resource it looked for.**
//! An operation addressed by a TYPED identifier of one named class keeps that service's own
//! modelled `ResourceNotFoundException`, which does name the resource it looked for, so the
//! frontend's class stays scoped to the generic surface.
//!
//! Why the generic surface differs: the three tag operations of this model are bound to one
//! route whose only identifier is an `Arn` (`smithy_query(query="operation",
//! operation="TagResource,UntagResource,ListTagsForResource")` — `POST`, `DELETE` and `GET
//! /tags/{resourceArn}`, `resourceArn` the only `@httpLabel`), so nothing in such a request
//! says which class was meant; the sentence the recorded refusal carries names no class
//! either, where every class's own reader of this service states which class it was asked
//! for.
//!
//! The rule generalises over the operation, over the resource class the ARN spells and over
//! the FORM of the identifier, so the table spans all three tag operations and, for each,
//! identifiers differing in resource segment, region, account, resource-id form and width,
//! and whether they are an ARN at all. The recorded cases — a tag write, a tag read and a tag
//! removal against an ARN no resource carries — are three of its rows.

use crate::error_type_header_form::{answer, emulator, label, minimal_create, ACCOUNT, REGION};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;

/// The error class the shared tagging frontend refuses an unresolved ARN with, and the whole
/// sentence it states: fixed text, with nothing from the request in it.
const TAGGING_NOT_FOUND_CLASS: &str = "NotFoundException";
const TAGGING_NOT_FOUND_SENTENCE: &str = "Resource was not found";

/// The class a TYPED lookup of this service answers instead, as the model declares it
/// (`smithy_query(query="shape", shape="ResourceNotFoundException")`).
const MODELLED_NOT_FOUND_CLASS: &str = "ResourceNotFoundException";

/// The class a response names, however it names it: the leading token of
/// `x-amzn-errortype`, which is what a `restJson1` client resolves the error shape from.
fn leading_token(named: &str) -> &str {
    let bare = named.rsplit('#').next().unwrap_or(named);
    bare.split(':').next().unwrap_or(bare).trim()
}

/// The class the BODY names, read whether the renderer wrote it bare or namespaced.
fn body_class(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>");
    raw.rsplit('#').next().unwrap_or(raw).to_string()
}

/// One call of the ARN-addressed tagging surface, built for a given identifier.
struct TagCall {
    operation: &'static str,
    method: &'static str,
    path: fn(&str) -> String,
    body: fn() -> Option<Value>,
}

/// All three operations of the surface, each with the payload its own model requires
/// (`tags` is `@required` on `TagResource`; `tagKeys` is `@required @httpQuery` on
/// `UntagResource`; the read carries no payload).
fn tag_calls() -> Vec<TagCall> {
    vec![
        TagCall {
            operation: "TagResource",
            method: "POST",
            path: |arn| format!("/tags/{}", label(arn)),
            body: || Some(json!({"tags": {"Project": "lionfish"}})),
        },
        TagCall {
            operation: "UntagResource",
            method: "DELETE",
            path: |arn| format!("/tags/{}?tagKeys=Project", label(arn)),
            body: || None,
        },
        TagCall {
            operation: "ListTagsForResource",
            method: "GET",
            path: |arn| format!("/tags/{}", label(arn)),
            body: || None,
        },
    ]
}

/// Identifiers no resource of this emulator carries, differing in which resource segment
/// they spell, their region, their account, the form and width of their resource id, and
/// whether they are an ARN at all.
fn unresolved_identifiers() -> Vec<(&'static str, String)> {
    vec![
        (
            "a cluster ARN in the signing region",
            format!(
                "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster/11111111-2222-3333-4444-555555555555"
            ),
        ),
        (
            "a cluster ARN in another region",
            format!(
                "arn:aws:docdb-elastic:us-west-2:{ACCOUNT}:cluster/00000000-0000-0000-0000-000000000000"
            ),
        ),
        (
            "a snapshot ARN in another account",
            format!(
                "arn:aws:docdb-elastic:{REGION}:210987654321:cluster-snapshot/ABCDEF01-2345-6789-abcd-ef0123456789"
            ),
        ),
        (
            "a snapshot ARN whose resource id is a long word",
            format!(
                "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster-snapshot/{}",
                "z".repeat(120)
            ),
        ),
        (
            "an identifier that is not an ARN at all",
            "not-an-arn-at-all".to_string(),
        ),
    ]
}

/// A cluster that must be created, so every refusal below is about the ARN the request named
/// rather than about an empty store.
async fn present_cluster(router: &MultiServiceRouter, name: &str) -> String {
    let (status, _, created) = answer(router, "POST", "/cluster", Some(minimal_create(name))).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the fixture cluster must be created, got {status} with {created}"
    );
    created["cluster"]["clusterArn"]
        .as_str()
        .expect("the created cluster carries its ARN")
        .to_string()
}

#[tokio::test]
async fn the_tagging_surface_refuses_an_unresolved_arn_with_the_frontends_class() {
    let router = emulator();
    let _held = present_cluster(&router, "tagging-frontend-present").await;

    let mut classes = BTreeSet::new();
    for tag_call in tag_calls() {
        for (row, arn) in unresolved_identifiers() {
            let case = format!("{} / {row}", tag_call.operation);
            let (status, error_type, body) = answer(
                &router,
                tag_call.method,
                &(tag_call.path)(&arn),
                (tag_call.body)(),
            )
            .await;

            assert_eq!(
                status, 404,
                "case {case:?}: an unresolved ARN is HTTP 404, got {status} with {body}"
            );
            let named = error_type.clone().unwrap_or_else(|| {
                panic!("case {case:?}: a refusal must name its error class, got {body}")
            });
            assert_eq!(
                leading_token(&named),
                TAGGING_NOT_FOUND_CLASS,
                "case {case:?}: expected {TAGGING_NOT_FOUND_CLASS:?} to lead the error-type \
                 header, got {named:?}"
            );
            assert_eq!(
                body_class(&body),
                TAGGING_NOT_FOUND_CLASS,
                "case {case:?}: the body must name the same class, got {body}"
            );
            assert_eq!(
                body["message"].as_str(),
                Some(TAGGING_NOT_FOUND_SENTENCE),
                "case {case:?}: expected {TAGGING_NOT_FOUND_SENTENCE:?}, got {body}"
            );
            // The frontend was asked for no resource class of its own, so its refusal
            // reports neither the class nor the identifier back.
            assert!(
                body["resourceType"].is_null() && body["resourceId"].is_null(),
                "case {case:?}: the generic refusal names no resource, got {body}"
            );
            classes.insert(leading_token(&named).to_string());
        }
    }

    // ONE class serves the whole surface: a class chosen per operation or per ARN would
    // leave more than one here.
    assert_eq!(
        classes,
        [TAGGING_NOT_FOUND_CLASS.to_string()].into_iter().collect(),
        "case \"the whole tagging surface\": expected one class, got {classes:?}"
    );
}

#[tokio::test]
async fn a_typed_lookup_of_an_absent_resource_keeps_the_modelled_class() {
    // The scope of the rule: an operation addressed by a TYPED identifier of one named
    // class is not served by the generic frontend, so it keeps this service's own modelled
    // class and names the resource it looked for.
    let router = emulator();
    let _held = present_cluster(&router, "typed-lookup-present").await;

    let absent_cluster =
        format!("arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster/22222222-3333-4444-5555-666666666666");
    let absent_snapshot = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster-snapshot/77777777-8888-9999-aaaa-bbbbbbbbbbbb"
    );

    let typed: Vec<(&str, &str, String, &str)> = vec![
        (
            "GetCluster",
            "GET",
            format!("/cluster/{}", label(&absent_cluster)),
            absent_cluster.as_str(),
        ),
        (
            "DeleteCluster",
            "DELETE",
            format!("/cluster/{}", label(&absent_cluster)),
            absent_cluster.as_str(),
        ),
        (
            "GetClusterSnapshot",
            "GET",
            format!("/cluster-snapshot/{}", label(&absent_snapshot)),
            absent_snapshot.as_str(),
        ),
    ];

    for (case, method, path, identifier) in typed {
        let (status, error_type, body) = answer(&router, method, &path, None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: an absent resource is HTTP 404, got {status} with {body}"
        );
        let named = error_type.unwrap_or_default();
        assert_eq!(
            leading_token(&named),
            MODELLED_NOT_FOUND_CLASS,
            "case {case:?}: expected {MODELLED_NOT_FOUND_CLASS:?} to lead the error-type \
             header, got {named:?}"
        );
        assert_eq!(
            body["resourceId"].as_str(),
            Some(identifier),
            "case {case:?}: a typed refusal reports the identifier it was handed, got {body}"
        );
    }
}

#[tokio::test]
async fn a_tag_call_on_an_arn_that_does_resolve_is_served() {
    // The positive control: the refusal above is about the ARN resolving to nothing, not
    // about the surface. An ARN this emulator does hold takes a tag write, reports it back
    // and names no error class at all.
    let router = emulator();
    let held = present_cluster(&router, "tagging-frontend-served").await;

    let (status, error_type, written) = answer(
        &router,
        "POST",
        &format!("/tags/{}", label(&held)),
        Some(json!({"tags": {"Project": "lionfish"}})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"TagResource on a held ARN\": expected HTTP 200, got {status} with {written}"
    );
    assert_eq!(
        error_type, None,
        "case \"TagResource on a held ARN\": a success names no error class, got {error_type:?}"
    );

    let (status, error_type, listed) =
        answer(&router, "GET", &format!("/tags/{}", label(&held)), None).await;
    assert_eq!(
        status, 200,
        "case \"ListTagsForResource on a held ARN\": expected HTTP 200, got {status} with {listed}"
    );
    assert_eq!(
        error_type, None,
        "case \"ListTagsForResource on a held ARN\": a success names no error class, got \
         {error_type:?}"
    );
    assert_eq!(
        listed["tags"]["Project"].as_str(),
        Some("lionfish"),
        "case \"ListTagsForResource on a held ARN\": expected the written tag back, got {listed}"
    );
}
