// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateAccountPool`.
//!
//! Every case drives the service the way a client does — a real `restJson1` request
//! through the router, with only the response body as evidence — and reuses the
//! `Emulator` harness from [`crate::harness`], so nothing here reaches into
//! `crate::state` and a handler answering from a hardcoded value would fail.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one request:
//!
//! 1. The update is a PATCH of the stored pool: a member the request carries replaces
//!    the stored value, a member it omits leaves the stored value untouched, and the
//!    answer is the pool as it now stands — which `GetAccountPool` then reports
//!    identically, because the update was persisted.
//! 2. The members that belong to the create (`id`, `domainId`, `domainUnitId`,
//!    `createdBy`, `createdAt`) survive every update, and `updatedBy` — absent until
//!    something updates the pool — becomes the caller's own principal.
//! 3. The documented prerequisites are enforced against the store: a caller holds no
//!    permission on a domain this store does not serve, a pool nothing created is not
//!    found, and a pool is addressable only through its own domain.
//! 4. A patch outside a modelled constraint is refused with the modelled validation
//!    error and nothing is written, so the pool still reads back as it was.
//! 5. An update touches ONE pool: every other pool of the domain reads back unchanged.
//! 6. A patch applies to the pool AS IT NOW STANDS, so successive updates accumulate —
//!    an earlier step's members survive a later one that omits them — and every reader of
//!    the pool (`GetAccountPool` and the `ListAccountPools` summary) reports the record as
//!    the last update left it.

use crate::harness::{accounts, Emulator};
use serde_json::{json, Map, Value};
// The one `ResolutionStrategy` member as the service stores it, so a drift between the
// constant and the value on the wire fails here.
use service_com_amazonaws_datazone_datazone::state::RESOLUTION_STRATEGY_MANUAL;

/// The principal the harness signs with, as the handlers derive it from the request's
/// SigV4 credential scope — never from a request body.
const CALLER: &str = "arn:aws:iam::000000000000:user/AKIAEXAMPLETESTKEY";

/// The members an account pool's answer carries from the request, so a patch's effect is
/// judged on all of them at once rather than only the one it names.
const PATCHABLE: [&str; 4] = ["name", "description", "resolutionStrategy", "accountSource"];

/// A static-account-list pool, the shortest `AccountPoolName` the model admits and no
/// description.
fn static_pool() -> Value {
    json!({
        // `AccountPoolName` is 1..64 of `^[\w -]+$`; this is the minimum length.
        "name": "p",
        "resolutionStrategy": RESOLUTION_STRATEGY_MANUAL,
        "accountSource": { "accounts": accounts(1) },
    })
}

/// A custom-handler pool with a description, so the other `AccountSource` member and a
/// present description are patched too.
fn handler_pool() -> Value {
    json!({
        "name": "handler pool-1",
        "description": "resolved by a Lambda handler",
        "resolutionStrategy": RESOLUTION_STRATEGY_MANUAL,
        "accountSource": {
            "customAccountPoolHandler": {
                "lambdaFunctionArn":
                    "arn:aws:lambda:us-west-2:111122223333:function:resolve-accounts",
                "lambdaExecutionRoleArn":
                    "arn:aws:iam::111122223333:role/service-role/AccountPoolHandler",
            }
        },
    })
}

/// One `UpdateAccountPool` request body and the pool it is applied to.
struct PatchCase {
    label: &'static str,
    create: Value,
    patch: Value,
}

