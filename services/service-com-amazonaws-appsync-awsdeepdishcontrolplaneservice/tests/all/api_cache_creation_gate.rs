// SPDX-License-Identifier: CC0-1.0

//! The precondition the two cache MUTATORS share, end to end over HTTP.
//!
//! One rule: a call that reconfigures (`UpdateApiCache`) or tears down
//! (`DeleteApiCache`) an API's cache addresses the instance the API HOLDS, so while that
//! instance's creation has not completed — the `ApiCacheStatus` the model defines as the
//! creation itself, `CREATING`, and the one it defines as a creation that failed,
//! `FAILED` — there is no provisioned instance for either call to act on and both are
//! refused `BadRequestException` (400), each with its own fixed sentence, writing
//! nothing. Once the create has come due the same two calls are served.
//!
//! The rows differ along what the rule generalises over — the settings the cache was
//! created with (`ttl`, caching behaviour, instance type, health metrics) and which of
//! the two mutators the row drives first — because the rule reads none of them: it reads
//! only whether the instance has finished being created.
//!
//! The harness (`call`, `create_api`, `router`, `Answer`) is the error-contract suite's.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, router, Answer};

/// The fixed sentence `UpdateApiCache` is refused with while the instance it addresses
/// has not finished being created. Entirely fixed text: it carries nothing of the
/// request and nothing of the stored cache.
const NOT_YET_AVAILABLE: &str = "The cache is not yet available. Please wait until it becomes available.";

/// The fixed sentence `DeleteApiCache` is refused with for the same instance, worded for
/// a teardown and likewise entirely fixed.
const NOT_DELETABLE: &str = "Cannot delete cache as it is either not available or its creation failed.";

/// The `ApiCacheStatus` a cache being provisioned carries.
/// Source: `smithy_query(query="shape", shape="ApiCacheStatus")`.
const CREATING: &str = "CREATING";

/// The `ApiCacheStatus` a provisioned cache carries.
/// Source: `smithy_query(query="shape", shape="ApiCacheStatus")`.
const AVAILABLE: &str = "AVAILABLE";

/// How long a poll for a transition the clock owes keeps looking before it gives up.
/// Generous: the suite waits for the CONDITION, so a loaded host only makes it look
/// again rather than fail.
const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a poll waits between two looks.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// The settings one row creates its cache with, and the ones its accepted update
/// carries.
#[derive(Debug, Clone, Copy)]
struct Settings {
    ttl: i64,
    behavior: &'static str,
    cache_type: &'static str,
    health: &'static str,
}

/// A `CreateApiCache` body with the members the create requires, plus the optional
/// health-metrics setting.
/// Source: `smithy_query(query="operation", operation="CreateApiCache")`.
fn create_cache_body(settings: &Settings) -> Value {
    json!({
        "ttl": settings.ttl,
        "apiCachingBehavior": settings.behavior,
        "type": settings.cache_type,
        "transitEncryptionEnabled": true,
        "atRestEncryptionEnabled": false,
        "healthMetricsConfig": settings.health,
    })
}

/// An `UpdateApiCache` body with the three members that update requires.
/// Source: `smithy_query(query="operation", operation="UpdateApiCache")`.
fn update_cache_body(settings: &Settings) -> Value {
    json!({
        "ttl": settings.ttl,
        "apiCachingBehavior": settings.behavior,
        "type": settings.cache_type,
    })
}

/// Create the API's cache.
async fn create_cache(router: &MultiServiceRouter, api_id: &str, settings: &Settings) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/ApiCaches"),
        Some(create_cache_body(settings)),
    )
    .await
}

/// Read the API's cache back.
async fn get_cache(router: &MultiServiceRouter, api_id: &str) -> Answer {
    call(router, "GET", &format!("/v1/apis/{api_id}/ApiCaches"), None).await
}

/// Reconfigure the API's cache.
async fn update_cache(router: &MultiServiceRouter, api_id: &str, settings: &Settings) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/ApiCaches/update"),
        Some(update_cache_body(settings)),
    )
    .await
}

/// Tear the API's cache down.
async fn delete_cache(router: &MultiServiceRouter, api_id: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/v1/apis/{api_id}/ApiCaches"),
        None,
    )
    .await
}

