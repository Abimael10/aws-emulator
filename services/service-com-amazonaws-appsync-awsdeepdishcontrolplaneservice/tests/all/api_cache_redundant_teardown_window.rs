// SPDX-License-Identifier: CC0-1.0

//! WHICH of AppSync's two absent-cache sentences an API answers after a redundant
//! teardown, end to end over HTTP.
//!
//! One rule, asserted over a table that spans the class rather than the single recorded
//! request: a teardown of an API's cache that arrives while the removal of the instance is
//! still being carried out finds that instance and removes it, so the API keeps reporting a
//! cache that HAS BEEN DELETED, whereas a teardown that arrives once the slot is empty and
//! the removal has settled has that deletion to complete rather than an instance to remove,
//! which returns the API to holding no cache at all — and from then on every call that
//! addresses the cache reports the never-configured absence in its own verb, and the next
//! teardown, with neither an instance nor a deletion left, is refused 404
//! `NotFoundException`.
//!
//! So which sentence applies turns on whether the teardown met a removal still in flight or
//! a settled one, never on how many teardowns have run: the rows below differ along how many
//! teardowns arrive inside the removal window (one, or several racing) and along the settings
//! of the cache they tear down, and every row ends in the same two-step answer.
//!
//! The sentences are written out in full rather than read from the service, so the
//! assertions are about AppSync's wording and not about whatever the handlers happen to
//! build.
//!
//! Nothing here waits for a span: every wait polls for the CONDITION it needs, under a
//! deadline generous enough that a loaded host only goes round the loop more often.
//!
//! The `call`/`create_api`/`router`/`Answer` harness is the error-contract suite's.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, router, Answer};

/// How long a poll below keeps looking for the condition it waits on. Generous on purpose:
/// the condition, not this bound, is what each case is about.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The `ApiCacheStatus` a provisioned instance is reported in.
/// Source: `smithy_query(query="shape", shape="ApiCacheStatus")`.
const AVAILABLE: &str = "AVAILABLE";

/// The `ApiCacheStatus` an instance whose teardown has been accepted is reported in.
const DELETING: &str = "DELETING";

/// The `ApiCachingBehavior` values the rows below span.
/// Source: `smithy_query(query="shape", shape="ApiCachingBehavior")`.
const CACHING_BEHAVIORS: &[&str] = &[
    "FULL_REQUEST_CACHING",
    "PER_RESOLVER_CACHING",
    "OPERATION_LEVEL_CACHING",
];

/// `ApiCacheType` values spanning the class the rule generalises over: generic instance
/// identifiers and the legacy EC2-style ones alike.
/// Source: `smithy_query(query="shape", shape="ApiCacheType")`.
const CACHE_TYPES: &[&str] = &["SMALL", "MEDIUM", "LARGE_2X", "T2_SMALL"];

/// `CacheHealthMetricsConfig`'s whole value set.
/// Source: `smithy_query(query="shape", shape="CacheHealthMetricsConfig")`.
const HEALTH_METRICS: &[&str] = &["ENABLED", "DISABLED"];

/// The sentences the calls that address an absent cache answer, one per verb — the reader's,
/// the flush's and the update's.
#[derive(Debug, Clone, Copy)]
struct Sentences {
    get: &'static str,
    flush: &'static str,
    update: &'static str,
}

/// AWS's sentences for a call that found no cache because a teardown removed the one the API
/// had: the caller is asked to RE-create it. Fixed text apart from the verb the attempted
/// call supplies.
const DELETED: Sentences = Sentences {
    get: "Unable to get the cache as it has been deleted, please re-create the cache.",
    flush: "Unable to flush the cache as it has been deleted, please re-create the cache.",
    update: "Unable to update the cache as it has been deleted, please re-create the cache.",
};

/// AWS's sentences for a call that found no cache on an API that holds none at all: the
/// caller is asked to create one. Fixed text apart from the verb the attempted call
/// supplies — the teardown's included ([`ABSENT_DELETE`]).
const ABSENT: Sentences = Sentences {
    get: "Unable to get the cache as it doesn't exist, please create the cache first.",
    flush: "Unable to flush the cache as it doesn't exist, please create the cache first.",
    update: "Unable to update the cache as it doesn't exist, please create the cache first.",
};

/// The teardown's own never-configured sentence, in its own verb.
const ABSENT_DELETE: &str =
    "Unable to delete the cache as it doesn't exist, please create the cache first.";