/// The rows differ along what the rule generalises over: WHICH members the patch carries
/// (none, one, several, all of them), the KIND of each value (a name at both modelled
/// bounds, a description added, replaced and at its maximum length, both members of the
/// `AccountSource` union and a static list of 1, 3 and the modelled maximum of 25
/// accounts), and the pool the patch starts from.
///
/// SOURCE for every bound and class named below:
/// `smithy_query(query="operation", operation="UpdateAccountPool")` and
/// `smithy_query(query="shape", shape="AccountSource,AccountInfo,AwsRegionList")`.
fn patch_cases() -> Vec<PatchCase> {
    vec![
        PatchCase {
            label: "a patch that carries nothing leaves every stored member alone",
            create: static_pool(),
            patch: json!({}),
        },
        PatchCase {
            label: "the name only, on a pool with no description",
            create: static_pool(),
            patch: json!({ "name": "renamed pool" }),
        },
        PatchCase {
            label: "a 64-character name, the modelled maximum",
            create: handler_pool(),
            patch: json!({ "name": "n".repeat(64) }),
        },
        PatchCase {
            label: "a description added to a pool that had none",
            create: static_pool(),
            patch: json!({ "description": "the accounts finance deploys into" }),
        },
        PatchCase {
            label: "a 2048-character description, the modelled maximum, over an existing one",
            create: handler_pool(),
            // `Description` is ..2048; this is the maximum.
            patch: json!({ "description": "d".repeat(2048) }),
        },
        PatchCase {
            label: "the resolution strategy only, the enum's one member",
            create: static_pool(),
            patch: json!({ "resolutionStrategy": RESOLUTION_STRATEGY_MANUAL }),
        },
        PatchCase {
            label: "a static account list grown to three accounts with names and regions",
            create: static_pool(),
            patch: json!({
                "accountSource": {
                    "accounts": [
                        {
                            "awsAccountId": "222233334444",
                            "supportedRegions": ["us-west-2"],
                            "awsAccountName": "finance-prod",
                        },
                        {
                            "awsAccountId": "333344445555",
                            "supportedRegions": ["eu-central-1", "ap-southeast-2"],
                        },
                        {
                            "awsAccountId": "444455556666",
                            "supportedRegions": ["us-east-1", "us-east-2", "eu-west-1"],
                            "awsAccountName": "finance-sandbox",
                        },
                    ]
                }
            }),
        },
        PatchCase {
            label: "a static account list grown to the modelled maximum of 25 accounts",
            create: static_pool(),
            patch: json!({ "accountSource": { "accounts": accounts(25) } }),
        },
        PatchCase {
            label: "a static list replaced by a custom handler, the other union member",
            create: static_pool(),
            patch: json!({
                "accountSource": {
                    "customAccountPoolHandler": {
                        "lambdaFunctionArn":
                            "arn:aws:lambda:eu-west-1:999988887777:function:pick_accounts:PROD",
                    }
                }
            }),
        },
        PatchCase {
            label: "a custom handler replaced by a static list, the other union member",
            create: handler_pool(),
            patch: json!({ "accountSource": { "accounts": accounts(2) } }),
        },
        PatchCase {
            label: "every patchable member at once",
            create: handler_pool(),
            patch: json!({
                "name": "fully_patched pool",
                "description": "everything replaced in one call",
                "resolutionStrategy": RESOLUTION_STRATEGY_MANUAL,
                "accountSource": { "accounts": accounts(4) },
            }),
        },
    ]
}

/// The pool `create` describes, stored in `domain`: its id and the create's own answer.
async fn create_pool(dz: &Emulator, domain: &str, label: &str, create: Value) -> (String, Value) {
    let created = dz
        .call(
            "POST",
            &format!("/v2/domains/{domain}/account-pools"),
            Some(create),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case {label:?}: creating the pool: expected 201, got {} {:?}",
        created.status, created.body
    );
    (created.s("id"), created.body)
}

/// Read a pool back through `GetAccountPool`, which must answer it.
async fn read_pool(dz: &Emulator, domain: &str, pool: &str, label: &str) -> Value {
    let got = dz
        .call(
            "GET",
            &format!("/v2/domains/{domain}/account-pools/{pool}"),
            None,
        )
        .await;
    assert_eq!(
        got.status, 200,
        "case {label:?}: reading the pool back: expected 200, got {} {:?}",
        got.status, got.body
    );
    got.body
}

/// `stored` with every member of `patch` replaced — the state the PATCH leaves behind.
fn merged(stored: &Value, patch: &Value) -> Map<String, Value> {
    let mut expected: Map<String, Value> = Map::new();
    for member in PATCHABLE {
        if let Some(value) = patch.get(member).or_else(|| stored.get(member)) {
            expected.insert(member.to_string(), value.clone());
        }
    }
    expected
}