/// Poll `GetApiCache` until the instance reports `status`, and answer that reply — the
/// transition to it is one the clock owes, so the suite looks again until it holds
/// rather than waiting a fixed time for it.
async fn poll_until_status(
    router: &MultiServiceRouter,
    api_id: &str,
    status: &str,
    case: &str,
) -> Answer {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let read = get_cache(router, api_id).await;
        if read.body["apiCache"]["status"].as_str() == Some(status) {
            return read;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the instance to reach {status}, last answer {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert the cache the API reports still carries `settings` — what a refusal that wrote
/// nothing leaves behind.
fn assert_reports(read: &Answer, settings: &Settings, case: &str, when: &str) {
    assert_eq!(
        read.status, 200,
        "case {case:?} {when}: expected the stored cache to be readable, got {read:?}"
    );
    assert_eq!(
        read.body["apiCache"]["ttl"].as_i64(),
        Some(settings.ttl),
        "case {case:?} {when}: expected ttl {}, got {read:?}",
        settings.ttl
    );
    assert_eq!(
        read.body["apiCache"]["apiCachingBehavior"].as_str(),
        Some(settings.behavior),
        "case {case:?} {when}: expected caching behavior {}, got {read:?}",
        settings.behavior
    );
    assert_eq!(
        read.body["apiCache"]["type"].as_str(),
        Some(settings.cache_type),
        "case {case:?} {when}: expected type {}, got {read:?}",
        settings.cache_type
    );
}

/// Which mutator a row sends first at the instance that is still being created.
#[derive(Debug, Clone, Copy)]
enum First {
    Update,
    Delete,
}

/// The rule: while the create has not come due, both mutators are refused 400 with their
/// own fixed sentence and the stored cache is untouched; once it has, both are served.
#[tokio::test]
async fn neither_mutator_acts_on_a_cache_whose_creation_has_not_completed() {
    let router = router();

    // (case, the settings the cache is created with, the settings an accepted update
    // would carry, which mutator the row sends first). The rows span the create's value
    // sets — both published `ttl` bounds and values between them, three caching
    // behaviours, generic and legacy instance types, both health-metrics settings — none
    // of which the rule reads.
    let rows: &[(&str, Settings, Settings, First)] = &[
        (
            "the published minimum ttl, full-request caching, update first",
            Settings { ttl: 1, behavior: "FULL_REQUEST_CACHING", cache_type: "SMALL", health: "ENABLED" },
            Settings { ttl: 600, behavior: "PER_RESOLVER_CACHING", cache_type: "SMALL", health: "ENABLED" },
            First::Update,
        ),
        (
            "a mid-window ttl, per-resolver caching, delete first",
            Settings { ttl: 300, behavior: "PER_RESOLVER_CACHING", cache_type: "LARGE_2X", health: "DISABLED" },
            Settings { ttl: 42, behavior: "OPERATION_LEVEL_CACHING", cache_type: "LARGE_4X", health: "DISABLED" },
            First::Delete,
        ),
        (
            "the published maximum ttl, operation-level caching, update first",
            Settings { ttl: 3_600, behavior: "OPERATION_LEVEL_CACHING", cache_type: "T2_MEDIUM", health: "ENABLED" },
            Settings { ttl: 1, behavior: "FULL_REQUEST_CACHING", cache_type: "R4_LARGE", health: "ENABLED" },
            First::Update,
        ),
        (
            "a legacy instance type, delete first",
            Settings { ttl: 120, behavior: "FULL_REQUEST_CACHING", cache_type: "R4_2XLARGE", health: "DISABLED" },
            Settings { ttl: 3_600, behavior: "PER_RESOLVER_CACHING", cache_type: "LARGE_12X", health: "ENABLED" },
            First::Delete,
        ),
    ];

    for (index, (case, created_with, update_with, first)) in rows.iter().enumerate() {
        let api_id = create_api(&router, &format!("Api_cache_gate_{index}")).await;
        let created = create_cache(&router, &api_id, created_with).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the create to be accepted, got {created:?}"
        );
        assert_eq!(
            created.body["apiCache"]["status"].as_str(),
            Some(CREATING),
            "case {case:?}: expected a provisioning instance, got {created:?}"
        );

        // Both mutators, in the row's order, twice each: the refusal writes nothing, so
        // it is the same answer however often it is asked for and whichever call asked
        // first.
        let order = match first {
            First::Update => [First::Update, First::Delete],
            First::Delete => [First::Delete, First::Update],
        };
        for attempt in 1..=2 {
            for mutator in &order {
                let (operation, refused, sentence) = match mutator {
                    First::Update => (
                        "UpdateApiCache",
                        update_cache(&router, &api_id, update_with).await,
                        NOT_YET_AVAILABLE,
                    ),
                    First::Delete => (
                        "DeleteApiCache",
                        delete_cache(&router, &api_id).await,
                        NOT_DELETABLE,
                    ),
                };
                assert_eq!(
                    (
                        refused.status,
                        refused.error_type.as_str(),
                        refused.message.as_str()
                    ),
                    (400, "BadRequestException", sentence),
                    "case {case:?} {operation} attempt {attempt} while the create has not \
                     completed: expected 400 BadRequestException {sentence:?}, got {refused:?}"
                );

                // And it wrote nothing: the instance is still the one the create stored,
                // still being created.
                let read = get_cache(&router, &api_id).await;
                assert_eq!(
                    read.body["apiCache"]["status"].as_str(),
                    Some(CREATING),
                    "case {case:?} {operation} attempt {attempt}: expected the refusal to \
                     leave the instance being created, got {read:?}"
                );
                assert_reports(&read, created_with, case, "after the refusal");
            }
        }

        // Once the creation has come due the precondition holds, so both mutators are
        // served: the update applies its settings and the teardown removes the instance.
        poll_until_status(&router, &api_id, AVAILABLE, case).await;
        let updated = update_cache(&router, &api_id, update_with).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected the update to be served once the instance exists, \
             got {updated:?}"
        );
        assert_reports(
            &get_cache(&router, &api_id).await,
            update_with,
            case,
            "after the accepted update",
        );
        let deleted = delete_cache(&router, &api_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected the teardown to be served once the instance exists, \
             got {deleted:?}"
        );
    }
}

/// The gate is about the instance the ADDRESSED API holds: a refusal on one API's
/// still-provisioning cache leaves a sibling API's available cache reconfigurable, so the
/// rule is read per cache rather than per store.
#[tokio::test]
async fn the_gate_reads_the_addressed_apis_own_instance() {
    let router = router();
    let settled = Settings {
        ttl: 300,
        behavior: "FULL_REQUEST_CACHING",
        cache_type: "SMALL",
        health: "ENABLED",
    };
    let fresh = Settings {
        ttl: 60,
        behavior: "PER_RESOLVER_CACHING",
        cache_type: "MEDIUM",
        health: "DISABLED",
    };

    let settled_api = create_api(&router, "Api_cache_gate_settled").await;
    let created = create_cache(&router, &settled_api, &settled).await;
    assert_eq!(
        created.status, 200,
        "case seed the settled API's cache: expected 200, got {created:?}"
    );
    poll_until_status(&router, &settled_api, AVAILABLE, "the settled API").await;

    let fresh_api = create_api(&router, "Api_cache_gate_fresh").await;
    let seeded = create_cache(&router, &fresh_api, &fresh).await;
    assert_eq!(
        seeded.status, 200,
        "case seed the fresh API's cache: expected 200, got {seeded:?}"
    );

    let refused = update_cache(&router, &fresh_api, &settled).await;
    assert_eq!(
        (
            refused.status,
            refused.error_type.as_str(),
            refused.message.as_str()
        ),
        (400, "BadRequestException", NOT_YET_AVAILABLE),
        "case the fresh API: expected 400 BadRequestException {NOT_YET_AVAILABLE:?}, \
         got {refused:?}"
    );

    let served = update_cache(&router, &settled_api, &fresh).await;
    assert_eq!(
        served.status, 200,
        "case the settled API: expected the update to be served, got {served:?}"
    );
    assert_reports(
        &get_cache(&router, &settled_api).await,
        &fresh,
        "the settled API",
        "after the accepted update",
    );
    assert_reports(
        &get_cache(&router, &fresh_api).await,
        &fresh,
        "the fresh API",
        "after the refusal",
    );
}
