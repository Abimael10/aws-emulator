// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the identities this service MINTS: a MicroVM image's `id`, and
//! the handles it draws for the resources it names by a prefix (a MicroVM's `microvmId`,
//! a build request's `buildId`).
//!
//! RULE: every response that reports a stored MicroVM IMAGE record carries that
//! record's own `id` — a lowercase hyphenated 8-4-4-4-12 version-4 UUID, minted once
//! when the image is created, unique across the images an account holds and
//! byte-identical in every later answer about that image (the read, every listing row,
//! an update that mints a further version, and an idempotent repeat of the create).
//!
//! RULE: every other identifier this service mints is DRAWN for its resource under that
//! resource's own prefix rather than rendered from how many the store has minted before
//! it, so a listing pages in the order the records were created rather than in the order
//! their identifiers sort.
//!
//! The member is RECORDED rather than modelled: `CreateMicrovmImageResponse`,
//! `GetMicrovmImageOutput`, `UpdateMicrovmImageResponse` and `MicrovmImageSummary`
//! declare no `id` (`smithy_query(query="shape",
//! shape="CreateMicrovmImageResponse,GetMicrovmImageOutput,MicrovmImageSummary",
//! model_path="services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json")`),
//! so it is written beside the modelled payload.
//!
//! Each test states that rule and exercises it over a SET of inputs; the values any one
//! row happens to carry are examples, not the specification. The wire harness
//! (`emulator`, `call`, `image_body`, …) is the one `microvm_lifecycle` owns.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    managed_base_image_arn, BUILD_ID_PREFIX, FIRST_IMAGE_VERSION, MANAGED_BASE_IMAGE_IDS,
    MICROVM_ID_PREFIX,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_image_and_instance_paths::get_image;
use crate::microvm_lifecycle::{
    as_uri_value, built_image, call, emulator, expected_image_arn, image_body, run_microvm,
    PAST_THE_SPAN, REGION,
};

/// Whether `value` is the id form the service mints: 36 characters in the lowercase
/// hyphenated `8-4-4-4-12` groups of a version-4 UUID — hexadecimal throughout, the
/// version nibble `4` and the variant nibble one of `8`, `9`, `a`, `b` (RFC 4122, which
/// is the layout `uuid::Builder::from_random_bytes` writes).
fn is_minted_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    if lengths != vec![8, 4, 4, 4, 12] {
        return false;
    }
    let hexadecimal = groups
        .iter()
        .all(|group| group.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    let version = groups[2].starts_with('4');
    let variant = groups[3].starts_with(['8', '9', 'a', 'b']);
    hexadecimal && version && variant
}

/// The `id` of a response body, failing the case when it is absent or not a string.
fn id_of(body: &Value, case: &str) -> String {
    let id = body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: the answer carries an id, got {body}"));
    assert!(
        is_minted_id(id),
        "case {case}: expected a lowercase 8-4-4-4-12 UUID, got {id:?}"
    );
    id.to_string()
}

/// `GET /2025-09-09/microvm-images`, with whatever `query` adds to the query string.
async fn list_images(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    let uri = match query.is_empty() {
        true => "/2025-09-09/microvm-images".to_string(),
        false => format!("/2025-09-09/microvm-images?{query}"),
    };
    call(router, "GET", &uri, None).await
}

/// The `id` the listing reports for the row named `name`, selected by name exactly as a
/// reconciliation sweep would.
async fn listed_id(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let (status, body) = list_images(router, "").await;
    assert_eq!(status, 200, "case {case}: listing got {status} {body}");
    let row = body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: items is a list, got {body}"))
        .iter()
        .find(|item| item["name"].as_str() == Some(name))
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: the listing carries a row for {name}, got {body}"));
    id_of(&row, case)
}

