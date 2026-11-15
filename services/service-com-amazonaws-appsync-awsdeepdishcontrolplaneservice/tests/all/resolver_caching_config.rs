// SPDX-License-Identifier: CC0-1.0

//! The TTL a cached resolver may be written with, end to end over HTTP, and what a
//! refused caching configuration leaves on the field it named.
//!
//! Two rules are asserted here, each over a TABLE that spans the class it generalises
//! over rather than the one request that exposed the defect:
//!
//! 1. a cached resolver's TTL lies inside the documented cache-entry window — "Valid
//!    values are 1–3,600 seconds" (`CachingConfig$ttl`) — so a configuration inside it
//!    is served 200 and round-trips through `GetResolver`, and one outside it is refused
//!    400 `BadRequestException` with AppSync's sentence for that window, the one the API
//!    cache's own TTL is refused with; the rule holds on BOTH calls that write a
//!    resolver's caching configuration;
//! 2. a resolver exists only where one was written: a refused create leaves the field it
//!    named carrying no resolver, so `GetResolver` answers 404 `NotFoundException` with
//!    AppSync's fixed no-resolver text — before the create, after a refused create, and
//!    after the resolver is deleted — while a refused update leaves the resolver already
//!    there exactly as it was.
//!
//! The rows differ along what the rule generalises over: where the TTL sits relative to
//! each published bound of the window (on it, one second past it, far past it, and
//! negative) and whether the configuration carries caching keys at all.
//!
//! The row type and the request helpers below are this suite's and the sibling
//! caching-keys suite's, which states the rule over the other member of the same
//! configuration. Every assertion goes through the router the production emulator
//! mounts; the API harness (`router`, `call`, the API with its data source, and the
//! create/read helpers) is the resolver suites' own, reused rather than copied.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, router, Answer};
use super::resolvers::{
    api_with_dependencies, create_resolver_body, get_resolver, DATA_SOURCE, TYPE,
};

/// AppSync's sentence for a cache-entry TTL outside the documented window.
///
/// Written out here rather than read off the service's own constant, so a change of
/// wording is caught rather than followed. The same holds for the one below.
const INVALID_TTL: &str =
    "TTL value cannot be smaller than 1 second and larger than 3600 seconds.";

/// AppSync's fixed text for a field that carries no resolver.
pub(crate) const NO_RESOLVER: &str = "No resolver found.";

/// The published bounds of the documented cache-entry window: "Valid values are 1–3,600
/// seconds" (`CachingConfig$ttl`). Source: `smithy_query(query="shape",
/// shape="CachingConfig")`.
const TTL_MIN: i64 = 1;

/// See [`TTL_MIN`].
const TTL_MAX: i64 = 3_600;

/// The mapping templates every row carries, so no row's refusal can come from a template
/// it left out.
const REQUEST_TEMPLATE: &str = "{\"version\": \"2018-05-29\"}";

/// See [`REQUEST_TEMPLATE`].
const RESPONSE_TEMPLATE: &str = "$util.toJson($ctx.result)";

/// One row of a caching table: the configuration a request asks for, and the sentence
/// AppSync refuses it with, where it refuses it at all.
#[derive(Debug)]
pub(crate) struct Row {
    /// What the row exercises, named in every assertion of it.
    pub(crate) label: &'static str,
    /// The `cachingConfig.ttl` the request carries.
    pub(crate) ttl: i64,
    /// The `cachingConfig.cachingKeys` the request carries, or `None` where it carries
    /// the member not at all.
    pub(crate) keys: Option<&'static [&'static str]>,
    /// The refusal text the configuration is owed, or `None` where AppSync serves it.
    pub(crate) refusal: Option<&'static str>,
}

