// SPDX-License-Identifier: CC0-1.0

//! `DeleteApiCache`, end to end over HTTP.
//!
//! One rule, asserted over a table that spans the class rather than a single recorded
//! request: the teardown removes the cache instance the API HOLDS, once that instance has
//! been created. While the create itself has not completed the delete is refused with the
//! operation's modelled `BadRequestException` (400) carrying AppSync's fixed sentence for
//! that class, and the instance is left exactly where it was; once the instance exists the
//! delete answers 200 with a response that carries no members and takes the instance down
//! over a span, reported in the modelled `DELETING` status, so a teardown racing with that
//! removal finds an instance of its own and is served as well.
//!
//! An empty cache slot records WHY it is empty, for as long as it stays empty: after a
//! teardown removed an instance every call that addresses the cache reports a cache that
//! HAS BEEN DELETED, until the next teardown completes that deletion — which is served,
//! and returns the API to holding no cache at all — or a create fills the slot again. A
//! teardown with neither an instance to remove nor a deletion to complete asks for the
//! removal of a cache the API never had, and is refused 404 `NotFoundException` naming
//! itself.
//!
//! Nothing here waits for a span where it can poll for the condition it needs instead.
//!
//! The `call`/`create_api`/`Answer` harness is the error-contract suite's.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::{state, EmulatedService};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, Answer};

/// The number of characters an AppSync API id has, as the service itself states it, so
/// the id-shape rows below cannot drift from the rule the service applies.
const API_ID_LEN: usize = state::API_ID_LEN;

/// The span the service records for a cache's transition to available — read from the
/// service itself, so this suite waits exactly as long as the service asks for rather
/// than on a number of its own.
const CACHE_SPAN: Duration = state::CACHE_SPAN;

/// AppSync's fixed refusal text for a delete of a cache that is not available; the
/// service's own copy, so the assertion cannot drift from what the handler answers.
const NOT_DELETABLE: &str = state::CACHE_NOT_DELETABLE_MESSAGE;

/// How long a poll below keeps looking for the condition it waits on. Generous on
/// purpose: the condition, not this bound, is what a case is about.
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
const CACHE_TYPES: &[&str] = &["SMALL", "LARGE_12X", "R4_LARGE", "T2_MEDIUM"];

/// `CacheHealthMetricsConfig`'s whole value set.
/// Source: `smithy_query(query="shape", shape="CacheHealthMetricsConfig")`.
const HEALTH_METRICS: &[&str] = &["ENABLED", "DISABLED"];

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
        "atRestEncryptionEnabled": false,
        "healthMetricsConfig": health,
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

/// Delete the API's cache.
async fn delete_cache(router: &MultiServiceRouter, api_id: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/v1/apis/{api_id}/ApiCaches"),
        None,
    )
    .await
}

/// Whether the response body carries no members at all — the shape of the modelled
/// empty `DeleteApiCacheResponse`, which is either an object with no members or no body
/// at all.
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

