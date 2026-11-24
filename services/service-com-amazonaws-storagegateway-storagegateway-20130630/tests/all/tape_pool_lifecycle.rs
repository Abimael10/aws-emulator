// SPDX-License-Identifier: CC0-1.0

//! The Storage Gateway custom-tape-pool lifecycle, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule under test: `CreateTapePool` persists a custom tape pool whose
//! members are the ones the create carried (with `RetentionLockType` and
//! `RetentionLockTimeInDays` filled from their own documented no-lock values when the
//! request names neither), `ListTapePools` computes every `PoolInfo` it reports from that
//! stored record, `DeleteTapePool` removes it, and a `PoolARN` that names no stored pool
//! is refused rather than answered.
//!
//! The request harness (`emulator`, `call`, `Answer`, the signing constants and the ARN
//! shape check) is the sibling suite's — see `gateway_nfs_share_lifecycle`.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_storagegateway_storagegateway_20130630::state::MAX_TAPE_POOLS;

use crate::gateway_nfs_share_lifecycle::{
    assert_not_found, call, emulator, is_resource_arn, Answer, ACCOUNT_ID,
    INVALID_GATEWAY_REQUEST, REGION,
};

/// The resource type a `PoolARN` names.
const POOL_RESOURCE: &str = "tapepool";

/// A pool ARN of the right shape that no create ever minted.
const NEVER_CREATED_POOL: &str =
    "arn:aws:storagegateway:eu-west-1:000000000000:tapepool/pool-DEADBEEF";

/// The `pool-` + hex form a minted custom tape pool id takes. Checked here rather than
/// through the crate's own code so a wrong id cannot make this test pass.
fn is_pool_id(value: &str) -> bool {
    value
        .strip_prefix("pool-")
        .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Create one custom tape pool and answer its `PoolARN`.
async fn create_pool(router: &smithy_server_runtime::MultiServiceRouter, request: Value) -> String {
    let answer = call(router, "CreateTapePool", request.clone()).await;
    assert_eq!(
        answer.status, 200,
        "expected HTTP 200 from CreateTapePool for {request:?}, got {} with {:?}",
        answer.status, answer.body
    );
    let arn = answer.string("PoolARN");
    assert!(
        is_resource_arn(&arn, POOL_RESOURCE, is_pool_id),
        "case {request:?}: CreateTapePool must answer a {POOL_RESOURCE} ARN of this \
         account and region, got {arn:?}"
    );
    arn
}

/// The `PoolInfo` entries one `ListTapePools` request reports.
async fn list_pools(
    router: &smithy_server_runtime::MultiServiceRouter,
    request: Value,
) -> (Answer, Vec<Value>) {
    let answer = call(router, "ListTapePools", request.clone()).await;
    assert_eq!(
        answer.status, 200,
        "expected HTTP 200 from ListTapePools for {request:?}, got {} with {:?}",
        answer.status, answer.body
    );
    let infos = answer.body["PoolInfos"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {request:?}: ListTapePools must report a PoolInfos array, got {:?}",
                answer.body
            )
        });
    (answer, infos)
}

/// The one `PoolInfo` `ListTapePools` reports for `arn`.
async fn describe_pool(router: &smithy_server_runtime::MultiServiceRouter, arn: &str) -> Value {
    let (_, infos) = list_pools(router, json!({ "PoolARNs": [arn] })).await;
    assert_eq!(
        infos.len(),
        1,
        "ListTapePools must report exactly the one pool asked for ({arn}), got {infos:?}"
    );
    infos[0].clone()
}

/// Every `PoolARN` `ListTapePools` reports, following `Marker` to the end.
async fn all_listed_pools(
    router: &smithy_server_runtime::MultiServiceRouter,
    limit: Option<i32>,
) -> Vec<String> {
    let mut found = Vec::new();
    let mut marker: Option<String> = None;
    loop {
        let mut request = json!({});
        if let Some(limit) = limit {
            request["Limit"] = json!(limit);
        }
        if let Some(marker) = &marker {
            request["Marker"] = json!(marker);
        }
        let (answer, infos) = list_pools(router, request).await;
        for info in &infos {
            found.push(info["PoolARN"].as_str().unwrap_or_default().to_string());
        }
        match answer.body["Marker"].as_str() {
            Some(next) => marker = Some(next.to_string()),
            None => return found,
        }
        assert!(
            found.len() < 10_000,
            "ListTapePools pagination did not terminate, walked {} pools",
            found.len()
        );
    }
}