/// The settings one row's cache is created with — none of which the rule reads, so the rows
/// vary them freely.
#[derive(Debug, Clone, Copy)]
struct CacheSettings {
    ttl: i64,
    behavior: &'static str,
    cache_type: &'static str,
    health: &'static str,
}

impl CacheSettings {
    /// The settings of row `index`, at `ttl`: each row takes the next value of every set the
    /// create's enums declare, so the table spans them rather than repeating one.
    fn for_row(index: usize, ttl: i64) -> Self {
        Self {
            ttl,
            behavior: CACHING_BEHAVIORS[index % CACHING_BEHAVIORS.len()],
            cache_type: CACHE_TYPES[index % CACHE_TYPES.len()],
            health: HEALTH_METRICS[index % HEALTH_METRICS.len()],
        }
    }

    /// A `CreateApiCache` body with the members the create requires, plus the optional
    /// health-metrics setting.
    fn create_body(&self) -> Value {
        json!({
            "ttl": self.ttl,
            "apiCachingBehavior": self.behavior,
            "type": self.cache_type,
            "transitEncryptionEnabled": false,
            "atRestEncryptionEnabled": true,
            "healthMetricsConfig": self.health,
        })
    }

    /// An `UpdateApiCache` body with the three members the update requires.
    fn update_body(&self) -> Value {
        json!({
            "ttl": self.ttl,
            "apiCachingBehavior": self.behavior,
            "type": self.cache_type,
        })
    }
}

/// Create the API's cache.
async fn create_cache(
    router: &MultiServiceRouter,
    api_id: &str,
    settings: &CacheSettings,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/ApiCaches"),
        Some(settings.create_body()),
    )
    .await
}

/// Read the API's cache back.
async fn get_cache(router: &MultiServiceRouter, api_id: &str) -> Answer {
    call(router, "GET", &format!("/v1/apis/{api_id}/ApiCaches"), None).await
}

/// Apply settings to the API's cache.
async fn update_cache(
    router: &MultiServiceRouter,
    api_id: &str,
    settings: &CacheSettings,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/ApiCaches/update"),
        Some(settings.update_body()),
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

/// Poll `GetApiCache` until it reports no instance at all, and answer that refusal — i.e.
/// until the removal the teardown started has SETTLED.
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

/// Create one API, give it a cache and wait for that instance to be available: the state a
/// teardown is served from.
async fn api_with_available_cache(
    router: &MultiServiceRouter,
    name: &str,
    settings: &CacheSettings,
    case: &str,
) -> String {
    let api_id = create_api(router, name).await;
    let created = create_cache(router, &api_id, settings).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected the seed create to be accepted, got {created:?}"
    );
    poll_until_status(router, &api_id, AVAILABLE, case).await;
    api_id
}

/// Fire `racers` teardowns at one API at the same time, so every one of them arrives while
/// the removal is still being carried out, and answer what each was served. The arities are
/// written out, so driving the race needs no crate of its own.
async fn teardowns_inside_the_window(
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
        4 => {
            let (first, second, third, fourth) = tokio::join!(
                delete_cache(router, api_id),
                delete_cache(router, api_id),
                delete_cache(router, api_id),
                delete_cache(router, api_id)
            );
            vec![first, second, third, fourth]
        }
        other => panic!("case {case:?}: this suite races 1, 2, 3 or 4 teardowns, not {other}"),
    }
}

/// Assert that the reader, the flush and the update all meet this API's absent cache with
/// `expected`'s sentence for their own verb, each refused 404 `NotFoundException`.
async fn assert_absence_is_reported(
    router: &MultiServiceRouter,
    api_id: &str,
    settings: &CacheSettings,
    expected: &Sentences,
    case: &str,
) {
    for (operation, answer, sentence) in [
        ("GetApiCache", get_cache(router, api_id).await, expected.get),
        (
            "FlushApiCache",
            flush_cache(router, api_id).await,
            expected.flush,
        ),
        (
            "UpdateApiCache",
            update_cache(router, api_id, settings).await,
            expected.update,
        ),
    ] {
        assert_eq!(
            (
                answer.status,
                answer.error_type.as_str(),
                answer.message.as_str()
            ),
            (404, "NotFoundException", sentence),
            "case {case:?} {operation}: expected 404 NotFoundException {sentence:?}, \
             got {answer:?}"
        );
    }
}