/// Poll `GetApiCache` until it reports no instance at all, and answer that refusal.
async fn poll_until_no_instance(router: &MultiServiceRouter, api_id: &str, case: &str) -> Answer {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let read = get_cache(router, api_id).await;
        if read.status != 200 {
            return read;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the instance to be removed, last answer {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create one API and one cache under it, and return the API's id.
async fn api_with_cache(
    router: &MultiServiceRouter,
    name: &str,
    ttl: i64,
    behavior: &str,
    cache_type: &str,
    health: &str,
) -> String {
    let api_id = create_api(router, name).await;
    let created = create_cache(router, &api_id, ttl, behavior, cache_type, health).await;
    assert_eq!(
        created.status, 200,
        "case seed a {cache_type} cache for {name:?}: expected 200, got {created:?}"
    );
    api_id
}

/// The rule's refusal half: while the create itself has not completed the delete is
/// refused, with 400 `BadRequestException` and AppSync's fixed sentence, and the cache
/// is left in place — so the very next read still reports it and a later delete, once
/// the clock has made the instance available, is served.
///
/// The rows differ along every input the rule generalises over: the cache's settings
/// (`ttl`, caching behaviour, instance type, health metrics) and how many times the
/// refused delete is repeated.
#[tokio::test]
async fn deleting_a_cache_that_is_still_being_created_is_refused_and_keeps_the_cache() {
    let router = router();

    let rows: &[(&str, i64)] = &[
        ("the published minimum ttl, being created", 1),
        ("a mid-window ttl, being created", 300),
        ("the published maximum ttl, being created", 3_600),
        ("another mid-window ttl, being created", 42),
    ];

    for (index, (case, ttl)) in rows.iter().enumerate() {
        let cache_type = CACHE_TYPES[index % CACHE_TYPES.len()];
        let behavior = CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()];
        let health = HEALTH_METRICS[index % HEALTH_METRICS.len()];
        let case = format!("{case} ({cache_type} caching {behavior}, metrics {health})");
        let api_id = api_with_cache(
            &router,
            &format!("Api_delete_busy_{index}"),
            *ttl,
            behavior,
            cache_type,
            health,
        )
        .await;

        // Two refusals in a row: the refusal writes nothing, so the second answers
        // exactly as the first did.
        for attempt in 1..=2 {
            let refused = delete_cache(&router, &api_id).await;
            assert_eq!(
                refused.status, 400,
                "case {case:?} delete {attempt}: expected status 400, got {refused:?}"
            );
            assert_eq!(
                refused.error_type, "BadRequestException",
                "case {case:?} delete {attempt}: expected error type BadRequestException, \
                 got {refused:?}"
            );
            assert_eq!(
                refused.message, NOT_DELETABLE,
                "case {case:?} delete {attempt}: expected the fixed refusal sentence, \
                 got {refused:?}"
            );

            // The refused delete did not remove the cache: it is still reported, with
            // the settings the create or the update left on it.
            let read = get_cache(&router, &api_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?} delete {attempt}: expected the cache to survive the refusal, \
                 got {read:?}"
            );
            assert_eq!(
                read.body["apiCache"]["ttl"].as_i64(),
                Some(*ttl),
                "case {case:?} delete {attempt}: expected the stored ttl to be unchanged, \
                 got {read:?}"
            );
            assert_eq!(
                read.body["apiCache"]["type"].as_str(),
                Some(cache_type),
                "case {case:?} delete {attempt}: expected the stored type to be unchanged, \
                 got {read:?}"
            );
        }

        // And the refusal is about the status, not about the operation: once the clock
        // has made the instance available the very same call is served.
        tokio::time::sleep(CACHE_SPAN + Duration::from_millis(100)).await;
        let settled = get_cache(&router, &api_id).await;
        assert_eq!(
            settled.body["apiCache"]["status"].as_str(),
            Some("AVAILABLE"),
            "case {case:?}: expected the instance to be available on the clock, got {settled:?}"
        );
        let served = delete_cache(&router, &api_id).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: expected the delete to be served once available, got {served:?}"
        );
    }
}