/// One create request, and the `PoolInfo` members the pool it mints must then report.
///
/// The rows differ along everything the rule generalises over: both `TapeStorageClass`
/// values, every `RetentionLockType` value plus its absence, the published bounds of
/// `RetentionLockTimeInDays` (0..36500) plus its absence, the `PoolName` length bounds
/// (1..100) and the punctuation its pattern admits, and tags present or absent. The value
/// lists come from `TapeStorageClass`, `RetentionLockType`, `RetentionLockTimeInDays` and
/// `PoolName` in this service's Smithy model.
struct Case {
    name: &'static str,
    request: Value,
    expected: Value,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "glacier, no retention lock members",
            request: json!({ "PoolName": "archive-glacier", "StorageClass": "GLACIER" }),
            expected: json!({
                "PoolName": "archive-glacier",
                "StorageClass": "GLACIER",
                "RetentionLockType": "NONE",
                "RetentionLockTimeInDays": 0,
                "PoolStatus": "ACTIVE",
            }),
        },
        Case {
            name: "deep archive, governance lock for one day",
            request: json!({
                "PoolName": "archive-deep-governance",
                "StorageClass": "DEEP_ARCHIVE",
                "RetentionLockType": "GOVERNANCE",
                "RetentionLockTimeInDays": 1,
            }),
            expected: json!({
                "PoolName": "archive-deep-governance",
                "StorageClass": "DEEP_ARCHIVE",
                "RetentionLockType": "GOVERNANCE",
                "RetentionLockTimeInDays": 1,
                "PoolStatus": "ACTIVE",
            }),
        },
        Case {
            name: "deep archive, compliance lock at the 36500-day maximum",
            request: json!({
                "PoolName": "archive-deep-compliance",
                "StorageClass": "DEEP_ARCHIVE",
                "RetentionLockType": "COMPLIANCE",
                "RetentionLockTimeInDays": 36_500,
            }),
            expected: json!({
                "PoolName": "archive-deep-compliance",
                "StorageClass": "DEEP_ARCHIVE",
                "RetentionLockType": "COMPLIANCE",
                "RetentionLockTimeInDays": 36_500,
                "PoolStatus": "ACTIVE",
            }),
        },
        Case {
            name: "glacier, lock type NONE and zero days spelled out",
            request: json!({
                "PoolName": "archive-glacier-unlocked",
                "StorageClass": "GLACIER",
                "RetentionLockType": "NONE",
                "RetentionLockTimeInDays": 0,
            }),
            expected: json!({
                "PoolName": "archive-glacier-unlocked",
                "StorageClass": "GLACIER",
                "RetentionLockType": "NONE",
                "RetentionLockTimeInDays": 0,
                "PoolStatus": "ACTIVE",
            }),
        },
        Case {
            name: "name of one character, PoolName's minimum length",
            request: json!({ "PoolName": "p", "StorageClass": "GLACIER" }),
            expected: json!({
                "PoolName": "p",
                "StorageClass": "GLACIER",
                "RetentionLockType": "NONE",
                "RetentionLockTimeInDays": 0,
                "PoolStatus": "ACTIVE",
            }),
        },
        Case {
            name: "name of 100 characters, PoolName's maximum length",
            request: json!({ "PoolName": "n".repeat(100), "StorageClass": "DEEP_ARCHIVE" }),
            expected: json!({
                "PoolName": "n".repeat(100),
                "StorageClass": "DEEP_ARCHIVE",
                "RetentionLockType": "NONE",
                "RetentionLockTimeInDays": 0,
                "PoolStatus": "ACTIVE",
            }),
        },
        Case {
            name: "name with spaces and the punctuation PoolName's pattern admits",
            request: json!({
                "PoolName": "tape pool +-=._:@ 2026",
                "StorageClass": "GLACIER",
                "RetentionLockType": "GOVERNANCE",
                "RetentionLockTimeInDays": 90,
            }),
            expected: json!({
                "PoolName": "tape pool +-=._:@ 2026",
                "StorageClass": "GLACIER",
                "RetentionLockType": "GOVERNANCE",
                "RetentionLockTimeInDays": 90,
                "PoolStatus": "ACTIVE",
            }),
        },
        Case {
            name: "tags carried by the create",
            request: json!({
                "PoolName": "archive-tagged",
                "StorageClass": "DEEP_ARCHIVE",
                "RetentionLockType": "COMPLIANCE",
                "RetentionLockTimeInDays": 7,
                "Tags": [
                    { "Key": "team", "Value": "backup" },
                    { "Key": "cost-center", "Value": "4711" },
                ],
            }),
            expected: json!({
                "PoolName": "archive-tagged",
                "StorageClass": "DEEP_ARCHIVE",
                "RetentionLockType": "COMPLIANCE",
                "RetentionLockTimeInDays": 7,
                "PoolStatus": "ACTIVE",
            }),
        },
    ]
}

