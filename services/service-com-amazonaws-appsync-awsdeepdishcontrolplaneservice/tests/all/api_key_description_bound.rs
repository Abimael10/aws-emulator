// SPDX-License-Identifier: CC0-1.0

//! The `description` an API key carries, end to end over HTTP.
//!
//! `CreateApiKey$description` and `UpdateApiKey$description` ("A description of the
//! purpose of the API key") both target a plain `String` with no modelled `@length`
//! (`smithy_query(query="operation", operation="CreateApiKey,UpdateApiKey")`), and both
//! operations model `BadRequestException` (400), so the bound on the member is the
//! service's.
//!
//! ONE rule, asserted over a TABLE of lengths spanning the class — the empty description,
//! lengths well inside the bound, the bound itself, and lengths past it: a description of
//! at most 256 characters is stored and read back verbatim, and a longer one is refused
//! with AppSync's own sentence, minting no key and leaving a stored key's description
//! alone. Each published bound and one past it is a row.
//!
//! Every assertion goes through the router the production emulator mounts. The harness
//! (`call`, `router`, `create_api`) is the error-contract suite's.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, router};

/// The longest description an API key accepts, which the service's own refusal names.
const DESCRIPTION_MAX_LEN: usize = 256;

/// AppSync's fixed sentence for a description past that ceiling.
const TOO_LONG: &str = "Invalid description given, must be less than 256 characters.";

/// Create one API key under `api_id` with `description`, and return the answer.
async fn create_key(
    router: &MultiServiceRouter,
    api_id: &str,
    description: Option<&str>,
) -> crate::error_contract::Answer {
    let mut body = json!({"expires": 0});
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/apikeys"),
        Some(body),
    )
    .await
}

/// The key ids `ListApiKeys` reports for `api_id`, in the order it reports them.
async fn listed_key_ids(router: &MultiServiceRouter, api_id: &str) -> Vec<String> {
    let listed = call(router, "GET", &format!("/v1/apis/{api_id}/apikeys"), None).await;
    listed.body["apiKeys"]
        .as_array()
        .map(|keys| {
            keys.iter()
                .map(|key| key["id"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Rule: a description inside the ceiling is stored verbatim and reported by the create
/// and by the listing alike.
#[tokio::test]
async fn a_description_inside_the_ceiling_is_stored_verbatim() {
    let router = router();
    let api_id = create_api(&router, "api-key-description-accepted").await;

    let mut minted = 0_usize;
    for length in [0_usize, 1, 64, DESCRIPTION_MAX_LEN - 1, DESCRIPTION_MAX_LEN] {
        let description = "d".repeat(length);
        let created = create_key(&router, &api_id, Some(&description)).await;
        assert_eq!(
            created.status, 200,
            "case a {length}-character description: expected 200, got {created:?}"
        );
        minted += 1;
        let reported = created.body["apiKey"]["description"].clone();
        assert_eq!(
            reported,
            json!(description),
            "case a {length}-character description: expected it reported back, got {reported:?}"
        );
        assert_eq!(
            listed_key_ids(&router, &api_id).await.len(),
            minted,
            "case a {length}-character description: expected {minted} listed keys"
        );
    }
}

/// Rule: a description past the ceiling is refused with the service's own sentence, and
/// mints no key — the listing holds only the keys that were accepted.
#[tokio::test]
async fn a_description_past_the_ceiling_is_refused_and_mints_no_key() {
    let router = router();
    let api_id = create_api(&router, "api-key-description-refused").await;

    // One accepted key, so the listing has something to stay equal to.
    let accepted = create_key(&router, &api_id, Some(&"d".repeat(DESCRIPTION_MAX_LEN))).await;
    assert_eq!(
        accepted.status, 200,
        "case the ceiling itself: expected 200, got {accepted:?}"
    );
    let accepted_id = accepted.body["apiKey"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for length in [
        DESCRIPTION_MAX_LEN + 1,
        DESCRIPTION_MAX_LEN + 2,
        DESCRIPTION_MAX_LEN * 4,
    ] {
        let description = "d".repeat(length);
        let created = create_key(&router, &api_id, Some(&description)).await;
        assert_eq!(
            (
                created.status,
                created.error_type.as_str(),
                created.message.as_str()
            ),
            (400, "BadRequestException", TOO_LONG),
            "case a {length}-character description: expected the ceiling refusal, got {created:?}"
        );
        assert_eq!(
            listed_key_ids(&router, &api_id).await,
            vec![accepted_id.clone()],
            "case a {length}-character description: expected only the accepted key to exist"
        );

        // The UPDATE path carries the same member under the same documentation, so it is
        // refused on the same ground and the stored description is left alone.
        let updated = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/apikeys/{accepted_id}"),
            Some(json!({"description": description, "expires": 0})),
        )
        .await;
        assert_eq!(
            (
                updated.status,
                updated.error_type.as_str(),
                updated.message.as_str()
            ),
            (400, "BadRequestException", TOO_LONG),
            "case a {length}-character description (update): expected the ceiling refusal, got \
             {updated:?}"
        );
        let read = call(&router, "GET", &format!("/v1/apis/{api_id}/apikeys"), None).await;
        assert_eq!(
            read.body["apiKeys"][0]["description"],
            json!("d".repeat(DESCRIPTION_MAX_LEN)),
            "case a {length}-character description (update): expected the stored description to \
             survive, got {:?}",
            read.body["apiKeys"][0]["description"]
        );
    }

    // A description at the ceiling still updates the key afterwards.
    let at_ceiling = "e".repeat(DESCRIPTION_MAX_LEN);
    let updated = call(
        &router,
        "POST",
        &format!("/v1/apis/{api_id}/apikeys/{accepted_id}"),
        Some(json!({"description": at_ceiling, "expires": 0})),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case an update at the ceiling: expected 200, got {updated:?}"
    );
    assert_eq!(
        updated.body["apiKey"]["description"],
        json!(at_ceiling),
        "case an update at the ceiling: expected the new description back, got {:?}",
        updated.body["apiKey"]["description"]
    );
}