/// `PUT /2025-09-09/microvm-images/{imageIdentifier}` with a configuration the update
/// accepts, which mints a further version of the image. The base image is the one the
/// AWS-managed catalogue publishes (`state::MANAGED_BASE_IMAGE_IDS`), in the region the
/// harness signs for.
async fn update_image(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    call(
        router,
        "PUT",
        &format!("/2025-09-09/microvm-images/{}", as_uri_value(identifier)),
        Some(json!({
            "baseImageArn": managed_base_image_arn(REGION, MANAGED_BASE_IMAGE_IDS[0]),
            "buildRoleArn": "arn:aws:iam::123456789012:role/microvm-build",
            "codeArtifact": {"uri": "s3://artifacts/app-2.zip"},
        })),
    )
    .await
}

/// RULE: the create answers the image's own id, and the READ, the LISTING ROW and an
/// UPDATE of that image all answer the SAME id — so a client that keyed its records on
/// the create can validate the identity and join a listing back to what it created.
#[tokio::test]
async fn one_image_reports_one_id_through_every_projection() {
    let router = emulator();

    // Rows span what the create can differ in — the name's published bounds and
    // punctuation (`ImageName` is len 1..64, pattern ^[a-zA-Z0-9-_]+$), which optional
    // members the request carries, and whether it is tagged — because none of them is
    // what the id is derived from.
    let longest = format!("ident-{}", "x".repeat(58));
    let rows: Vec<(&str, String, Value)> = vec![
        ("required members only", "a".to_string(), json!({})),
        ("name at the 64-character maximum", longest, json!({})),
        (
            "underscores, hyphens and digits",
            "id_0-1_2".to_string(),
            json!({"description": "a described image"}),
        ),
        (
            "every optional member",
            "fully-identified".to_string(),
            json!({
                "baseImageVersion": "7",
                "description": "fully specified",
                "logging": {"cloudWatch": {"logGroup": "/aws/microvm/demo"}},
                "egressNetworkConnectors": ["connector-egress"],
                "cpuConfigurations": [{"architecture": "ARM_64"}],
                "resources": [{"minimumMemoryInMiB": 2048}],
                "additionalOsCapabilities": ["ALL"],
                "hooks": {"port": 8080},
                "environmentVariables": {"STAGE": "prod"},
                "tags": {"team": "platform"},
            }),
        ),
    ];

    let mut created: Vec<(String, String)> = Vec::new();
    for (case, name, extra) in rows {
        let (status, body) = call(
            &router,
            "POST",
            "/2025-09-09/microvm-images",
            Some(image_body(&name, extra)),
        )
        .await;
        assert_eq!(status, 201, "case {case}: create got {status} {body}");
        created.push((name, id_of(&body, case)));
    }

    // Every id is distinct: the id is the image's identity, not a value derived from
    // anything two images can share.
    let distinct: BTreeSet<&String> = created.iter().map(|(_, id)| id).collect();
    assert_eq!(
        distinct.len(),
        created.len(),
        "each image gets its own id, got {created:?}"
    );

    // One asynchronous build span covers every row, so it is waited out once: the read
    // below therefore also crosses the clock-owed CREATING -> CREATED transition, which
    // rewrites the record and must not rewrite its identity.
    tokio::time::sleep(PAST_THE_SPAN).await;

    for (name, minted) in &created {
        // Both forms `MicrovmImageIdentifier` accepts resolve to the one record, so
        // both answer the one id.
        for identifier in [name.clone(), expected_image_arn(name)] {
            let case = format!("read {identifier:?}");
            let (status, body) = get_image(&router, &identifier).await;
            assert_eq!(status, 200, "case {case}: got {status} {body}");
            assert_eq!(
                &id_of(&body, &case),
                minted,
                "case {case}: expected the id the create answered ({minted}), got {body}"
            );
        }

        let case = format!("listing row for {name:?}");
        assert_eq!(
            &listed_id(&router, name, &case).await,
            minted,
            "case {case}: expected the id the create answered ({minted})"
        );

        let case = format!("update {name:?}");
        let (status, body) = update_image(&router, name).await;
        assert_eq!(status, 200, "case {case}: got {status} {body}");
        assert_eq!(
            &id_of(&body, &case),
            minted,
            "case {case}: an update mints a version, not an identity, got {body}"
        );
    }

    // And the identity outlives the version the update is building: the read after it
    // still answers the id the create minted.
    tokio::time::sleep(PAST_THE_SPAN).await;
    for (name, minted) in &created {
        let case = format!("read after the update {name:?}");
        let (status, body) = get_image(&router, name).await;
        assert_eq!(status, 200, "case {case}: got {status} {body}");
        assert_eq!(
            &id_of(&body, &case),
            minted,
            "case {case}: expected the id the create answered ({minted}), got {body}"
        );
    }
}

