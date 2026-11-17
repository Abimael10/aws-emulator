// SPDX-License-Identifier: CC0-1.0

//! How an API's cache slot reports WHY it is empty, end to end over HTTP.
//!
//! One rule, asserted over tables that span the class rather than the single request that
//! exposed it: a teardown removes the instance the API HOLDS, over a span in which that
//! instance is reported in the modelled `DELETING` status, and from then on every call
//! that addresses the absent cache reports a cache that HAS BEEN DELETED and asks for a
//! re-create, for as long as the slot stays empty. A teardown that arrives while the
//! removal is still being carried out finds an instance of its own, so teardowns racing
//! on one cache are all served. A teardown that finds the slot already empty completes
//! the deletion an earlier one recorded — which returns the API to holding no cache at
//! all — and a teardown with neither an instance nor a deletion to complete asks for the
//! removal of a cache the API never had, which is refused 404 `NotFoundException` naming
//! the call.
//!
//! The sentences asserted here are written out in full rather than read from the service,
//! so the assertion is about AppSync's wording rather than about whatever the handler
//! happens to build.
//!
//! Nothing here waits for a span: every wait polls for the CONDITION it needs, under a
//! deadline generous enough that a loaded host only goes round the loop more often.
//!
//! The `call`/`create_api`/`Answer` harness is the error-contract suite's.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, Answer};

/// How long a poll below keeps looking for the condition it waits on. Generous on
/// purpose: the condition, not this bound, is what each case is about.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// `ApiCachingBehavior`'s whole value set, in the order the model declares it.
/// Source: `smithy_query(query="shape", shape="ApiCachingBehavior")`.
const CACHING_BEHAVIORS: &[&str] = &[
    "FULL_REQUEST_CACHING",
    "PER_RESOLVER_CACHING",
    "OPERATION_LEVEL_CACHING",
];

/// `ApiCacheType` values spanning the class the rule generalises over: the generic
/// instance identifiers and the discouraged legacy EC2-style ones alike.
/// Source: `smithy_query(query="shape", shape="ApiCacheType")`.
const CACHE_TYPES: &[&str] = &["SMALL", "LARGE", "LARGE_8X", "R4_2XLARGE"];

/// `CacheHealthMetricsConfig`'s whole value set.
/// Source: `smithy_query(query="shape", shape="CacheHealthMetricsConfig")`.
const HEALTH_METRICS: &[&str] = &["ENABLED", "DISABLED"];

/// The `ApiCacheStatus` an instance whose teardown has been accepted is reported in.
/// Source: `smithy_query(query="shape", shape="ApiCacheStatus")`.
const DELETING: &str = "DELETING";

/// The `ApiCacheStatus` a provisioned instance is reported in.
const AVAILABLE: &str = "AVAILABLE";

/// AWS's sentence for a call that found no cache because a teardown removed the one the
/// API had: the cache has been deleted, so the caller is asked to re-create it. Fixed
/// text apart from the verb the call itself supplies.
const DELETED_GET: &str =
    "Unable to get the cache as it has been deleted, please re-create the cache.";
const DELETED_UPDATE: &str =
    "Unable to update the cache as it has been deleted, please re-create the cache.";
const DELETED_FLUSH: &str =
    "Unable to flush the cache as it has been deleted, please re-create the cache.";

/// AWS's sentence for a call that found no cache on an API that holds none at all: there
/// is nothing there, so the caller is asked to create one. Fixed text apart from the verb
/// the call itself supplies — the teardown's included.
const ABSENT_GET: &str = "Unable to get the cache as it doesn't exist, please create the cache first.";
const ABSENT_UPDATE: &str =
    "Unable to update the cache as it doesn't exist, please create the cache first.";
const ABSENT_FLUSH: &str =
    "Unable to flush the cache as it doesn't exist, please create the cache first.";
const ABSENT_DELETE: &str =
    "Unable to delete the cache as it doesn't exist, please create the cache first.";