/// Assert `answer` reports exactly the members `expected` names, and none of the others.
fn assert_members(label: &str, source: &str, answer: &Value, expected: &Map<String, Value>) {
    for member in PATCHABLE {
        assert_eq!(
            answer.get(member),
            expected.get(member),
            "case {label:?}: {source} must report {member} as {:?}, got {:?}",
            expected.get(member),
            answer.get(member)
        );
    }
}

#[tokio::test]
async fn an_update_patches_the_stored_pool_and_is_reported_by_get() {
    let dz = Emulator::new();
    let domain = dz.available_domain("update-account-pool").await;

    for case in patch_cases() {
        let (id, created) = create_pool(&dz, &domain, case.label, case.create.clone()).await;
        // Nothing has updated the pool yet, so no principal is reported as its updater and
        // its last update IS its creation.
        assert!(
            created.get("updatedBy").is_none(),
            "case {:?}: a pool nothing updated must report no updatedBy, got {:?}",
            case.label,
            created
        );
        assert_eq!(
            created.get("lastUpdatedAt"),
            created.get("createdAt"),
            "case {:?}: a pool nothing updated must report its creation as its last update, got \
             {:?}",
            case.label,
            created.get("lastUpdatedAt")
        );
        let expected = merged(&created, &case.patch);

        let updated = dz
            .call(
                "PATCH",
                &format!("/v2/domains/{domain}/account-pools/{id}"),
                Some(case.patch.clone()),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected 200 from UpdateAccountPool, got {} {:?}",
            case.label, updated.status, updated.body
        );
        assert_members(case.label, "UpdateAccountPool", &updated.body, &expected);

        // The create's own members survive the update untouched.
        for member in ["id", "domainId", "domainUnitId", "createdBy", "createdAt"] {
            assert_eq!(
                updated.body.get(member),
                created.get(member),
                "case {:?}: UpdateAccountPool must report the stored {member} {:?}, got {:?}",
                case.label,
                created.get(member),
                updated.body.get(member)
            );
        }
        // The update is what sets `updatedBy`, from the caller's credential scope.
        assert_eq!(
            updated.s("updatedBy"),
            CALLER,
            "case {:?}: expected the signing principal as updatedBy, got {:?}",
            case.label,
            updated.body
        );
        assert!(
            updated.body.get("lastUpdatedAt").is_some(),
            "case {:?}: expected lastUpdatedAt, got {:?}",
            case.label,
            updated.body
        );

        // The update was PERSISTED: the pool's own reader answers the same record.
        let read_back = read_pool(&dz, &domain, &id, case.label).await;
        assert_members(case.label, "GetAccountPool", &read_back, &expected);
        for member in [
            "id",
            "domainId",
            "domainUnitId",
            "createdBy",
            "createdAt",
            "lastUpdatedAt",
            "updatedBy",
        ] {
            assert_eq!(
                read_back.get(member),
                updated.body.get(member),
                "case {:?}: GetAccountPool must report {member} as the update did ({:?}), got {:?}",
                case.label,
                updated.body.get(member),
                read_back.get(member)
            );
        }
    }
}