/// RULE: the id belongs to the stored record, so an `@idempotencyToken` repeat of a
/// create — which answers the record that token already minted rather than minting a
/// second one — answers that record's id, and a read is a pure report: repeating it
/// changes nothing a later caller sees.
#[tokio::test]
async fn a_replayed_create_and_a_repeated_read_answer_the_same_id() {
    let router = emulator();

    // Rows differ in the client token's form, which is an opaque string the service
    // only compares: a plain word, a UUID-shaped token, and one carrying punctuation.
    let rows: Vec<(&str, &str, &str)> = vec![
        ("a plain token", "replayed-plain", "token-1"),
        (
            "a uuid-shaped token",
            "replayed-uuid",
            "6f1a0e1e-0000-4000-8000-00000000abcd",
        ),
        ("a token with punctuation", "replayed-punct", "a.b_c-d:1/2"),
    ];

    for (case, name, token) in rows {
        let body = image_body(name, json!({"clientToken": token}));
        let (status, first) = call(&router, "POST", "/2025-09-09/microvm-images", Some(body.clone()))
            .await;
        assert_eq!(status, 201, "case {case}: create got {status} {first}");
        let minted = id_of(&first, case);

        let (status, replayed) =
            call(&router, "POST", "/2025-09-09/microvm-images", Some(body)).await;
        assert_eq!(status, 201, "case {case}: replay got {status} {replayed}");
        assert_eq!(
            id_of(&replayed, case),
            minted,
            "case {case}: a replay answers the record the token minted, got {replayed}"
        );

        let (status, read) = get_image(&router, name).await;
        assert_eq!(status, 200, "case {case}: read got {status} {read}");
        let (status, again) = get_image(&router, name).await;
        assert_eq!(status, 200, "case {case}: re-read got {status} {again}");
        assert_eq!(
            read, again,
            "case {case}: a read reports and changes nothing, got {again}"
        );
        assert_eq!(
            id_of(&again, case),
            minted,
            "case {case}: expected the id the create answered ({minted}), got {again}"
        );
    }
}

/// RULE: the id is reported for every row the listing serves, however the pages are
/// cut — a reconciliation sweep that walks the listing can match every row to the
/// resource it created.
#[tokio::test]
async fn every_listing_row_of_every_page_carries_its_images_id() {
    let router = emulator();

    // Five images, so the walk below cuts the listing at sizes that leave one, several
    // and no further pages.
    let mut minted: Vec<(String, String)> = Vec::new();
    for index in 0..5 {
        let name = format!("page-row-{index}");
        let (status, body) = call(
            &router,
            "POST",
            "/2025-09-09/microvm-images",
            Some(image_body(&name, json!({}))),
        )
        .await;
        assert_eq!(status, 201, "create {name}: got {status} {body}");
        minted.push((name, id_of(&body, "create")));
    }

    // Rows differ in the page size, which is what the walk generalises over.
    for size in [None, Some(1), Some(2), Some(5), Some(50)] {
        let case = format!("maxResults={size:?}");
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;

        for _ in 0..=minted.len() {
            let mut query: Vec<String> = Vec::new();
            if let Some(size) = size {
                query.push(format!("maxResults={size}"));
            }
            if let Some(token) = &token {
                query.push(format!("nextToken={token}"));
            }
            let (status, body) = list_images(&router, &query.join("&")).await;
            assert_eq!(status, 200, "case {case}: got {status} {body}");
            for row in body["items"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case}: items is a list, got {body}"))
            {
                let name = row["name"].as_str().unwrap_or_default().to_string();
                seen.push((name, id_of(row, &case)));
            }
            token = body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }

        let mut expected = minted.clone();
        expected.sort();
        seen.sort();
        assert_eq!(
            seen, expected,
            "case {case}: every row carries the id its image was created with"
        );
    }
}