/// Every member of the created pool comes back from `ListTapePools`, and every pool gets
/// its own ARN.
#[tokio::test]
async fn create_tape_pool_persists_every_member() {
    let router = emulator();
    let mut arns = BTreeSet::new();

    for case in cases() {
        let Case {
            name,
            request,
            expected,
        } = case;
        let arn = create_pool(&router, request.clone()).await;
        assert!(
            arns.insert(arn.clone()),
            "case {name}: CreateTapePool must mint a fresh PoolARN, got the already \
             minted {arn:?}"
        );

        let info = describe_pool(&router, &arn).await;
        assert_eq!(
            info["PoolARN"].as_str(),
            Some(arn.as_str()),
            "case {name}: ListTapePools must report the ARN the create answered, got {info:?}"
        );
        for (member, want) in expected.as_object().expect("the expected members are an object") {
            assert_eq!(
                &info[member], want,
                "case {name}: expected {member} {want:?}, got {:?}",
                info[member]
            );
        }
    }

    // Each create added exactly one pool, and no reporting call invented or lost one.
    let listed: BTreeSet<String> = all_listed_pools(&router, None).await.into_iter().collect();
    assert_eq!(
        listed, arns,
        "ListTapePools must report exactly the pools the creates minted, got {listed:?}"
    );
}

/// `PoolARNs` selects the pools to report, and nothing else.
#[tokio::test]
async fn list_tape_pools_reports_only_the_pools_asked_for() {
    let router = emulator();

    let (answer, infos) = list_pools(&router, json!({})).await;
    assert!(
        infos.is_empty() && answer.body["Marker"].as_str().is_none(),
        "with no pool created, ListTapePools must report an empty PoolInfos and no \
         Marker, got {:?}",
        answer.body
    );

    let mut arns = Vec::new();
    for index in 0..5 {
        arns.push(
            create_pool(
                &router,
                json!({
                    "PoolName": format!("selection-{index}"),
                    "StorageClass": if index % 2 == 0 { "GLACIER" } else { "DEEP_ARCHIVE" },
                }),
            )
            .await,
        );
    }

    // A subset, the whole set, one pool, and a pool ARN nothing minted: in every case the
    // answer carries exactly the stored pools the request named.
    let selections: Vec<Vec<String>> = vec![
        vec![arns[0].clone()],
        vec![arns[1].clone(), arns[4].clone()],
        arns.clone(),
        vec![NEVER_CREATED_POOL.to_string()],
        vec![arns[2].clone(), NEVER_CREATED_POOL.to_string()],
    ];
    for selection in selections {
        let (_, infos) = list_pools(&router, json!({ "PoolARNs": selection })).await;
        let reported: BTreeSet<String> = infos
            .iter()
            .map(|info| info["PoolARN"].as_str().unwrap_or_default().to_string())
            .collect();
        let wanted: BTreeSet<String> = selection
            .iter()
            .filter(|arn| arns.contains(arn))
            .cloned()
            .collect();
        assert_eq!(
            reported, wanted,
            "case {selection:?}: ListTapePools must report exactly the named stored \
             pools, got {reported:?}"
        );
    }
}