/// A router with only this service mounted, over a fresh engine.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// A `CreateApiCache` body with the members the create requires, plus the optional
/// health-metrics setting.
fn create_cache_body(ttl: i64, behavior: &str, cache_type: &str, health: &str) -> Value {
    json!({
        "ttl": ttl,
        "apiCachingBehavior": behavior,
        "type": cache_type,
        "transitEncryptionEnabled": true,
        "atRestEncryptionEnabled": true,
        "healthMetricsConfig": health,
    })
}

/// An `UpdateApiCache` body with the three members the update requires.
fn update_cache_body(ttl: i64, behavior: &str, cache_type: &str) -> Value {
    json!({
        "ttl": ttl,
        "apiCachingBehavior": behavior,
        "type": cache_type,
    })
}

/// Create the API's cache.
async fn create_cache(
    router: &MultiServiceRouter,
    api_id: &str,
    ttl: i64,
    behavior: &str,
    cache_type: &str,
    health: &str,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/ApiCaches"),
        Some(create_cache_body(ttl, behavior, cache_type, health)),
    )
    .await
}

/// Read the API's cache back.
async fn get_cache(router: &MultiServiceRouter, api_id: &str) -> Answer {
    call(router, "GET", &format!("/v1/apis/{api_id}/ApiCaches"), None).await
}

/// Apply new settings to the API's cache.
async fn update_cache(
    router: &MultiServiceRouter,
    api_id: &str,
    ttl: i64,
    behavior: &str,
    cache_type: &str,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/ApiCaches/update"),
        Some(update_cache_body(ttl, behavior, cache_type)),
    )
    .await
}