/// Whether `value` is the handle form this service mints for a resource it names by a
/// short prefix: that prefix, then a drawn UUID in the same lowercase hyphenated
/// 8-4-4-4-12 form a MicroVM image's own `id` carries ([`is_minted_id`]). The prefixes are
/// the ones the service publishes (`state::MICROVM_ID_PREFIX`,
/// `state::BUILD_ID_PREFIX`), read from the service rather than spelled out here.
fn is_minted_handle(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(is_minted_id)
}

/// The drawn half of a minted handle: everything after its prefix.
fn drawn_half(value: &str, prefix: &str) -> String {
    value.strip_prefix(prefix).unwrap_or(value).to_string()
}

/// `GET /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}/builds`.
async fn list_builds(router: &MultiServiceRouter, identifier: &str, version: &str) -> (u16, Value) {
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions/{version}/builds",
        as_uri_value(identifier)
    );
    call(router, "GET", &uri, None).await
}

/// The `buildId`s the build listing of `version` of the image `identifier` serves, in the
/// order the page carries them.
async fn listed_build_ids(
    router: &MultiServiceRouter,
    identifier: &str,
    version: &str,
    case: &str,
) -> Vec<String> {
    let (status, body) = list_builds(router, identifier, version).await;
    assert_eq!(status, 200, "case {case}: the build listing got {status} {body}");
    body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: items is a list, got {body}"))
        .iter()
        .map(|row| {
            row["buildId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a row carries a buildId, got {row}"))
                .to_string()
        })
        .collect()
}

/// The `microvmId`s `ListMicrovms` serves, in the order the page carries them.
async fn listed_microvm_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = call(router, "GET", "/2025-09-09/microvms", None).await;
    assert_eq!(status, 200, "case {case}: the MicroVM listing got {status} {body}");
    body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: items is a list, got {body}"))
        .iter()
        .map(|row| {
            row["microvmId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a row carries a microvmId, got {row}"))
                .to_string()
        })
        .collect()
}