/// The TTL table, over the window the rule reads. Every row's keys are rooted in a
/// context map, so a row's outcome can only come from its TTL.
const ROWS: &[Row] = &[
    Row {
        label: "a TTL at the short end of the window",
        ttl: TTL_MIN,
        keys: Some(&["$context.arguments.id"]),
        refusal: None,
    },
    Row {
        label: "a TTL inside the window",
        ttl: 60,
        keys: Some(&["$context.arguments.id", "$context.identity.sub"]),
        refusal: None,
    },
    Row {
        label: "a TTL at the long end of the window",
        ttl: TTL_MAX,
        keys: None,
        refusal: None,
    },
    Row {
        label: "a TTL one second under the window",
        ttl: TTL_MIN - 1,
        keys: Some(&["$context.arguments.id"]),
        refusal: Some(INVALID_TTL),
    },
    Row {
        label: "a TTL one second over the window",
        ttl: TTL_MAX + 1,
        keys: Some(&["$context.arguments.id"]),
        refusal: Some(INVALID_TTL),
    },
    Row {
        label: "a TTL a day long",
        ttl: 86_400,
        keys: None,
        refusal: Some(INVALID_TTL),
    },
    Row {
        label: "a negative TTL",
        ttl: -1,
        keys: Some(&[]),
        refusal: Some(INVALID_TTL),
    },
    Row {
        label: "a TTL outside the window and a key outside the context maps",
        ttl: 0,
        keys: Some(&["$context.nonsense.id"]),
        // The TTL every cached resolver carries is the member named first.
        refusal: Some(INVALID_TTL),
    },
];

/// The row a resolver an update addresses is created from: one AppSync serves, cached by
/// a key inside the maps at a TTL inside the window.
pub(crate) const SETTLED: Row = Row {
    label: "the resolver an update addresses",
    ttl: 60,
    keys: Some(&["$context.arguments.id"]),
    refusal: None,
};

/// The `cachingConfig` member a row asks for.
pub(crate) fn caching_config(row: &Row) -> Value {
    match row.keys {
        Some(keys) => json!({"ttl": row.ttl, "cachingKeys": keys}),
        None => json!({"ttl": row.ttl}),
    }
}

/// A UNIT resolver on `field`, cached as `row` asks.
pub(crate) fn resolver_body(field: &str, row: &Row) -> Value {
    json!({
        "fieldName": field,
        "dataSourceName": DATA_SOURCE,
        "requestMappingTemplate": REQUEST_TEMPLATE,
        "responseMappingTemplate": RESPONSE_TEMPLATE,
        "cachingConfig": caching_config(row),
    })
}

/// Send one `UpdateResolver` restating the resolver on `field` as `row` asks.
pub(crate) async fn update_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    row: &Row,
) -> Answer {
    let mut body = resolver_body(field, row);
    body.as_object_mut()
        .expect("the request body is a JSON object")
        .remove("fieldName");
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
        Some(body),
    )
    .await
}

/// Assert `answer` is the refusal `message` states.
pub(crate) fn assert_refused(case: &str, answer: &Answer, message: &str) {
    assert_eq!(
        (
            answer.status,
            answer.error_type.as_str(),
            answer.message.as_str()
        ),
        (400, "BadRequestException", message),
        "case {case}: expected 400 BadRequestException {message:?}, got {answer:?}"
    );
}

/// Assert `answer` is the 404 a field carrying no resolver is owed.
pub(crate) fn assert_no_resolver(case: &str, answer: &Answer) {
    assert_eq!(
        (
            answer.status,
            answer.error_type.as_str(),
            answer.message.as_str()
        ),
        (404, "NotFoundException", NO_RESOLVER),
        "case {case}: expected 404 NotFoundException {NO_RESOLVER:?}, got {answer:?}"
    );
}