/// Discard the API's cache entries.
async fn flush_cache(router: &MultiServiceRouter, api_id: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/v1/apis/{api_id}/FlushCache"),
        None,
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

/// Whether the response body carries no members at all — the shape of the modelled empty
/// `DeleteApiCacheResponse`, which is either an object with no members or no body at all.
fn carries_no_members(body: &Value) -> bool {
    match body {
        Value::Null => true,
        Value::Object(members) => members.is_empty(),
        _ => false,
    }
}

/// Poll `GetApiCache` until the instance reports `status`, and answer that reply.
async fn poll_until_status(
    router: &MultiServiceRouter,
    api_id: &str,
    status: &str,
    case: &str,
) -> Answer {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let read = get_cache(router, api_id).await;
        if read.body["apiCache"]["status"].as_str() == Some(status) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the instance to reach {status}, last answer {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `GetApiCache` until it reports no instance at all, and answer that refusal.
async fn poll_until_no_instance(router: &MultiServiceRouter, api_id: &str, case: &str) -> Answer {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let read = get_cache(router, api_id).await;
        if read.status != 200 {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the instance to be removed, last answer {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create one API, give it a cache and wait for that instance to be available: the state
/// a teardown is served from.
async fn api_with_available_cache(
    router: &MultiServiceRouter,
    name: &str,
    ttl: i64,
    behavior: &str,
    cache_type: &str,
    health: &str,
    case: &str,
) -> String {
    let api_id = create_api(router, name).await;
    let created = create_cache(router, &api_id, ttl, behavior, cache_type, health).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected the seed create to be accepted, got {created:?}"
    );
    poll_until_status(router, &api_id, AVAILABLE, case).await;
    api_id
}

/// Fire `racers` teardowns at one API at the same time and answer what each was served.
/// The arities are written out, so driving the race needs no crate of its own.
async fn racing_deletes(
    router: &MultiServiceRouter,
    api_id: &str,
    racers: usize,
    case: &str,
) -> Vec<Answer> {
    match racers {
        1 => vec![delete_cache(router, api_id).await],
        2 => {
            let (first, second) =
                tokio::join!(delete_cache(router, api_id), delete_cache(router, api_id));
            vec![first, second]
        }
        3 => {
            let (first, second, third) = tokio::join!(
                delete_cache(router, api_id),
                delete_cache(router, api_id),
                delete_cache(router, api_id)
            );
            vec![first, second, third]
        }
        5 => {
            let (first, second, third, fourth, fifth) = tokio::join!(
                delete_cache(router, api_id),
                delete_cache(router, api_id),
                delete_cache(router, api_id),
                delete_cache(router, api_id),
                delete_cache(router, api_id)
            );
            vec![first, second, third, fourth, fifth]
        }
        other => panic!("case {case:?}: this suite races 1, 2, 3 or 5 teardowns, not {other}"),
    }
}

/// The teardown takes the instance down through the modelled `DELETING` status: a read
/// taken while the removal is being carried out reports the instance in that status, a
/// call that would CHANGE the cache meets a cache that has been deleted, and once the
/// instance is gone every reader keeps reporting that deletion rather than an API that
/// never had a cache.
///
/// The rows differ along the cache's settings — `ttl` across the published window, the
/// caching behaviour, the instance type (a generic identifier and a legacy one) and the
/// health-metrics setting — none of which the rule reads.
#[tokio::test]
async fn a_teardown_takes_the_instance_down_through_the_deleting_status() {
    let router = router();

    // (case, ttl)
    let rows: &[(&str, i64)] = &[
        ("the published minimum ttl", 1),
        ("a mid-window ttl", 300),
        ("the published maximum ttl", 3_600),
        ("another mid-window ttl", 42),
    ];

    for (index, (case, ttl)) in rows.iter().enumerate() {
        let behavior = CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()];
        let cache_type = CACHE_TYPES[index % CACHE_TYPES.len()];
        let health = HEALTH_METRICS[index % HEALTH_METRICS.len()];
        let case = format!("{case} ({cache_type} caching {behavior}, metrics {health})");
        let api_id = api_with_available_cache(
            &router,
            &format!("Api_teardown_window_{index}"),
            *ttl,
            behavior,
            cache_type,
            health,
            &case,
        )
        .await;

        let deleted = delete_cache(&router, &api_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected the teardown to be served, got {deleted:?}"
        );
        assert!(
            carries_no_members(&deleted.body),
            "case {case:?}: expected a response with no members, got {deleted:?}"
        );

        // The instance is on its way out, and that is what a read of it reports: the
        // status a caller waiting for the teardown watches.
        let read = get_cache(&router, &api_id).await;
        assert_eq!(
            (read.status, read.body["apiCache"]["status"].as_str()),
            (200, Some(DELETING)),
            "case {case:?}: expected the instance to be reported as being removed, got {read:?}"
        );
        assert_eq!(
            read.body["apiCache"]["ttl"].as_i64(),
            Some(*ttl),
            "case {case:?}: expected the instance being removed to report its own ttl, \
             got {read:?}"
        );

        // There is no cache left to CHANGE, though: both mutators meet a cache that has
        // been deleted.
        for (operation, answer, expected) in [
            (
                "FlushApiCache",
                flush_cache(&router, &api_id).await,
                DELETED_FLUSH,
            ),
            (
                "UpdateApiCache",
                update_cache(&router, &api_id, *ttl, behavior, cache_type).await,
                DELETED_UPDATE,
            ),
        ] {
            assert_eq!(
                (answer.status, answer.error_type.as_str(), answer.message.as_str()),
                (404, "NotFoundException", expected),
                "case {case:?} {operation} during the teardown: expected 404 \
                 NotFoundException {expected:?}, got {answer:?}"
            );
        }

        // Once the instance is gone the reader reports the DELETION — and keeps reporting
        // it, for as long as the slot stays empty.
        let refused = poll_until_no_instance(&router, &api_id, &case).await;
        assert_eq!(
            (refused.status, refused.error_type.as_str(), refused.message.as_str()),
            (404, "NotFoundException", DELETED_GET),
            "case {case:?}: expected 404 NotFoundException {DELETED_GET:?} once the instance \
             is gone, got {refused:?}"
        );
        for repeat in 1..=3 {
            let again = get_cache(&router, &api_id).await;
            assert_eq!(
                (again.status, again.message.as_str()),
                (404, DELETED_GET),
                "case {case:?} read {repeat} after the removal: expected the deletion to \
                 still be reported, got {again:?}"
            );
        }
        // And so do the calls that would change the cache.
        for (operation, answer, expected) in [
            (
                "FlushApiCache",
                flush_cache(&router, &api_id).await,
                DELETED_FLUSH,
            ),
            (
                "UpdateApiCache",
                update_cache(&router, &api_id, *ttl, behavior, cache_type).await,
                DELETED_UPDATE,
            ),
        ] {
            assert_eq!(
                (answer.status, answer.message.as_str()),
                (404, expected),
                "case {case:?} {operation} after the removal: expected 404 {expected:?}, \
                 got {answer:?}"
            );
        }
    }
}

/// Teardowns racing on one cache are ALL served — the instance lingers in `DELETING`
/// while it is being removed, so a racer that arrives during that window finds an
/// instance of its own to remove — and the API reports a cache that has been deleted
/// afterwards, however many racers there were.
///
/// The rows differ along how many teardowns race, and along whether the cache was
/// reconfigured (by racing updates of its own) before they do.
#[tokio::test]
async fn racing_teardowns_are_all_served_and_the_api_then_reports_a_deleted_cache() {
    let router = router();

    // (case, how many teardowns race, how many pairs of racing updates run first)
    let rows: &[(&str, usize, usize)] = &[
        ("two teardowns at once", 2, 0),
        ("three teardowns at once", 3, 0),
        ("five teardowns at once, after racing updates", 5, 2),
        ("two teardowns at once, after one pair of racing updates", 2, 1),
    ];

    for (index, (case, racers, update_rounds)) in rows.iter().enumerate() {
        let behavior = CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()];
        let cache_type = CACHE_TYPES[index % CACHE_TYPES.len()];
        let health = HEALTH_METRICS[index % HEALTH_METRICS.len()];
        let case = format!("{case} ({cache_type} caching {behavior}, metrics {health})");
        let api_id = api_with_available_cache(
            &router,
            &format!("Api_teardown_race_{index}"),
            300,
            behavior,
            cache_type,
            health,
            &case,
        )
        .await;

        for round in 1..=*update_rounds {
            let (first, second) = tokio::join!(
                update_cache(&router, &api_id, 300 + 10 * round as i64, behavior, cache_type),
                update_cache(&router, &api_id, 310 + 10 * round as i64, behavior, cache_type)
            );
            for (racer, answer) in [first, second].into_iter().enumerate() {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?} racing update {round}/{racer}: expected 200, got {answer:?}"
                );
            }
        }

        for (racer, answer) in racing_deletes(&router, &api_id, *racers, &case)
            .await
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                answer.status, 200,
                "case {case:?} racing teardown {racer}: expected status 200, got {answer:?}"
            );
            assert!(
                answer.error_type.is_empty(),
                "case {case:?} racing teardown {racer}: expected no error shape, got {answer:?}"
            );
            assert!(
                carries_no_members(&answer.body),
                "case {case:?} racing teardown {racer}: expected a response with no members, \
                 got {answer:?}"
            );
        }

        // No racer erased what the others did: the API reports the cache it HAD and lost,
        // not one it never had.
        let refused = poll_until_no_instance(&router, &api_id, &case).await;
        assert_eq!(
            (refused.status, refused.error_type.as_str(), refused.message.as_str()),
            (404, "NotFoundException", DELETED_GET),
            "case {case:?}: expected 404 NotFoundException {DELETED_GET:?} after the race, \
             got {refused:?}"
        );
    }
}