/// RULE: every identifier this service MINTS for a resource of its own — a MicroVM's
/// `microvmId`, a build request's `buildId` — is DRAWN per resource under that resource's
/// prefix rather than rendered from how many the store has minted before it: the
/// identifiers of one store are distinct and carry no ordering, so a listing pages in the
/// order the records were created rather than in the order their identifiers sort.
#[tokio::test]
async fn every_minted_handle_is_drawn_rather_than_counted() {
    let router = emulator();
    let image_arn = built_image(&router, "drawn-handles").await;

    // Rows: one MicroVM per launch, enough launches that a position a counter would
    // leave constant shows up as one here, and enough that the creation order and the
    // identifier order are independent.
    const LAUNCHES: usize = 8;
    let mut launched: Vec<String> = Vec::new();
    for index in 0..LAUNCHES {
        let case = format!("launch {index}");
        let (status, body) = run_microvm(&router, json!({"imageIdentifier": image_arn})).await;
        assert_eq!(status, 200, "case {case}: RunMicrovm got {status} {body}");
        let microvm_id = body["microvmId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the answer carries a microvmId, got {body}"))
            .to_string();
        assert!(
            is_minted_handle(&microvm_id, MICROVM_ID_PREFIX),
            "case {case}: expected {MICROVM_ID_PREFIX:?} and a drawn UUID, got {microvm_id:?}"
        );
        launched.push(microvm_id);
    }

    let distinct: BTreeSet<&String> = launched.iter().collect();
    assert_eq!(
        distinct.len(),
        launched.len(),
        "each MicroVM gets its own id, got {launched:?}"
    );

    // No DRAWN position of a minted identifier is fixed: a counter-rendered one leaves its
    // leading characters constant for every resource a store ever mints. The positions the
    // UUID form itself fixes are not drawn — the group separators, the version nibble that
    // opens the third group and the variant nibble that opens the fourth — and they are
    // what `is_minted_id` pins instead.
    const VERSION_NIBBLE: usize = 14;
    const VARIANT_NIBBLE: usize = 19;
    let drawn: Vec<String> = launched
        .iter()
        .map(|id| drawn_half(id, MICROVM_ID_PREFIX))
        .collect();
    for position in 0..drawn[0].chars().count() {
        if position == VERSION_NIBBLE || position == VARIANT_NIBBLE {
            continue;
        }
        let seen: BTreeSet<Option<char>> =
            drawn.iter().map(|id| id.chars().nth(position)).collect();
        if seen == BTreeSet::from([Some('-')]) {
            continue;
        }
        assert!(
            seen.len() > 1,
            "case position {position}: expected the drawn character to vary over \
             {LAUNCHES} mints, got {seen:?} from {launched:?}"
        );
    }

    // The identifiers carry no order, so the listing's own order is the order the
    // MicroVMs were RUN — which is what a client paging the listing walks.
    assert_eq!(
        listed_microvm_ids(&router, "creation order").await,
        launched,
        "the listing pages the MicroVMs in the order they were run, got ids {launched:?}"
    );

    // The same rule on the other identifier this service mints: the build requests one
    // image version's build fans out into.
    let builds = listed_build_ids(&router, &image_arn, FIRST_IMAGE_VERSION, "builds").await;
    assert!(
        !builds.is_empty(),
        "the version's build fans out into at least one build request, got {builds:?}"
    );
    for build_id in &builds {
        assert!(
            is_minted_handle(build_id, BUILD_ID_PREFIX),
            "case build {build_id:?}: expected {BUILD_ID_PREFIX:?} and a drawn UUID, got \
             {build_id:?}"
        );
        // The drawn identifier is the one handle on that build: the single-build read is
        // addressed by it and echoes it back unchanged.
        let uri = format!(
            "/2025-09-09/microvm-images/{}/versions/{FIRST_IMAGE_VERSION}/builds/{build_id}",
            as_uri_value(&image_arn)
        );
        let (status, body) = call(&router, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "case build {build_id:?}: the build read got {status} {body}"
        );
        assert_eq!(
            body["buildId"].as_str(),
            Some(build_id.as_str()),
            "case build {build_id:?}: the read echoes the identifier it was addressed by, \
             got {body}"
        );
    }
    let distinct_builds: BTreeSet<&String> = builds.iter().collect();
    assert_eq!(
        distinct_builds.len(),
        builds.len(),
        "each build request gets its own id, got {builds:?}"
    );
}

/// RULE: a minted identifier is the resource's own rather than its position in a store,
/// so two INDEPENDENT stores — what a restarted emulator is — mint different identifiers
/// for the resources they hold: a client that distinguishes two MicroVMs created in
/// separate runs never sees one identifier for both.
#[tokio::test]
async fn two_independent_stores_mint_different_handles() {
    // Rows: the two identifiers this service mints, each read from the FIRST resource of
    // its kind in each store — the one a counter would render identically in both.
    let mut first_microvm: Vec<String> = Vec::new();
    let mut first_builds: Vec<Vec<String>> = Vec::new();

    for store in 0..2 {
        let case = format!("store {store}");
        let router = emulator();
        let image_arn = built_image(&router, "separate-store").await;
        let (status, body) = run_microvm(&router, json!({"imageIdentifier": image_arn})).await;
        assert_eq!(status, 200, "case {case}: RunMicrovm got {status} {body}");
        first_microvm.push(
            body["microvmId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: the answer carries a microvmId, got {body}"))
                .to_string(),
        );
        first_builds.push(listed_build_ids(&router, &image_arn, FIRST_IMAGE_VERSION, &case).await);
    }

    assert_ne!(
        first_microvm[0], first_microvm[1],
        "two stores mint different MicroVM ids, got {first_microvm:?}"
    );
    let shared: Vec<&String> = first_builds[0]
        .iter()
        .filter(|build_id| first_builds[1].contains(build_id))
        .collect();
    assert!(
        shared.is_empty(),
        "two stores mint different build ids, got {shared:?} in both of {first_builds:?}"
    );
}