/// Walk `rows` through `CreateResolver`: a served row is read back through `GetResolver`,
/// and a refused one leaves the field it named carrying no resolver, before and after.
pub(crate) async fn assert_creates_follow_the_rule(
    router: &MultiServiceRouter,
    api_id: &str,
    rows: &[Row],
) {
    for (index, row) in rows.iter().enumerate() {
        let field = format!("created_{index}");

        // Nothing is attached yet, so the field carries no resolver whatever the row
        // asks for.
        let before = get_resolver(router, api_id, TYPE, &field).await;
        assert_no_resolver(&format!("before create {}", row.label), &before);

        let created = create_resolver_body(router, api_id, TYPE, resolver_body(&field, row)).await;

        if let Some(message) = row.refusal {
            assert_refused(&format!("create {}", row.label), &created, message);

            // The refusal wrote nothing, so the field it named still carries no
            // resolver — the existence a later reader sees is the one the writes left.
            let read = get_resolver(router, api_id, TYPE, &field).await;
            assert_no_resolver(&format!("after refused create {}", row.label), &read);
            continue;
        }

        assert_eq!(
            created.status, 200,
            "case create {}: expected 200, got {created:?}",
            row.label
        );
        assert_eq!(
            created.body["resolver"]["cachingConfig"],
            caching_config(row),
            "case create {}: expected the caching configuration the request set, got {:?}",
            row.label,
            created.body["resolver"]
        );

        // The stored record an independent read answers is that same body, keys and all.
        let read = get_resolver(router, api_id, TYPE, &field).await;
        assert_eq!(
            read.status, 200,
            "case read back {}: expected 200, got {read:?}",
            row.label
        );
        assert_eq!(
            read.body["resolver"], created.body["resolver"],
            "case read back {}: expected the stored record {:?}, got {:?}",
            row.label, created.body["resolver"], read.body["resolver"]
        );
    }
}

/// Walk `rows` through `UpdateResolver`, each against a resolver a create attached: a
/// served row is read back, and a refused one leaves the stored resolver untouched.
pub(crate) async fn assert_updates_follow_the_rule(
    router: &MultiServiceRouter,
    api_id: &str,
    rows: &[Row],
) {
    for (index, row) in rows.iter().enumerate() {
        let field = format!("updated_{index}");
        let created =
            create_resolver_body(router, api_id, TYPE, resolver_body(&field, &SETTLED)).await;
        assert_eq!(
            created.status, 200,
            "case update {}: expected the resolver to attach first, got {created:?}",
            row.label
        );
        let stored = created.body["resolver"].clone();

        let updated = update_resolver(router, api_id, &field, row).await;

        if let Some(message) = row.refusal {
            assert_refused(&format!("update {}", row.label), &updated, message);
            let read = get_resolver(router, api_id, TYPE, &field).await;
            assert_eq!(
                read.body["resolver"], stored,
                "case update {}: expected the stored resolver untouched {stored:?}, got {:?}",
                row.label, read.body["resolver"]
            );
            continue;
        }

        assert_eq!(
            updated.status, 200,
            "case update {}: expected 200, got {updated:?}",
            row.label
        );
        let read = get_resolver(router, api_id, TYPE, &field).await;
        assert_eq!(
            read.body["resolver"]["cachingConfig"],
            caching_config(row),
            "case update {}: expected the caching configuration the update set, got {:?}",
            row.label,
            read.body["resolver"]
        );
    }
}

#[tokio::test]
async fn a_create_writes_a_cached_resolver_only_inside_the_documented_ttl_window() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_caching_ttl_create").await;
    assert_creates_follow_the_rule(&router, &api_id, ROWS).await;
}

#[tokio::test]
async fn an_update_is_judged_by_the_same_window_and_a_refusal_keeps_the_stored_cache() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_caching_ttl_update").await;
    assert_updates_follow_the_rule(&router, &api_id, ROWS).await;
}

#[tokio::test]
async fn a_deleted_resolver_is_no_longer_served() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_caching_ttl_delete").await;

    // Each served row's resolver is attached, read back, deleted and read again, so the
    // existence a read reports follows the writes rather than the request's shape.
    for (index, row) in ROWS.iter().filter(|row| row.refusal.is_none()).enumerate() {
        let field = format!("deleted_{index}");
        let created =
            create_resolver_body(&router, &api_id, TYPE, resolver_body(&field, row)).await;
        assert_eq!(
            created.status, 200,
            "case delete {}: expected the resolver to attach first, got {created:?}",
            row.label
        );

        let read = get_resolver(&router, &api_id, TYPE, &field).await;
        assert_eq!(
            read.status, 200,
            "case delete {}: expected the attached resolver to be served, got {read:?}",
            row.label
        );

        let deleted = call(
            &router,
            "DELETE",
            &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case delete {}: expected DeleteResolver 200, got {deleted:?}",
            row.label
        );

        let after = get_resolver(&router, &api_id, TYPE, &field).await;
        assert_no_resolver(&format!("after delete {}", row.label), &after);
    }
}