/// Why the API a row addresses holds no cache when the teardown arrives.
#[derive(Debug, Clone, Copy)]
enum Absence {
    /// No create ever ran for this API, so nothing ever filled the slot.
    NeverConfigured,
    /// A cache existed, a teardown removed it, and a redundant teardown completed that
    /// deletion — so the API holds no cache at all again.
    DeletionCompleted,
}

/// A teardown with no cache to remove is REFUSED: 404 `NotFoundException` with AppSync's
/// never-configured sentence in the teardown's own verb. The three other cache operations
/// meet the same absence and say so in their own verbs, and none of those refusals wrote
/// anything — a create still enables caching on the API, and the cache it makes is found
/// by every one of them.
///
/// The rows differ along WHY the API holds no cache (nothing ever created one, or a
/// completed deletion returned it to that state), along how many refused teardowns are
/// issued, and along the settings the control create carries.
#[tokio::test]
async fn a_teardown_with_no_cache_to_remove_is_refused_and_the_other_calls_agree() {
    let router = router();

    // (case, why the slot is empty, how many teardowns are issued, ttl of the control create)
    let rows: &[(&str, Absence, usize, i64)] = &[
        ("an api that never had a cache", Absence::NeverConfigured, 1, 60),
        ("an api that never had a cache, three teardowns", Absence::NeverConfigured, 3, 1),
        (
            "an api whose deletion a redundant teardown completed",
            Absence::DeletionCompleted,
            1,
            3_600,
        ),
        (
            "an api whose deletion a redundant teardown completed, two teardowns",
            Absence::DeletionCompleted,
            2,
            900,
        ),
    ];

    for (index, (case, absence, teardowns, ttl)) in rows.iter().enumerate() {
        let behavior = CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()];
        let cache_type = CACHE_TYPES[index % CACHE_TYPES.len()];
        let health = HEALTH_METRICS[index % HEALTH_METRICS.len()];
        let case = format!("{case} ({cache_type} caching {behavior}, metrics {health})");
        let api_id = create_api(&router, &format!("Api_teardown_absent_{index}")).await;

        if let Absence::DeletionCompleted = absence {
            let created = create_cache(&router, &api_id, 120, behavior, cache_type, health).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected the seed create to be accepted, got {created:?}"
            );
            poll_until_status(&router, &api_id, AVAILABLE, &case).await;
            let seeded = delete_cache(&router, &api_id).await;
            assert_eq!(
                seeded.status, 200,
                "case {case:?}: expected the seed teardown to be served, got {seeded:?}"
            );
            poll_until_no_instance(&router, &api_id, &case).await;
            // The redundant teardown that completes the deletion the first one recorded:
            // served, and it is what returns the API to holding no cache at all.
            let completed = delete_cache(&router, &api_id).await;
            assert_eq!(
                completed.status, 200,
                "case {case:?}: expected the redundant teardown to be served, got {completed:?}"
            );
        }

        // Every teardown is refused, and the refusal writes nothing, so the next one meets
        // exactly the same answer.
        for attempt in 1..=*teardowns {
            let refused = delete_cache(&router, &api_id).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_type.as_str(),
                    refused.message.as_str()
                ),
                (404, "NotFoundException", ABSENT_DELETE),
                "case {case:?} teardown {attempt}: expected 404 NotFoundException \
                 {ABSENT_DELETE:?}, got {refused:?}"
            );
        }

        // The other three calls agree about the absence, each in its own verb.
        for (operation, answer, expected) in [
            ("GetApiCache", get_cache(&router, &api_id).await, ABSENT_GET),
            (
                "FlushApiCache",
                flush_cache(&router, &api_id).await,
                ABSENT_FLUSH,
            ),
            (
                "UpdateApiCache",
                update_cache(&router, &api_id, *ttl, behavior, cache_type).await,
                ABSENT_UPDATE,
            ),
        ] {
            assert_eq!(
                (answer.status, answer.error_type.as_str(), answer.message.as_str()),
                (404, "NotFoundException", expected),
                "case {case:?} {operation}: expected 404 NotFoundException {expected:?}, \
                 got {answer:?}"
            );
        }

        // The control: none of those refusals wrote anything, so a create still enables
        // caching, and the cache it makes is found by the reader, the flush and the
        // teardown alike.
        let created = create_cache(&router, &api_id, *ttl, behavior, cache_type, health).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the control create to be accepted, got {created:?}"
        );
        let available = poll_until_status(&router, &api_id, AVAILABLE, &case).await;
        assert_eq!(
            available.body["apiCache"]["ttl"].as_i64(),
            Some(*ttl),
            "case {case:?}: expected the control cache to report its own ttl, got {available:?}"
        );
        let flushed = flush_cache(&router, &api_id).await;
        assert_eq!(
            flushed.status, 200,
            "case {case:?}: expected the flush to find the control cache, got {flushed:?}"
        );
        let torn_down = delete_cache(&router, &api_id).await;
        assert_eq!(
            torn_down.status, 200,
            "case {case:?}: expected the teardown to find the control cache, got {torn_down:?}"
        );
    }
}