/// The rule's served half: a delete of an available instance answers 200 with no
/// members and takes the instance down — reported in the modelled `DELETING` status
/// while the removal is carried out, and gone afterwards, so every operation that
/// resolves the cache answers not-found and each of them names the deletion this API's
/// slot records. The teardown that follows completes that deletion and is served, which
/// returns the API to holding no cache at all; the one after it has nothing left to
/// remove and is refused. A fresh create is accepted and starts the lifecycle again.
///
/// The rows differ along the cache's settings and along how it reached the available
/// status (straight from the create, or after its settings were applied again).
#[tokio::test]
async fn deleting_an_available_cache_removes_it_and_answers_without_members() {
    let router = router();

    let rows: &[(&str, i64, bool)] = &[
        ("created then deleted", 1, false),
        ("created, updated, then deleted", 900, true),
        ("the published maximum ttl, created then deleted", 3_600, false),
    ];

    for (index, (case, ttl, update_first)) in rows.iter().enumerate() {
        let cache_type = CACHE_TYPES[index % CACHE_TYPES.len()];
        let behavior = CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()];
        let health = HEALTH_METRICS[index % HEALTH_METRICS.len()];
        let case = format!("{case} ({cache_type} caching {behavior}, metrics {health})");
        let api_id = api_with_cache(
            &router,
            &format!("Api_delete_available_{index}"),
            *ttl,
            behavior,
            cache_type,
            health,
        )
        .await;
        poll_until_status(&router, &api_id, "AVAILABLE", &case).await;

        if *update_first {
            let updated = call(
                &router,
                "POST",
                &format!("/v1/apis/{api_id}/ApiCaches/update"),
                Some(json!({
                    "ttl": ttl,
                    "apiCachingBehavior": behavior,
                    "type": cache_type,
                })),
            )
            .await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected the update to be accepted, got {updated:?}"
            );
            poll_until_status(&router, &api_id, "AVAILABLE", &case).await;
        }

        let deleted = delete_cache(&router, &api_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected status 200, got {deleted:?}"
        );
        assert!(
            carries_no_members(&deleted.body),
            "case {case:?}: expected a response with no members, got {deleted:?}"
        );

        // Read straight after the teardown, the instance is still being removed, and that
        // is what the read reports — while the two calls that would CHANGE the cache have
        // none left to change and name the deletion.
        let going = get_cache(&router, &api_id).await;
        assert_eq!(
            (going.status, going.body["apiCache"]["status"].as_str()),
            (200, Some("DELETING")),
            "case {case:?}: expected the instance to be reported as being removed, \
             got {going:?}"
        );

        // Once the instance is gone every operation that addresses the cache answers for
        // the absence — each naming the call that met it, and all of them naming the
        // deletion rather than an API that never had a cache.
        let gone = poll_until_no_instance(&router, &api_id, &case).await;
        assert_eq!(
            (gone.status, gone.error_type.as_str(), gone.message.as_str()),
            (
                404,
                "NotFoundException",
                state::cache_deleted_message(state::CacheCall::Get).as_str()
            ),
            "case {case:?}: expected the reader to report the deletion, got {gone:?}"
        );
        for (op, method, path, body, expected_message) in [
            (
                "get",
                "GET",
                format!("/v1/apis/{api_id}/ApiCaches"),
                None,
                state::cache_deleted_message(state::CacheCall::Get),
            ),
            (
                "flush",
                "DELETE",
                format!("/v1/apis/{api_id}/FlushCache"),
                None,
                state::cache_deleted_message(state::CacheCall::Flush),
            ),
            (
                "update",
                "POST",
                format!("/v1/apis/{api_id}/ApiCaches/update"),
                Some(json!({
                    "ttl": ttl,
                    "apiCachingBehavior": behavior,
                    "type": cache_type,
                })),
                state::cache_deleted_message(state::CacheCall::Update),
            ),
        ] {
            let answer = call(&router, method, &path, body).await;
            assert_eq!(
                answer.status, 404,
                "case {case:?} {op} after delete: expected status 404, got {answer:?}"
            );
            assert_eq!(
                answer.error_type, "NotFoundException",
                "case {case:?} {op} after delete: expected error type NotFoundException, \
                 got {answer:?}"
            );
            assert_eq!(
                answer.message, expected_message,
                "case {case:?} {op} after delete: expected {expected_message:?}, \
                 got {answer:?}"
            );
        }

        // The next teardown has the deletion the first one recorded to complete, so it is
        // served the same empty 200 — and that is what returns the API to holding no cache
        // at all, which is what the reader reports from then on.
        let completing = delete_cache(&router, &api_id).await;
        assert_eq!(
            completing.status, 200,
            "case {case:?} delete 2: expected status 200, got {completing:?}"
        );
        assert!(
            carries_no_members(&completing.body),
            "case {case:?} delete 2: expected a response with no members, got {completing:?}"
        );
        let expected = state::cache_not_found_message(state::CacheCall::Get);
        let read = get_cache(&router, &api_id).await;
        assert_eq!(
            (read.status, read.error_type.as_str(), read.message.as_str()),
            (404, "NotFoundException", expected.as_str()),
            "case {case:?} delete 2: expected the reader to report an API that holds no \
             cache, got {read:?}"
        );

        // And every teardown after it has neither an instance to remove nor a deletion to
        // complete, so each is refused — identically, since the refusal writes nothing.
        let absent_delete = state::cache_not_found_message(state::CacheCall::Delete);
        for attempt in 3..=4 {
            let refused = delete_cache(&router, &api_id).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_type.as_str(),
                    refused.message.as_str()
                ),
                (404, "NotFoundException", absent_delete.as_str()),
                "case {case:?} delete {attempt}: expected 404 NotFoundException \
                 {absent_delete:?}, got {refused:?}"
            );
        }

        // The API itself survived the delete, and holds room for a cache again: the
        // create that was a duplicate before is accepted, and the new instance starts
        // the lifecycle over.
        let recreated = create_cache(&router, &api_id, *ttl, behavior, cache_type, health).await;
        assert_eq!(
            recreated.status, 200,
            "case {case:?}: expected the cache to be creatable again, got {recreated:?}"
        );
        assert_eq!(
            recreated.body["apiCache"]["status"].as_str(),
            Some("CREATING"),
            "case {case:?}: expected the recreated instance to start over, got {recreated:?}"
        );
    }
}

