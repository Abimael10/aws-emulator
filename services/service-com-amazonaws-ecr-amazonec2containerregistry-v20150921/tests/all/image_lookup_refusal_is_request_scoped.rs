// SPDX-License-Identifier: CC0-1.0

//! What an image-not-found refusal of `DescribeImages` echoes does not depend on the REST
//! of the request, end to end over the wire.
//!
//! The rule under test: when an `imageIds` entry resolves to no stored image the call is
//! refused with `ImageNotFoundException` (HTTP 400) whose message states the identifier the
//! REQUEST sent — both members spelled out, a member the request did not carry rendered as
//! `null` — beside the repository the request named and the registry of the account that
//! holds it. The identifier is the request's own, so nothing else the call carries can
//! change it: not the `filter` the read asks for, not whether the request names
//! `registryId` explicitly, not which Region the call is made against, and not what other
//! repositories of the registry hold under the same tag.
//!
//! The suites beside this one in `refusal_wording` pin the echo across the identifier's own
//! shapes and across every operation that resolves one; these two pin that the rest of the
//! request is not an input to it, and close the table on the admitted side — the same
//! request resolves once the repository of that Region carries the image.
//!
//! The wire harness is `crate::wire`'s: every case goes through the router, so each
//! exercises routing, the boundary validator, the handler, the persisted state and
//! serialization — the same path a client takes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    at, call_in_region, emulator, given_layer_in_region, ok, ok_in_region, refused, REGION,
    REGISTRY_ID,
};

/// How a member of a rendered identifier reads when the request carried none.
const UNSET_MEMBER: &str = "null";

/// A well-formed content digest nothing was pushed under, so it is the IDENTIFIER that
/// misses and not the digest's form.
const UNSTORED_DIGEST: &str =
    "sha256:4f1b7d3e6a9c25800d1e4b7a3c6f9825be04d1a7c3e69f2580bd41e7a3c6f982";

/// The wording ECR refuses an unresolvable `imageId` with, written out literally rather
/// than imported so a wrong message in the implementation fails this suite. `digest` and
/// `tag` read exactly as the message states them, so each case states the identifier its
/// own request sent.
fn image_not_found_message(repository: &str, digest: &str, tag: &str) -> String {
    format!(
        "The image with imageId {{imageDigest:'{digest}', imageTag:'{tag}'}} does not exist \
         within the repository with name '{repository}' in the registry with id \
         '{REGISTRY_ID}'"
    )
}

/// The message a refusal carries, read from whichever member the body spells it in.
fn carried_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// A repository of `region`'s registry, created through the operation that creates one.
async fn given_repository(router: &MultiServiceRouter, case: &str, region: &str, name: &str) {
    ok_in_region(
        router,
        case,
        region,
        "CreateRepository",
        &json!({"repositoryName": name}),
    )
    .await;
}

/// Push one image carrying `tag` into `repository` of `region`'s registry — a layer through
/// the documented upload sequence, then the manifest that references it — and answer the
/// digest the registry registered the manifest under.
async fn given_image_in_region(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    repository: &str,
    tag: &str,
) -> String {
    let content = format!("layer-of-{region}-{repository}-{tag}").into_bytes();
    let layer = given_layer_in_region(router, case, region, repository, &content).await;
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "layers": [{
            "mediaType": "application/vnd.docker.image.rootfs.diff.tar.gzip",
            "digest": layer,
            "size": content.len(),
        }],
    })
    .to_string();
    let pushed = ok_in_region(
        router,
        case,
        region,
        "PutImage",
        &json!({
            "repositoryName": repository,
            "imageManifest": manifest,
            "imageTag": tag,
        }),
    )
    .await;
    at(&pushed, "/image/imageId/imageDigest")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the push must answer the stored digest"))
        .to_string()
}