/// Which absent-cache sentence an API answers follows the removal window, not the number of
/// teardowns: teardowns that all arrive while the removal is in flight leave the API
/// reporting a cache that HAS BEEN DELETED, and only the teardown that arrives once that
/// removal has settled returns the API to the never-configured state — after which the three
/// other cache calls report that state in their own verbs, the next teardown has nothing left
/// to remove and is refused, and a create still fills the slot again.
///
/// The rows differ along how many teardowns arrive inside the removal window and along the
/// settings of the cache they tear down, none of which the rule reads.
#[tokio::test]
async fn the_redundant_teardown_returns_the_api_to_never_configured_only_once_the_removal_settled()
{
    let router = router();

    // (case, how many teardowns arrive inside the removal window, ttl)
    let rows: &[(&str, usize, i64)] = &[
        ("one teardown, then the sequential redundant one", 1, 300),
        ("two teardowns inside the removal window", 2, 1),
        ("three teardowns inside the removal window", 3, 3_600),
        ("four teardowns inside the removal window", 4, 42),
    ];

    for (index, (case, racers, ttl)) in rows.iter().enumerate() {
        let settings = CacheSettings::for_row(index, *ttl);
        let case = format!("{case} ({settings:?})");
        let api_id = api_with_available_cache(
            &router,
            &format!("Api_cache_window_{index}"),
            &settings,
            &case,
        )
        .await;

        // Every teardown that arrives inside the window finds the instance and is served.
        for (racer, answer) in teardowns_inside_the_window(&router, &api_id, *racers, &case)
            .await
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                answer.status, 200,
                "case {case:?} teardown {racer} inside the window: expected status 200, \
                 got {answer:?}"
            );
            assert!(
                answer.error_type.is_empty(),
                "case {case:?} teardown {racer} inside the window: expected no error shape, \
                 got {answer:?}"
            );
            assert!(
                carries_no_members(&answer.body),
                "case {case:?} teardown {racer} inside the window: expected a response with no \
                 members, got {answer:?}"
            );
        }

        // None of them completed a deletion: the instance is still on its way out, and once
        // the clock has removed it the API reports the cache it HAD and lost.
        let gone = poll_until_no_instance(&router, &api_id, &case).await;
        assert_eq!(
            (gone.status, gone.error_type.as_str(), gone.message.as_str()),
            (404, "NotFoundException", DELETED.get),
            "case {case:?}: expected 404 NotFoundException {:?} once the removal settled, \
             got {gone:?}",
            DELETED.get
        );
        assert_absence_is_reported(
            &router,
            &api_id,
            &settings,
            &DELETED,
            &format!("{case} while the deletion stands"),
        )
        .await;

        // The teardown that arrives now, once the slot is empty and settled, has that
        // deletion to complete: it is served the modelled empty response, and it is what
        // returns the API to holding no cache at all.
        let completing = delete_cache(&router, &api_id).await;
        assert_eq!(
            completing.status, 200,
            "case {case:?} sequential redundant teardown: expected status 200, got {completing:?}"
        );
        assert!(
            completing.error_type.is_empty(),
            "case {case:?} sequential redundant teardown: expected no error shape, \
             got {completing:?}"
        );
        assert!(
            carries_no_members(&completing.body),
            "case {case:?} sequential redundant teardown: expected a response with no members, \
             got {completing:?}"
        );

        // So every call that addresses the cache now reports the never-configured absence in
        // its own verb, and keeps reporting it: those are reads, and the refusals write
        // nothing.
        for round in 1..=2 {
            assert_absence_is_reported(
                &router,
                &api_id,
                &settings,
                &ABSENT,
                &format!("{case} after the sequential redundant teardown, round {round}"),
            )
            .await;
        }

        // And the teardown after it has neither an instance to remove nor a deletion to
        // complete, so it is refused with the never-configured sentence in its own verb.
        for attempt in 1..=2 {
            let refused = delete_cache(&router, &api_id).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_type.as_str(),
                    refused.message.as_str()
                ),
                (404, "NotFoundException", ABSENT_DELETE),
                "case {case:?} teardown {attempt} with nothing left to remove: expected 404 \
                 NotFoundException {ABSENT_DELETE:?}, got {refused:?}"
            );
        }

        // Nothing along the way left the API unable to cache: a create fills the slot again
        // and the new instance starts its lifecycle over.
        let recreated = create_cache(&router, &api_id, &settings).await;
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
        let available = poll_until_status(&router, &api_id, AVAILABLE, &case).await;
        assert_eq!(
            available.body["apiCache"]["ttl"].as_i64(),
            Some(settings.ttl),
            "case {case:?}: expected the new instance to report its own ttl, got {available:?}"
        );
        // And the lifecycle runs from the start on that new instance: the teardown finds it
        // and takes it down over the same window.
        let torn_down = delete_cache(&router, &api_id).await;
        assert_eq!(
            torn_down.status, 200,
            "case {case:?}: expected the teardown to find the new instance, got {torn_down:?}"
        );
        let going = get_cache(&router, &api_id).await;
        assert_eq!(
            (going.status, going.body["apiCache"]["status"].as_str()),
            (200, Some(DELETING)),
            "case {case:?}: expected the new instance to be reported as being removed, \
             got {going:?}"
        );
    }
}