/// `Limit` sizes a page and `Marker` walks the rest, whatever the page size.
#[tokio::test]
async fn list_tape_pools_paginates_every_pool() {
    let router = emulator();
    let mut arns = BTreeSet::new();
    for index in 0..7 {
        arns.insert(
            create_pool(
                &router,
                json!({
                    "PoolName": format!("paged-{index}"),
                    "StorageClass": "GLACIER",
                }),
            )
            .await,
        );
    }

    // One page, several pages, exactly as many as there are, and more than there are.
    for limit in [1, 2, 3, 7, 50] {
        let walked = all_listed_pools(&router, Some(limit)).await;
        let unique: BTreeSet<String> = walked.iter().cloned().collect();
        assert_eq!(
            walked.len(),
            unique.len(),
            "case Limit={limit}: pagination must report each pool once, got {walked:?}"
        );
        assert_eq!(
            unique, arns,
            "case Limit={limit}: pagination must reach every pool, got {unique:?}"
        );
    }

    // A page carries at most `Limit` pools, and a `Marker` exactly while pools remain.
    let (answer, infos) = list_pools(&router, json!({ "Limit": 3 })).await;
    assert_eq!(
        infos.len(),
        3,
        "case Limit=3: the first page must carry three pools, got {infos:?}"
    );
    assert!(
        answer.body["Marker"].as_str().is_some(),
        "case Limit=3: a truncated answer must carry a Marker, got {:?}",
        answer.body
    );
}

/// A delete removes the pool it names, and only that one; a `PoolARN` naming no stored
/// pool is refused.
#[tokio::test]
async fn delete_tape_pool_removes_only_the_named_pool() {
    let router = emulator();
    let doomed = create_pool(
        &router,
        json!({
            "PoolName": "to-be-deleted",
            "StorageClass": "DEEP_ARCHIVE",
            "RetentionLockType": "GOVERNANCE",
            "RetentionLockTimeInDays": 30,
        }),
    )
    .await;
    let kept = create_pool(
        &router,
        json!({ "PoolName": "to-be-kept", "StorageClass": "GLACIER" }),
    )
    .await;

    let answer = call(&router, "DeleteTapePool", json!({ "PoolARN": doomed })).await;
    assert_eq!(
        answer.status, 200,
        "case {doomed}: expected HTTP 200 from DeleteTapePool, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.string("PoolARN"),
        doomed,
        "case {doomed}: DeleteTapePool must answer the ARN it deleted, got {:?}",
        answer.body
    );

    let listed = all_listed_pools(&router, None).await;
    assert_eq!(
        listed,
        vec![kept.clone()],
        "case {doomed}: after the delete only the other pool is reported, got {listed:?}"
    );

    // The deleted pool, a pool ARN nothing ever minted: both name no stored pool, so both
    // are refused with this service's one modelled client error rather than answered.
    for arn in [doomed.as_str(), NEVER_CREATED_POOL] {
        let answer = call(&router, "DeleteTapePool", json!({ "PoolARN": arn })).await;
        assert_eq!(
            answer.status, 400,
            "case {arn}: expected HTTP 400 from DeleteTapePool for a pool that is not \
             stored, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            INVALID_GATEWAY_REQUEST,
            "case {arn}: expected {INVALID_GATEWAY_REQUEST}, got {:?}",
            answer.body
        );
    }

    // The kept pool is still readable, so the refusals changed nothing.
    let info = describe_pool(&router, &kept).await;
    assert_eq!(
        info["PoolName"].as_str(),
        Some("to-be-kept"),
        "case {kept}: the surviving pool must still report its stored members, got {info:?}"
    );
}

/// The store is bounded: the create past the ceiling is refused, and the pools already
/// stored are untouched.
#[tokio::test]
async fn create_tape_pool_refuses_past_the_store_ceiling() {
    let router = emulator();
    for index in 0..MAX_TAPE_POOLS {
        create_pool(
            &router,
            json!({
                "PoolName": format!("bounded-{index}"),
                "StorageClass": "GLACIER",
            }),
        )
        .await;
    }

    let answer = call(
        &router,
        "CreateTapePool",
        json!({ "PoolName": "one-too-many", "StorageClass": "GLACIER" }),
    )
    .await;
    assert_eq!(
        answer.status, 400,
        "case at the ceiling: expected HTTP 400 from CreateTapePool, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        INVALID_GATEWAY_REQUEST,
        "case at the ceiling: expected {INVALID_GATEWAY_REQUEST}, got {:?}",
        answer.body
    );

    let listed = all_listed_pools(&router, None).await;
    assert_eq!(
        listed.len(),
        MAX_TAPE_POOLS,
        "case at the ceiling: the refused create must store nothing, got {} pools",
        listed.len()
    );
}

