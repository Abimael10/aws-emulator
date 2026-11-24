// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH asynchronous-invocation configuration decides how many times a
//! failing asynchronous event reaches the handler.
//!
//! RULE: a failing asynchronous event enters the handler `1 + MaximumRetryAttempts` times —
//! "the maximum number of times to retry when the function returns an error"
//! (`FunctionEventInvokeConfig$MaximumRetryAttempts`, `@range(min: 0, max: 2)`, so the member
//! counts the attempts AFTER the first) — with that budget read out of the configuration stored
//! under the QUALIFIER the event was ADDRESSED to. `PutFunctionEventInvokeConfig` "configures
//! options for asynchronous invocation on a function, version, or alias" and takes its
//! `Qualifier` as "a version number or alias name", so each qualifier holds a configuration of
//! its own: a qualifier inherits no budget from another, and one holding no budget at all has
//! the documented default of two retries ("if you invoke a function asynchronously and it
//! returns an error, Lambda executes the function up to two more times"). The last attempt of
//! the ladder then earns exactly one invocation record at the destination
//! `DestinationConfig$OnFailure` names, reporting `RetriesExhausted` as
//! `requestContext.condition` and the entries as `requestContext.approximateInvokeCount`.
//!
//! [`crate::async_retry_budget_attempts`] pins the LENGTH of that ladder for every budget a put
//! can state on the unpublished version, and
//! [`crate::async_retry_budget_from_stored_config`] pins it for a budget an update stated. The
//! rows below differ along what this reading of the rule generalises over: the qualifier the
//! event is addressed to (none at all, a published version, an alias) and the budget that
//! qualifier holds (each end of the published range, the value between them, and no budget at
//! all) — with every OTHER qualifier of the same function holding a budget that would make a
//! DIFFERENT number of attempts, so an emulator that fell back to the unpublished version's
//! budget, or that resolved an alias to its target version's, fails here rather than passing by
//! coincidence.
//!
//! The wiring every row needs — the queues, the function whose package RUNS a handler that
//! counts its own entries and raises, the accepting call — and the one reader of what a ladder
//! leaves behind are [`crate::async_retry_budget_attempts`]'s, so the suites that drive a
//! ladder of attempts share one harness. Nothing here waits a span of its own: every wait looks
//! again until what it waits for is there or a generous deadline passes.
//!
//! Which qualifier's configuration names the DESTINATION a record reaches is
//! `async_destination_qualifier_scope.rs`'s subject; this suite is about the budget the same
//! stored configuration carries.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::async_retry_budget_attempts::{
    assert_each_ladder, given_function, given_queue, lambda, router_on, Ladder,
    EVENT_INVOKE_CONFIG_URI,
};

/// Where `PublishVersion`, `CreateAlias` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The request header `Invoke$InvocationType` is bound to, and the value that queues the event
/// instead of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// The version every row of the table publishes, and the one its alias points at.
const FIRST_VERSION: &str = "1";

/// The alias every row creates, inside `Alias`'s own pattern
/// (`^(?!^[0-9]+$)([a-zA-Z0-9-_]+)$`).
const ALIAS: &str = "live";

/// One of the three qualifiers an event of this suite can be addressed to, and a configuration
/// can be held against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addressed {
    /// The unpublished version, addressed by an invocation that names no qualifier at all.
    Unqualified,
    /// The one version every row publishes.
    Version,
    /// The alias pointing at that version.
    Alias,
}

impl Addressed {
    /// The `Qualifier` a request carries to name this one, and `None` for the unqualified
    /// form — which is the request that addresses the unpublished version.
    fn qualifier(self) -> Option<&'static str> {
        match self {
            Self::Unqualified => None,
            Self::Version => Some(FIRST_VERSION),
            Self::Alias => Some(ALIAS),
        }
    }
}

/// Every qualifier a row configures or addresses.
const QUALIFIERS: [Addressed; 3] = [
    Addressed::Unqualified,
    Addressed::Version,
    Addressed::Alias,
];

/// `PublishVersion`, which freezes the function's code and configuration as a version of its
/// own.
async fn published_version(router: &MultiServiceRouter, case: &str, function: &str) -> String {
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/versions"),
        &json!({ "Description": "the version an alias points at" }),
        &[],
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: PublishVersion answered {status} {answered}"
    );
    let published: Value = serde_json::from_str(&answered)
        .unwrap_or_else(|_| panic!("case {case:?}: PublishVersion answered {answered}"));
    published["Version"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Version in {answered}"))
        .to_string()
}

/// `CreateAlias` naming `version`.
async fn given_alias(router: &MultiServiceRouter, case: &str, function: &str, version: &str) {
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/aliases"),
        &json!({ "Name": ALIAS, "FunctionVersion": version }),
        &[],
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateAlias {ALIAS:?} answered {status} {answered}"
    );
}

/// The `event-invoke-config` URI of the qualifier `qualifier` names, or of the unpublished
/// version when it names none.
fn config_uri(function: &str, qualifier: Option<&str>) -> String {
    match qualifier {
        Some(qualifier) => format!(
            "{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config?Qualifier={qualifier}"
        ),
        None => format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config"),
    }
}