/// The state the redundant teardown returns an API to is the SAME state an API that was
/// never given a cache is in: both answer the never-configured sentence, with the same status
/// and error shape, on every call that addresses the cache — which is what makes the
/// distinction from a standing deletion the only signal of the difference.
///
/// The rows differ along how the API came to hold no cache: never created, created and torn
/// down with the deletion then completed, and the same again after the slot was filled and
/// emptied a second time.
#[tokio::test]
async fn an_api_whose_deletion_was_completed_answers_exactly_as_one_that_never_had_a_cache() {
    let router = router();

    // (case, how many create-then-tear-down rounds run before the four calls, ttl)
    let rows: &[(&str, usize, i64)] = &[
        ("an api that never had a cache", 0, 60),
        ("an api whose cache was created and torn down once", 1, 600),
        ("an api whose cache was created and torn down twice", 2, 3_600),
    ];

    let mut answers: Vec<(String, [String; 4])> = Vec::new();
    for (index, (case, rounds, ttl)) in rows.iter().enumerate() {
        let settings = CacheSettings::for_row(index, *ttl);
        let case = format!("{case} ({settings:?})");
        let api_id = create_api(&router, &format!("Api_cache_absence_{index}")).await;

        for round in 1..=*rounds {
            let created = create_cache(&router, &api_id, &settings).await;
            assert_eq!(
                created.status, 200,
                "case {case:?} round {round}: expected the create to be accepted, got {created:?}"
            );
            poll_until_status(&router, &api_id, AVAILABLE, &case).await;
            let torn_down = delete_cache(&router, &api_id).await;
            assert_eq!(
                torn_down.status, 200,
                "case {case:?} round {round}: expected the teardown to be served, \
                 got {torn_down:?}"
            );
            poll_until_no_instance(&router, &api_id, &case).await;
            // The redundant teardown that completes the deletion: served, and it is what
            // returns the API to the never-configured state.
            let completing = delete_cache(&router, &api_id).await;
            assert_eq!(
                completing.status, 200,
                "case {case:?} round {round}: expected the redundant teardown to be served, \
                 got {completing:?}"
            );
        }

        let read = get_cache(&router, &api_id).await;
        let flushed = flush_cache(&router, &api_id).await;
        let updated = update_cache(&router, &api_id, &settings).await;
        let refused = delete_cache(&router, &api_id).await;
        for (operation, answer, sentence) in [
            ("GetApiCache", &read, ABSENT.get),
            ("FlushApiCache", &flushed, ABSENT.flush),
            ("UpdateApiCache", &updated, ABSENT.update),
            ("DeleteApiCache", &refused, ABSENT_DELETE),
        ] {
            assert_eq!(
                (
                    answer.status,
                    answer.error_type.as_str(),
                    answer.message.as_str()
                ),
                (404, "NotFoundException", sentence),
                "case {case:?} {operation}: expected 404 NotFoundException {sentence:?}, \
                 got {answer:?}"
            );
        }
        answers.push((
            case,
            [
                read.message,
                flushed.message,
                updated.message,
                refused.message,
            ],
        ));
    }

    // And every row answered the same four sentences, whichever way it came to hold no
    // cache.
    let (first_case, first) = &answers[0];
    for (case, messages) in &answers[1..] {
        assert_eq!(
            messages, first,
            "case {case:?}: expected the same answers as {first_case:?}, got {messages:?}"
        );
    }
}