/// The `error.errorCode` a refusal carries when a `PoolARN` names no custom tape pool.
///
/// A missing resource is told apart from a malformed request by this code — both arrive as
/// the one wire error [`INVALID_GATEWAY_REQUEST`] with HTTP 400 — so it is the resource
/// kind's own `<Resource>NotFound`, the spelling this service's `ErrorCode` enum uses for
/// every kind it lists (`GatewayNotFound`, `VolumeNotFound`, `SnapshotNotFound`,
/// `TapeCartridgeNotFound`, …), and never the generic `InvalidParameters`.
const POOL_NOT_FOUND: &str = "PoolNotFound";

/// The whole `message` of that refusal: fixed text, with no part taken from the request.
/// Spelled out here rather than imported, so wording the implementation changes fails.
const POOL_NOT_FOUND_MESSAGE: &str = "The specified pool was not found.";

/// A `PoolARN` of exactly `len` characters, of the shape Storage Gateway mints, naming a
/// pool nothing ever created. `PoolARN` is modelled `@length(min: 50, max: 500)`, so the
/// published maximum gets its own row below.
fn unknown_pool_arn_of_length(len: usize) -> String {
    let prefix = format!("arn:aws:storagegateway:{REGION}:{ACCOUNT_ID}:{POOL_RESOURCE}/pool-");
    assert!(
        len > prefix.len(),
        "a {len}-character PoolARN cannot carry the {} -character ARN prefix",
        prefix.len()
    );
    format!("{prefix}{}", "A".repeat(len - prefix.len()))
}

/// A `PoolARN` that names no stored pool is refused as a MISSING POOL: `error.errorCode`
/// is the pool kind's not-found code and `message` is that error class's fixed sentence,
/// whatever ARN the request carried.
#[tokio::test]
async fn delete_tape_pool_refuses_a_missing_pool_as_not_found() {
    let router = emulator();

    // A pool that existed and was deleted — the cleanup path a client retries — and a
    // pool that never existed.
    let deleted = create_pool(
        &router,
        json!({ "PoolName": "deleted-once", "StorageClass": "GLACIER" }),
    )
    .await;
    let answer = call(&router, "DeleteTapePool", json!({ "PoolARN": &deleted })).await;
    assert_eq!(
        answer.status, 200,
        "case {deleted}: the first delete must succeed, got {} with {:?}",
        answer.status, answer.body
    );

    // One pool is left stored, so the rows below are refused for naming no pool rather
    // than because the store is empty.
    let kept = create_pool(
        &router,
        json!({ "PoolName": "kept-while-refusing", "StorageClass": "DEEP_ARCHIVE" }),
    )
    .await;

    // The rows differ along everything the rule generalises over: a pool deleted a moment
    // ago, an id nothing minted, the modelled maximum ARN length, the shortest ARN of this
    // account and region, another region, another account, and an ARN naming a different
    // resource kind entirely.
    let cases: Vec<(&str, String)> = vec![
        ("a pool deleted a moment ago", deleted.clone()),
        ("a pool id nothing ever minted", NEVER_CREATED_POOL.to_string()),
        (
            "a 500-character PoolARN, the modelled maximum",
            unknown_pool_arn_of_length(500),
        ),
        (
            "the shortest PoolARN of this account and region",
            unknown_pool_arn_of_length(61),
        ),
        (
            "a pool of another region",
            format!("arn:aws:storagegateway:us-east-1:{ACCOUNT_ID}:{POOL_RESOURCE}/pool-0000FFFF"),
        ),
        (
            "a pool of another account",
            format!("arn:aws:storagegateway:{REGION}:111122223333:{POOL_RESOURCE}/pool-0000FFFF"),
        ),
        (
            "an ARN naming a gateway rather than a pool",
            format!("arn:aws:storagegateway:{REGION}:{ACCOUNT_ID}:gateway/sgw-0000FFFF"),
        ),
    ];

    for (case, pool_arn) in &cases {
        let answer = call(&router, "DeleteTapePool", json!({ "PoolARN": pool_arn })).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400 from DeleteTapePool, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            INVALID_GATEWAY_REQUEST,
            "case {case}: expected {INVALID_GATEWAY_REQUEST}, got {:?}",
            answer.body
        );
        assert_not_found(
            &answer,
            case,
            POOL_NOT_FOUND,
            POOL_NOT_FOUND_MESSAGE,
            pool_arn,
        );
    }

    // The refusals changed nothing: the other pool is still reported.
    let listed = all_listed_pools(&router, None).await;
    assert_eq!(
        listed,
        vec![kept],
        "a refused delete must leave the stored pools alone, got {listed:?}"
    );
}