/// `PutFunctionEventInvokeConfig` on that qualifier: the budget it holds, when the row states
/// one, and the queue the records of its failed invocations go to, when the qualifier is the
/// one the row addresses.
///
/// A put that states no `MaximumRetryAttempts` leaves the qualifier holding none, which is the
/// configuration the documented default of two retries applies to.
async fn put_budget(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    qualifier: Option<&str>,
    retries: Option<i64>,
    on_failure: Option<&str>,
) {
    let mut body = json!({});
    if let Some(retries) = retries {
        body["MaximumRetryAttempts"] = json!(retries);
    }
    if let Some(destination) = on_failure {
        body["DestinationConfig"] = json!({ "OnFailure": { "Destination": destination } });
    }
    let (status, answered) =
        lambda(router, "PUT", &config_uri(function, qualifier), &body, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutFunctionEventInvokeConfig on {qualifier:?} answered {status} \
         {answered}"
    );
    // The budget the ladder is measured against is the one a client can READ back, so the row
    // is pinned to the configuration rather than to the request that wrote it.
    let (status, held) = lambda(
        router,
        "GET",
        &config_uri(function, qualifier),
        &Value::Null,
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetFunctionEventInvokeConfig on {qualifier:?} answered {status} {held}"
    );
    let reported: Value = serde_json::from_str(&held)
        .unwrap_or_else(|_| panic!("case {case:?}: GetFunctionEventInvokeConfig answered {held}"));
    let read = reported.get("MaximumRetryAttempts").and_then(Value::as_i64);
    assert_eq!(
        read, retries,
        "case {case:?}: expected the configuration of {qualifier:?} to report \
         MaximumRetryAttempts {retries:?}, got {read:?} in {held}"
    );
}

/// Queue one event for the qualifier `qualifier` names through an asynchronous `Invoke`, which
/// the accepting call answers `202` for.
async fn invoke_async(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    qualifier: Option<&str>,
    event: &Value,
) {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations{query}"),
        event,
        &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: an asynchronous Invoke of {qualifier:?} answered {status} {answered}"
    );
}

/// One row: the qualifier the event is addressed to, the budget that qualifier holds, the
/// budget every OTHER qualifier of the same function holds, and how many times the handler
/// therefore ENTERS for one event.
struct Row {
    case: &'static str,
    /// The function the row drives, which carries its own two queues.
    function: &'static str,
    /// The qualifier the accepting call addresses.
    addressed: Addressed,
    /// The `MaximumRetryAttempts` the addressed qualifier's configuration states, or `None`
    /// for a configuration that states none at all.
    retries: Option<i64>,
    /// The `MaximumRetryAttempts` every other qualifier of the function states: a value that
    /// would make a DIFFERENT number of attempts, so a budget read off the wrong qualifier is
    /// caught.
    other_retries: i64,
    /// The entries the handler makes for the event: the first invocation plus the retries the
    /// addressed qualifier allows, which is what the record reports as
    /// `approximateInvokeCount`.
    attempts: usize,
}

/// The rows: each form a qualifier takes, against each end of the published range, the value
/// between them, and a configuration stating no budget at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no qualifier, whose configuration states the published maximum",
            function: "budget-scope-latest",
            addressed: Addressed::Unqualified,
            retries: Some(2),
            other_retries: 0,
            attempts: 3,
        },
        Row {
            case: "a published version, whose configuration states the published minimum",
            function: "budget-scope-version",
            addressed: Addressed::Version,
            retries: Some(0),
            other_retries: 2,
            attempts: 1,
        },
        Row {
            case: "an alias, whose configuration states one retry",
            function: "budget-scope-alias",
            addressed: Addressed::Alias,
            retries: Some(1),
            other_retries: 2,
            attempts: 2,
        },
        Row {
            case: "an alias, whose configuration states no budget at all",
            function: "budget-scope-alias-unstated",
            addressed: Addressed::Alias,
            retries: None,
            other_retries: 0,
            attempts: 3,
        },
    ]
}

/// RULE: a failing asynchronous event enters the handler `1 + MaximumRetryAttempts` times with
/// the budget the ADDRESSED qualifier's own configuration holds — the documented two retries
/// where that configuration states none — whatever budget the function's other qualifiers hold,
/// and the last of those attempts earns exactly one `RetriesExhausted` record reporting them.
#[tokio::test]
async fn a_failing_event_spends_the_budget_of_the_qualifier_it_was_addressed_to() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows = rows();

    // Every row is wired and its event queued first, so the ladders of attempts run alongside
    // one another rather than one suite-length wait per row.
    let mut wired: Vec<Ladder> = Vec::new();
    for row in &rows {
        let case = row.case;
        let (attempts_url, _) =
            given_queue(&router, case, &format!("{}-attempts", row.function)).await;
        let (failure_url, failure_arn) =
            given_queue(&router, case, &format!("{}-failure", row.function)).await;
        given_function(&router, &engine, case, row.function).await;
        let published = published_version(&router, case, row.function).await;
        assert_eq!(
            published, FIRST_VERSION,
            "case {case:?}: expected the first publish to mint version {FIRST_VERSION}, got \
             {published:?}"
        );
        given_alias(&router, case, row.function, FIRST_VERSION).await;
        for qualifier in QUALIFIERS {
            if qualifier == row.addressed {
                // The addressed qualifier's own configuration: the budget the row states, and
                // the destination the record of its spent ladder is owed to.
                put_budget(
                    &router,
                    case,
                    row.function,
                    qualifier.qualifier(),
                    row.retries,
                    Some(&failure_arn),
                )
                .await;
            } else {
                // Another qualifier of the same function, holding a budget that would make a
                // different number of attempts and no destination at all.
                put_budget(
                    &router,
                    case,
                    row.function,
                    qualifier.qualifier(),
                    Some(row.other_retries),
                    None,
                )
                .await;
            }
        }
        let marker = format!("{}-marker", row.function);
        invoke_async(
            &router,
            case,
            row.function,
            row.addressed.qualifier(),
            &json!({ "marker": marker, "attempts": attempts_url }),
        )
        .await;
        wired.push(Ladder {
            case,
            attempts_url,
            failure_url,
            marker,
            attempts: row.attempts,
        });
    }

    assert_each_ladder(&router, &wired).await;
}