#[tokio::test]
async fn an_update_touches_only_the_pool_it_names() {
    let dz = Emulator::new();
    let domain = dz.available_domain("update-account-pool-isolation").await;

    // Three pools of the same domain, so a patch of one is checked against the others.
    let mut pools: Vec<(String, Value)> = Vec::new();
    for (index, create) in [static_pool(), handler_pool(), static_pool()]
        .into_iter()
        .enumerate()
    {
        let label = "isolation";
        let mut body = create;
        body["name"] = json!(format!("isolated pool {index}"));
        let (id, created) = create_pool(&dz, &domain, label, body).await;
        pools.push((id, created));
    }

    let (target, target_created) = pools[1].clone();
    let patch = json!({
        "name": "the only pool that changed",
        "accountSource": { "accounts": accounts(3) },
    });
    let updated = dz
        .call(
            "PATCH",
            &format!("/v2/domains/{domain}/account-pools/{target}"),
            Some(patch.clone()),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "updating the target pool: expected 200, got {} {:?}",
        updated.status, updated.body
    );
    assert_members(
        "isolation",
        "UpdateAccountPool",
        &updated.body,
        &merged(&target_created, &patch),
    );

    for (id, created) in &pools {
        if *id == target {
            continue;
        }
        let read_back = read_pool(&dz, &domain, id, "isolation").await;
        assert_members(
            "isolation",
            "GetAccountPool on an untouched pool",
            &read_back,
            &merged(created, &json!({})),
        );
        assert!(
            read_back.get("updatedBy").is_none(),
            "pool {id:?} was not updated, so it must report no updatedBy, got {read_back:?}"
        );
        assert_eq!(
            read_back.get("lastUpdatedAt"),
            read_back.get("createdAt"),
            "pool {id:?} was not updated, so its last update must still be its creation, got \
             {:?}",
            read_back.get("lastUpdatedAt")
        );
    }
}

#[tokio::test]
async fn an_update_needs_a_domain_and_a_pool_the_store_holds() {
    let dz = Emulator::new();
    let domain = dz.available_domain("update-account-pool-prerequisites").await;
    let elsewhere = dz
        .available_domain("update-account-pool-other-domain")
        .await;
    let (pool, _) = create_pool(&dz, &domain, "prerequisites", static_pool()).await;

    // A well-formed domain id nothing created: a permission is held ON a domain, so the
    // caller holds none here and authorization decides the update before any lookup.
    for case in patch_cases() {
        let denied = dz
            .call(
                "PATCH",
                &format!("/v2/domains/dzd_nosuchdomain000/account-pools/{pool}"),
                Some(case.patch),
            )
            .await;
        assert_eq!(
            denied.status, 403,
            "case {:?}: expected 403 for a domain nothing created, got {} {:?}",
            case.label, denied.status, denied.body
        );
        assert_eq!(
            denied.error_code, "AccessDeniedException",
            "case {:?}: expected AccessDeniedException, got {:?}",
            case.label, denied.error_code
        );
        assert_eq!(
            denied.s("message"),
            "User is not permitted to perform operation: UpdateAccountPool",
            "case {:?}: the denial must carry the recorded wording, got {:?}",
            case.label,
            denied.body
        );
    }

    // A pool id nothing created, in a domain that does exist: the identifiers span the
    // modelled `AccountPoolId` pattern `^[a-zA-Z0-9_-]{1,36}$`.
    for id in ["nosuchpool", "0", &"z".repeat(36)] {
        let missing = dz
            .call(
                "PATCH",
                &format!("/v2/domains/{domain}/account-pools/{id}"),
                Some(json!({ "name": "no such pool" })),
            )
            .await;
        assert_eq!(
            missing.status, 404,
            "updating pool {id:?}: expected 404, got {} {:?}",
            missing.status, missing.body
        );
        assert_eq!(
            missing.error_code, "ResourceNotFoundException",
            "updating pool {id:?}: expected ResourceNotFoundException, got {:?}",
            missing.error_code
        );
    }

    // A pool is addressable only through its OWN domain: the same id patched through
    // another domain is as absent as one that was never created, and the pool itself is
    // left alone.
    let cross = dz
        .call(
            "PATCH",
            &format!("/v2/domains/{elsewhere}/account-pools/{pool}"),
            Some(json!({ "name": "patched from elsewhere" })),
        )
        .await;
    assert_eq!(
        cross.status, 404,
        "a pool of another domain must not be patchable here, got {} {:?}",
        cross.status, cross.body
    );
    let untouched = read_pool(&dz, &domain, &pool, "cross-domain patch").await;
    assert_members(
        "cross-domain patch",
        "GetAccountPool",
        &untouched,
        &merged(&static_pool(), &json!({})),
    );
    assert!(
        untouched.get("updatedBy").is_none(),
        "a refused patch must not mark the pool updated, got {untouched:?}"
    );
}