/// Every `filter` a `DescribeImages` naming `imageIds` may carry, as the model declares the
/// two members and their values (`smithy_query(query="shape",
/// shape="DescribeImagesFilter,TagStatus,ImageStatusFilter")`: `tagStatus` is `TAGGED` |
/// `UNTAGGED` | `ANY`, `imageStatus` is `ACTIVE` | `ARCHIVED` | `ACTIVATING` | `ANY`), plus
/// the request that carries no filter at all and one carrying both members at once.
fn filters() -> Vec<(&'static str, Option<Value>)> {
    let mut rows: Vec<(&'static str, Option<Value>)> = vec![("no filter", None)];
    for tag_status in ["TAGGED", "UNTAGGED", "ANY"] {
        rows.push((tag_status, Some(json!({"tagStatus": tag_status}))));
    }
    for image_status in ["ACTIVE", "ARCHIVED", "ACTIVATING", "ANY"] {
        rows.push((image_status, Some(json!({"imageStatus": image_status}))));
    }
    rows.push((
        "both members",
        Some(json!({"tagStatus": "ANY", "imageStatus": "ANY"})),
    ));
    rows
}

/// An `imageIds` entry naming no stored image is refused with the identifier the request
/// sent, whatever else the request carries: the filter it asks the read under, whether it
/// names this account's registry explicitly, and what another repository of the registry
/// holds under the same tag.
///
/// The rows differ along what the rule generalises over: which members the identifier
/// carries (a tag alone — the reported instance — a digest alone, both together), every
/// value of each filter member the model declares crossed with a request carrying no filter
/// and one carrying both members, and whether the request names `registryId`.
#[tokio::test]
async fn the_echoed_identifier_does_not_depend_on_the_rest_of_the_request() {
    let repository = "echo-request-scoped";
    let sibling = "echo-request-scoped-sibling";
    let tag = "latest";

    let router = emulator();
    given_repository(&router, "the addressed repository", REGION, repository).await;
    // Another repository of the same registry DOES carry the tag, so a lookup that
    // resolved against the registry rather than against the repository the request named
    // would resolve here — and the refusal is still about the entry this request sent.
    given_repository(&router, "the sibling repository", REGION, sibling).await;
    given_image_in_region(&router, "the sibling repository", REGION, sibling, tag).await;

    // (case, the identifier sent, the digest the message must state, the tag it must state)
    let identifiers: Vec<(&str, Value, &str, &str)> = vec![
        // The reported instance: a tag-only entry in a repository holding no image.
        ("a tag alone", json!({"imageTag": tag}), UNSET_MEMBER, tag),
        (
            "a digest alone",
            json!({"imageDigest": UNSTORED_DIGEST}),
            UNSTORED_DIGEST,
            UNSET_MEMBER,
        ),
        (
            "a digest and a tag",
            json!({"imageDigest": UNSTORED_DIGEST, "imageTag": tag}),
            UNSTORED_DIGEST,
            tag,
        ),
    ];

    for (identifier, id, digest, echoed_tag) in identifiers {
        for (filter_case, filter) in filters() {
            for names_registry in [false, true] {
                let case = format!(
                    "{identifier} under the filter {filter_case}, \
                     registryId named: {names_registry}"
                );
                let mut request = json!({
                    "repositoryName": repository,
                    "imageIds": [id.clone()],
                });
                if let Some(filter) = filter.clone() {
                    request["filter"] = filter;
                }
                if names_registry {
                    request["registryId"] = json!(REGISTRY_ID);
                }

                let body = refused(
                    &router,
                    &case,
                    "DescribeImages",
                    &request,
                    "ImageNotFoundException",
                )
                .await;
                let expected = image_not_found_message(repository, digest, echoed_tag);
                let actual = carried_message(&body);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected the message {expected:?}, got {actual:?}"
                );
            }
        }
    }

    // The admitted side closes the table: the sibling repository really does carry that
    // tag, so the refusals above are about WHICH repository the request addressed and not
    // about a tag the registry never saw.
    let described = ok(
        &router,
        "the sibling repository resolves the same tag",
        "DescribeImages",
        &json!({"repositoryName": sibling, "imageIds": [{"imageTag": tag}]}),
    )
    .await;
    let reported = at(&described, "/imageDetails/0/imageTags");
    assert_eq!(
        reported,
        json!([tag]),
        "case {sibling:?}: expected the stored image to report the tag {tag:?}, got \
         {described:?}"
    );
}

/// A registry is per Region, so the SAME request is refused against the Region whose
/// repository holds no such image and resolves against the one whose repository does — and
/// the refusal echoes the request's identifier beside the repository the request named,
/// never anything read off the other Region's registry.
///
/// The rows differ along which Region holds the image and which one is asked, and along the
/// tag the request asks by.
#[tokio::test]
async fn the_echo_is_scoped_to_the_region_the_call_was_made_against() {
    // Two Regions, each with its own registry; `REGION` is the one the harness signs for by
    // default, and the second is any other.
    let other_region = "us-east-2";
    let repository = "echo-region-scoped";

    for tag in ["latest", "release-1.2_3"] {
        let router = emulator();
        given_repository(&router, tag, REGION, repository).await;
        given_repository(&router, tag, other_region, repository).await;
        // Only the OTHER Region's repository is given the image.
        let digest = given_image_in_region(&router, tag, other_region, repository, tag).await;

        let request = json!({"repositoryName": repository, "imageIds": [{"imageTag": tag}]});

        let case = format!("by {tag:?} against the Region holding no image");
        let answer = call_in_region(&router, REGION, "DescribeImages", &request).await;
        assert_eq!(
            (answer.status, answer.error_code()),
            (400, "ImageNotFoundException"),
            "case {case:?}: expected 400 ImageNotFoundException, got {} with {:?}",
            answer.status,
            answer.body
        );
        let expected = image_not_found_message(repository, UNSET_MEMBER, tag);
        let actual = carried_message(&answer.body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the message {expected:?}, got {actual:?}"
        );

        let case = format!("by {tag:?} against the Region holding the image");
        let described = ok_in_region(&router, &case, other_region, "DescribeImages", &request).await;
        assert_eq!(
            at(&described, "/imageDetails/0/imageDigest"),
            json!(digest),
            "case {case:?}: expected the stored digest {digest:?}, got {described:?}"
        );
        assert_eq!(
            at(&described, "/imageDetails/0/imageTags"),
            json!([tag]),
            "case {case:?}: expected the stored image to report the tag {tag:?}, got \
             {described:?}"
        );
    }
}