/// What the teardown answers on an empty cache slot depends on WHY the slot is empty,
/// and the `apiId` checks ahead of that are untouched — an `apiId` that is not an API id
/// at all is the bad request the shape rule makes it, on this operation as on every other
/// that resolves one, a well-formed id naming no API is a not-found about the API, and
/// neither is the status refusal, so the refusals stay distinguishable.
///
/// The rows differ along what the `apiId` names and along its length: an API that never
/// had a cache, an API whose instance is still being removed, an API whose deletion is
/// standing, well-formed ids of several lengths that name no API, and ids past the length
/// an AppSync API id has.
#[tokio::test]
async fn what_the_teardown_answers_on_an_empty_slot_and_the_id_checks_are_unchanged() {
    let router = router();

    let cacheless = create_api(&router, "Api_delete_no_cache").await;
    // An API whose instance a teardown is still removing: a teardown racing with that one
    // finds an instance of its own.
    let removing = api_with_cache(
        &router,
        "Api_delete_removing",
        120,
        "PER_RESOLVER_CACHING",
        "LARGE",
        "DISABLED",
    )
    .await;
    poll_until_status(&router, &removing, "AVAILABLE", "seed the instance to remove").await;
    let seeded = delete_cache(&router, &removing).await;
    assert_eq!(
        seeded.status, 200,
        "case start removing the instance: expected 200, got {seeded:?}"
    );
    // An API whose instance is gone and whose deletion is standing: the teardown has that
    // deletion to complete.
    let deleted = api_with_cache(
        &router,
        "Api_delete_deleted",
        120,
        "PER_RESOLVER_CACHING",
        "LARGE",
        "DISABLED",
    )
    .await;
    poll_until_status(&router, &deleted, "AVAILABLE", "seed the deletion").await;
    let seeded = delete_cache(&router, &deleted).await;
    assert_eq!(
        seeded.status, 200,
        "case record the deletion: expected 200, got {seeded:?}"
    );
    poll_until_no_instance(&router, &deleted, "seed the deletion").await;
    let absent_short = "a".to_string();
    let absent_exact = "b".repeat(API_ID_LEN);
    let overlong_by_one = "c".repeat(API_ID_LEN + 1);
    let overlong = "d".repeat(64);
    let absent_delete = state::cache_not_found_message(state::CacheCall::Delete);

    // (case, apiId, expected status, the message the refusal has to carry where the row
    // pins one — the id rows pin the class instead, since the id's own refusals are the
    // error-contract suite's rule)
    let rows: &[(&str, &String, u16, Option<&str>)] = &[
        (
            "an api that never had a cache",
            &cacheless,
            404,
            Some(absent_delete.as_str()),
        ),
        ("an api whose instance is being removed", &removing, 200, None),
        ("an api whose deletion is standing", &deleted, 200, None),
        ("a one-character id naming no api", &absent_short, 404, None),
        ("a well-formed id naming no api", &absent_exact, 404, None),
        ("an id one past the id length", &overlong_by_one, 400, None),
        ("an id of 64 characters", &overlong, 400, None),
    ];

    for (case, api_id, expected, expected_message) in rows {
        let answer = delete_cache(&router, api_id).await;
        assert_eq!(
            answer.status, *expected,
            "case {case:?}: expected status {expected}, got {answer:?}"
        );
        if *expected == 200 {
            assert!(
                carries_no_members(&answer.body),
                "case {case:?}: expected a response with no members, got {answer:?}"
            );
            assert!(
                answer.error_type.is_empty(),
                "case {case:?}: expected no error shape at all, got {answer:?}"
            );
            continue;
        }
        let error_type = if *expected == 404 {
            "NotFoundException"
        } else {
            "BadRequestException"
        };
        assert_eq!(
            answer.error_type, error_type,
            "case {case:?}: expected error type {error_type}, got {answer:?}"
        );
        if let Some(expected_message) = expected_message {
            assert_eq!(
                answer.message, *expected_message,
                "case {case:?}: expected {expected_message:?}, got {answer:?}"
            );
        }
        assert_ne!(
            answer.message, NOT_DELETABLE,
            "case {case:?}: expected the refusal to be about the resolved id or the absent \
             cache, not the instance status, got {answer:?}"
        );
        assert!(
            !answer.message.is_empty(),
            "case {case:?}: expected the refusal to say what is wrong, got {answer:?}"
        );
    }

    // The refusal created nothing, so the cache the API holds none of can still be
    // created — and that create is what gives the very same call something to remove.
    let created = create_cache(
        &router,
        &cacheless,
        60,
        "FULL_REQUEST_CACHING",
        "SMALL",
        "ENABLED",
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case create the cache the delete wanted: expected 200, got {created:?}"
    );
    poll_until_status(&router, &cacheless, "AVAILABLE", "the created cache").await;
    let served = delete_cache(&router, &cacheless).await;
    assert_eq!(
        served.status, 200,
        "case delete once the cache exists and is available: expected status 200, got {served:?}"
    );
}