#[tokio::test]
async fn a_patch_outside_a_modelled_constraint_is_refused_and_stores_nothing() {
    let dz = Emulator::new();
    let domain = dz.available_domain("update-account-pool-constraints").await;
    let (pool, created) = create_pool(&dz, &domain, "constraints", handler_pool()).await;
    let stored = merged(&created, &json!({}));

    // Each row is one modelled constraint, named with the shape it comes from, taken one
    // step PAST its published bound or outside its class.
    let refused: Vec<(&str, Value)> = vec![
        // `AccountInfoList` is 1..25.
        ("an empty account list", json!({ "accountSource": { "accounts": [] } })),
        (
            "26 accounts, one past the modelled maximum",
            json!({ "accountSource": { "accounts": accounts(26) } }),
        ),
        (
            // `AwsAccountId` is `^\d{12}$`.
            "an account id of eleven digits",
            json!({
                "accountSource": {
                    "accounts": [
                        { "awsAccountId": "11112222333", "supportedRegions": ["us-east-1"] }
                    ]
                }
            }),
        ),
        (
            // `AwsRegionList` is 1..3.
            "four supported regions, one past the modelled maximum",
            json!({
                "accountSource": {
                    "accounts": [{
                        "awsAccountId": "111122223333",
                        "supportedRegions": ["us-east-1", "us-east-2", "us-west-1", "us-west-2"],
                    }]
                }
            }),
        ),
        (
            // `AwsRegion` is `^[a-z]{2}-[a-z]{4,10}-\d$`.
            "a region outside the modelled class",
            json!({
                "accountSource": {
                    "accounts": [
                        { "awsAccountId": "111122223333", "supportedRegions": ["US-EAST-1"] }
                    ]
                }
            }),
        ),
        (
            // `AccountPoolName` is 1..64 of `^[\w -]+$`.
            "a 65-character name, one past the modelled maximum",
            json!({ "name": "n".repeat(65) }),
        ),
        ("an empty name, one short of the modelled minimum", json!({ "name": "" })),
        ("a name outside the modelled character class", json!({ "name": "pool!" })),
        (
            // `Description` is ..2048.
            "a 2049-character description, one past the modelled maximum",
            json!({ "description": "d".repeat(2049) }),
        ),
        (
            // `ResolutionStrategy`'s only member is MANUAL.
            "a resolution strategy the enum does not have",
            json!({ "resolutionStrategy": "AUTOMATIC" }),
        ),
    ];

    for (label, body) in refused {
        let answer = dz
            .call(
                "PATCH",
                &format!("/v2/domains/{domain}/account-pools/{pool}"),
                Some(body),
            )
            .await;
        assert_eq!(
            answer.status, 400,
            "case {label:?}: expected 400, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "ValidationException",
            "case {label:?}: expected ValidationException, got {:?}",
            answer.error_code
        );
        // A refused patch writes NOTHING: the pool still reads back as it was created.
        let read_back = read_pool(&dz, &domain, &pool, label).await;
        assert_members(label, "GetAccountPool after a refusal", &read_back, &stored);
        assert!(
            read_back.get("updatedBy").is_none(),
            "case {label:?}: a refused patch must not mark the pool updated, got {read_back:?}"
        );
    }
}

/// The members `AccountPoolSummary` declares and a pool's own record also carries, so the
/// listing is checked as a READER of the same stored pool rather than against literals
/// (SOURCE: `smithy_query(query="shape", shape="AccountPoolSummary")`).
const SUMMARY_MEMBERS: [&str; 7] = [
    "domainId",
    "id",
    "name",
    "resolutionStrategy",
    "domainUnitId",
    "createdBy",
    "updatedBy",
];

/// The summary `ListAccountPools` reports for `pool`, which has to be among them.
async fn listed_summary(dz: &Emulator, domain: &str, pool: &str, label: &str) -> Value {
    let listed = dz
        .call("GET", &format!("/v2/domains/{domain}/account-pools"), None)
        .await;
    assert_eq!(
        listed.status, 200,
        "case {label:?}: expected 200 from ListAccountPools, got {} {:?}",
        listed.status, listed.body
    );
    listed
        .body
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(pool))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {label:?}: ListAccountPools must report pool {pool:?}, got {:?}",
                listed.body
            )
        })
}