/// The redundant teardown is what resolves the deletion: issued once the removal is done
/// it is served and returns the API to holding no cache at all, so the reader stops
/// reporting a deleted cache and the NEXT teardown — which now has neither an instance nor
/// a deletion to complete — is refused. A create fills the slot again either way.
///
/// The rows differ along how many teardowns started the removal (one, or several racing)
/// and along the settings the cache was created with.
#[tokio::test]
async fn a_redundant_teardown_completes_the_deletion_and_returns_the_api_to_never_configured() {
    let router = router();

    // (case, how many teardowns start the removal, ttl)
    let rows: &[(&str, usize, i64)] = &[
        ("one teardown started the removal", 1, 1),
        ("two racing teardowns started it", 2, 600),
        ("five racing teardowns started it", 5, 3_600),
    ];

    for (index, (case, racers, ttl)) in rows.iter().enumerate() {
        let behavior = CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()];
        let cache_type = CACHE_TYPES[index % CACHE_TYPES.len()];
        let health = HEALTH_METRICS[index % HEALTH_METRICS.len()];
        let case = format!("{case} ({cache_type} caching {behavior}, metrics {health})");
        let api_id = api_with_available_cache(
            &router,
            &format!("Api_teardown_redundant_{index}"),
            *ttl,
            behavior,
            cache_type,
            health,
            &case,
        )
        .await;

        for (racer, answer) in racing_deletes(&router, &api_id, *racers, &case)
            .await
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                answer.status, 200,
                "case {case:?} teardown {racer}: expected status 200, got {answer:?}"
            );
        }

        let deleted = poll_until_no_instance(&router, &api_id, &case).await;
        assert_eq!(
            deleted.message, DELETED_GET,
            "case {case:?}: expected {DELETED_GET:?} while the deletion stands, got {deleted:?}"
        );

        // The redundant teardown: served, and it completes the deletion rather than
        // starting another, so the reader reports an API that holds no cache at all.
        let completed = delete_cache(&router, &api_id).await;
        assert_eq!(
            completed.status, 200,
            "case {case:?}: expected the redundant teardown to be served, got {completed:?}"
        );
        assert!(
            carries_no_members(&completed.body),
            "case {case:?}: expected a response with no members, got {completed:?}"
        );
        let read = get_cache(&router, &api_id).await;
        assert_eq!(
            (read.status, read.message.as_str()),
            (404, ABSENT_GET),
            "case {case:?}: expected the reader to report an API that holds no cache, \
             got {read:?}"
        );

        // And the one after it has nothing left to remove at all.
        let refused = delete_cache(&router, &api_id).await;
        assert_eq!(
            (
                refused.status,
                refused.error_type.as_str(),
                refused.message.as_str()
            ),
            (404, "NotFoundException", ABSENT_DELETE),
            "case {case:?}: expected 404 NotFoundException {ABSENT_DELETE:?} for the teardown \
             with nothing to remove, got {refused:?}"
        );

        // The slot is fillable throughout: the API can cache again.
        let recreated = create_cache(&router, &api_id, *ttl, behavior, cache_type, health).await;
        assert_eq!(
            recreated.status, 200,
            "case {case:?}: expected the cache to be creatable again, got {recreated:?}"
        );
        assert_eq!(
            recreated.body["apiCache"]["status"].as_str(),
            Some("CREATING"),
            "case {case:?}: expected the new instance to start its lifecycle over, \
             got {recreated:?}"
        );
    }
}