/// The removal is scoped to the cache the call addressed: deleting one API's cache
/// leaves every other API's cache exactly as it was.
#[tokio::test]
async fn a_delete_removes_only_the_cache_it_addressed() {
    let router = router();

    // Three APIs, each with a cache of its own; only the middle one is deleted.
    let mut api_ids = Vec::new();
    for index in 0..3 {
        api_ids.push(
            api_with_cache(
                &router,
                &format!("Api_delete_scope_{index}"),
                60 + index as i64,
                CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()],
                CACHE_TYPES[index % CACHE_TYPES.len()],
                HEALTH_METRICS[index % HEALTH_METRICS.len()],
            )
            .await,
        );
    }
    for (index, api_id) in api_ids.iter().enumerate() {
        poll_until_status(&router, api_id, "AVAILABLE", &format!("api {index}")).await;
    }

    let mut before = Vec::new();
    for api_id in &api_ids {
        before.push(get_cache(&router, api_id).await.body["apiCache"].clone());
    }

    let deleted_id = api_ids[1].clone();
    let deleted = delete_cache(&router, &deleted_id).await;
    assert_eq!(
        deleted.status, 200,
        "case delete the second api's cache: expected status 200, got {deleted:?}"
    );
    // Only that API's instance is on its way out, and it is gone once the clock says so.
    poll_until_no_instance(&router, &deleted_id, "the deleted api's cache").await;

    for (index, api_id) in api_ids.iter().enumerate() {
        let case = format!("api {index} after deleting api 1's cache");
        let read = get_cache(&router, api_id).await;
        if api_id == &deleted_id {
            assert_eq!(
                read.status, 404,
                "case {case:?}: expected the deleted cache to be gone, got {read:?}"
            );
            continue;
        }
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the untouched cache to be readable, got {read:?}"
        );
        for member in [
            "ttl",
            "apiCachingBehavior",
            "type",
            "healthMetricsConfig",
            "status",
            "transitEncryptionEnabled",
            "atRestEncryptionEnabled",
        ] {
            assert_eq!(
                read.body["apiCache"][member], before[index][member],
                "case {case:?}: expected the stored {member} to be unchanged, got {read:?}"
            );
        }
    }
}

/// Why the API a row addresses holds no cache when the redundant deletes arrive.
#[derive(Debug, Clone, Copy)]
enum Absence {
    /// No create ever ran for this API, so nothing ever filled the slot.
    NeverConfigured,
    /// A cache existed and a delete emptied the slot.
    Deleted,
}