/// A timestamp member as epoch seconds, so two of them can be ordered.
fn epoch_seconds(answer: &Value, member: &str, label: &str) -> f64 {
    answer
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {label:?}: expected a {member} timestamp, got {answer:?}"))
}

#[tokio::test]
async fn successive_updates_accumulate_and_every_reader_reports_the_patched_pool() {
    let dz = Emulator::new();
    let domain = dz.available_domain("update-account-pool-successive").await;
    let (pool, created) = create_pool(&dz, &domain, "successive", static_pool()).await;

    // One pool patched step after step: each step is applied to the pool as the step
    // BEFORE it left it, so the expectation carries every earlier member forward. The
    // steps differ along what the rule generalises over — which members a step carries
    // (one, several, none), the kind of each value, both members of the `AccountSource`
    // union, and a step that repeats the one before it unchanged.
    let repeated = json!({
        "accountSource": {
            "customAccountPoolHandler": {
                "lambdaFunctionArn":
                    "arn:aws:lambda:us-west-2:555566667777:function:resolve_pool",
                "lambdaExecutionRoleArn":
                    "arn:aws:iam::555566667777:role/service-role/PoolResolver",
            }
        }
    });
    let steps: Vec<(&str, Value)> = vec![
        ("the name only, over the created record", json!({ "name": "first rename" })),
        (
            "a description, leaving the name the step before set alone",
            json!({ "description": "the accounts this pool now resolves" }),
        ),
        ("a custom handler replacing the created static list", repeated.clone()),
        ("the same patch again, unchanged", repeated),
        ("a patch that carries nothing, over an updated record", json!({})),
        (
            "every patchable member at once",
            json!({
                "name": "fully patched",
                "description": "replaced in one call",
                "resolutionStrategy": RESOLUTION_STRATEGY_MANUAL,
                "accountSource": { "accounts": accounts(3) },
            }),
        ),
        (
            "the name again, leaving the whole accumulated record alone",
            json!({ "name": "last rename" }),
        ),
    ];

    let mut expected = Value::Object(merged(&created, &json!({})));
    for (label, patch) in steps {
        expected = Value::Object(merged(&expected, &patch));

        let updated = dz
            .call(
                "PATCH",
                &format!("/v2/domains/{domain}/account-pools/{pool}"),
                Some(patch.clone()),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: expected 200 from UpdateAccountPool, got {} {:?}",
            updated.status, updated.body
        );
        let expected_members = expected.as_object().expect("an object").clone();
        assert_members(label, "UpdateAccountPool", &updated.body, &expected_members);

        // The accumulated record is what the pool's own reader answers, member for
        // member, with the create's members still as they were stored.
        let read_back = read_pool(&dz, &domain, &pool, label).await;
        assert_members(
            label,
            "GetAccountPool after the update",
            &read_back,
            &expected_members,
        );
        for member in ["id", "domainId", "domainUnitId", "createdBy", "createdAt"] {
            assert_eq!(
                read_back.get(member),
                created.get(member),
                "case {label:?}: {member} belongs to the create, so it must still be {:?}, got {:?}",
                created.get(member),
                read_back.get(member)
            );
        }
        assert_eq!(
            read_back.get("updatedBy").and_then(Value::as_str),
            Some(CALLER),
            "case {label:?}: expected the signing principal as updatedBy, got {:?}",
            read_back
        );
        assert!(
            epoch_seconds(&read_back, "lastUpdatedAt", label)
                >= epoch_seconds(&read_back, "createdAt", label),
            "case {label:?}: an updated pool's last update must not precede its creation, got \
             {read_back:?}"
        );

        // The listing is a reader of the SAME record, so it reports the pool as the last
        // update left it rather than as it was created.
        let summary = listed_summary(&dz, &domain, &pool, label).await;
        for member in SUMMARY_MEMBERS {
            assert_eq!(
                summary.get(member),
                read_back.get(member),
                "case {label:?}: the summary's {member} must be the stored {:?}, got {:?}",
                read_back.get(member),
                summary.get(member)
            );
        }
    }
}