/// Repeating the teardown on an empty slot RESOLVES the deletion and then finds nothing:
/// the first repetition has the deletion an earlier teardown recorded to complete, so it
/// is served the modelled empty 200 and the reader stops reporting a deleted cache, and
/// every repetition after it has neither an instance to remove nor a deletion to complete
/// and is refused 404 `NotFoundException` naming itself. On an API that never had a cache
/// the very first teardown is already that refusal. A create still fills the slot again
/// afterwards, whichever way the row got there.
///
/// The rows differ along what the rule generalises over: why the API holds no cache, how
/// many redundant deletes are issued, and the settings the deleted row's cache was
/// created with.
#[tokio::test]
async fn repeating_the_teardown_resolves_the_deletion_and_then_finds_no_cache() {
    let router = router();

    // AppSync's sentences, taken from the service so the assertions cannot drift from the
    // wording the handlers answer.
    let absent_get = state::cache_not_found_message(state::CacheCall::Get);
    let deleted_get = state::cache_deleted_message(state::CacheCall::Get);
    let absent_delete = state::cache_not_found_message(state::CacheCall::Delete);

    // (case, why the cache is absent, how many redundant deletes, ttl of the seeded cache)
    let rows: &[(&str, Absence, usize, i64)] = &[
        ("an api that never had a cache, deleted once", Absence::NeverConfigured, 1, 60),
        ("an api that never had a cache, deleted five times", Absence::NeverConfigured, 5, 60),
        ("an api whose minimum-ttl cache was deleted, once more", Absence::Deleted, 1, 1),
        ("an api whose mid-window cache was deleted, twice more", Absence::Deleted, 2, 600),
        ("an api whose maximum-ttl cache was deleted, five times more", Absence::Deleted, 5, 3_600),
    ];

    for (index, (case, absence, repeats, ttl)) in rows.iter().enumerate() {
        let cache_type = CACHE_TYPES[index % CACHE_TYPES.len()];
        let behavior = CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()];
        let health = HEALTH_METRICS[index % HEALTH_METRICS.len()];
        let case = format!("{case} ({cache_type} caching {behavior}, metrics {health})");
        let api_id = create_api(&router, &format!("Api_delete_repeated_{index}")).await;

        if let Absence::Deleted = absence {
            let created = create_cache(&router, &api_id, *ttl, behavior, cache_type, health).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected the seed create to be accepted, got {created:?}"
            );
            // The instance has to have been created before it can be torn down.
            poll_until_status(&router, &api_id, "AVAILABLE", &case).await;
            let first = delete_cache(&router, &api_id).await;
            assert_eq!(
                first.status, 200,
                "case {case:?}: expected the seed delete to be served, got {first:?}"
            );
            // And the removal that teardown started has to be done before the rows below
            // read an empty slot rather than the instance being removed.
            poll_until_no_instance(&router, &api_id, &case).await;
        }

        // What a reader answers before the redundant deletes: the deletion this row's
        // teardown recorded, or an API that never had a cache.
        let expected_before = match absence {
            Absence::NeverConfigured => &absent_get,
            Absence::Deleted => &deleted_get,
        };
        let before = get_cache(&router, &api_id).await;
        assert_eq!(
            (before.status, before.error_type.as_str(), before.message.as_str()),
            (404, "NotFoundException", expected_before.as_str()),
            "case {case:?}: expected the reader to report this row's absence, got {before:?}"
        );

        for attempt in 1..=*repeats {
            // The FIRST repetition of a row whose deletion is standing has that deletion
            // to complete, so it is served; everything else has nothing left to remove.
            let completes_a_deletion = attempt == 1 && matches!(absence, Absence::Deleted);
            let again = delete_cache(&router, &api_id).await;
            if completes_a_deletion {
                assert_eq!(
                    again.status, 200,
                    "case {case:?} redundant delete {attempt}: expected status 200, got {again:?}"
                );
                assert!(
                    again.error_type.is_empty(),
                    "case {case:?} redundant delete {attempt}: expected no error shape, \
                     got {again:?}"
                );
                assert!(
                    carries_no_members(&again.body),
                    "case {case:?} redundant delete {attempt}: expected a response with no \
                     members, got {again:?}"
                );
            } else {
                assert_eq!(
                    (again.status, again.error_type.as_str(), again.message.as_str()),
                    (404, "NotFoundException", absent_delete.as_str()),
                    "case {case:?} redundant delete {attempt}: expected 404 NotFoundException \
                     {absent_delete:?}, got {again:?}"
                );
            }

            // From the repetition that completed the deletion onwards the reader reports
            // an API that holds no cache at all, and no later repetition changes that.
            let read = get_cache(&router, &api_id).await;
            assert_eq!(
                (read.status, read.error_type.as_str(), read.message.as_str()),
                (404, "NotFoundException", absent_get.as_str()),
                "case {case:?} redundant delete {attempt}: expected the reader to report an \
                 API that holds no cache, got {read:?}"
            );
        }

        // And the slot is still fillable, so none of those calls left the API unable to
        // cache again.
        let recreated = create_cache(&router, &api_id, *ttl, behavior, cache_type, health).await;
        assert_eq!(
            recreated.status, 200,
            "case {case:?}: expected the cache to be creatable afterwards, \
             got {recreated:?}"
        );
        assert_eq!(
            recreated.body["apiCache"]["ttl"].as_i64(),
            Some(*ttl),
            "case {case:?}: expected the new instance to report its own ttl, got {recreated:?}"
        );
    }
}
