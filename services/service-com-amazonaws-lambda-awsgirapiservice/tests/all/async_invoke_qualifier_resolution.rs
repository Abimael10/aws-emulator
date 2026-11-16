// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the QUALIFIER an invocation addresses, on the asynchronous invoke
//! path: `Invoke` (`POST /2015-03-31/functions/{FunctionName}/invocations`, with
//! `X-Amz-Invocation-Type`) and `InvokeAsync`
//! (`POST /2014-11-13/functions/{FunctionName}/invoke-async`).
//!
//! The rule under test: **an invocation resolves its `FunctionName`/`Qualifier` pair to a
//! function version BEFORE the invocation is admitted, so a pair naming a version or alias
//! the service does not hold is answered with the modelled `ResourceNotFoundException`
//! (HTTP 404) whose message is the fixed `Function not found: ` followed by the ARN the
//! request resolved — the named function in the Region of the endpoint, qualified by the
//! qualifier the request named — whatever the `InvocationType` is.** `Qualifier` "specif[ies]
//! a version or alias to invoke a published version of the function" and the answer to an
//! accepted asynchronous invocation "only includes a status code", so the resolution cannot
//! be deferred past the answer: an asynchronous invoke naming a qualifier that resolves to
//! nothing is REFUSED rather than accepted with 202, and its function is left holding no
//! event for it.
//!
//! The other half of the same rule is the admitted side: a pair that DOES resolve is
//! accepted, and the event is queued against the resource the call ADDRESSED — the qualifier
//! it named, which is what the asynchronous invocation configuration of that event is read
//! under (`FunctionEventInvokeConfig` is "for asynchronous invocation on a function, version,
//! or alias"), and the unpublished version for a call naming no qualifier.
//!
//! The rows differ along what the rule generalises over: which entry point accepted the
//! call and, for `Invoke`, which `InvocationType` it asked for (`Event`, `RequestResponse`,
//! `DryRun`); WHAT is missing (a version the function never published, an alias it does not
//! have, a version of a function that does not exist at all); WHERE the qualifier travels
//! (the `Qualifier` query parameter, the `name:qualifier` form of `FunctionName`); and which
//! form the qualifier takes (a published version number, an alias name).
//!
//! Nothing here waits on a run or on a fixed span: the refusal and the queueing both happen
//! on the request path, and the state the admitted side reads — the qualifier an entry was
//! addressed to and the event it holds — is written before the accepting call is answered, so
//! no row depends on how fast the host runs. That an accepted event is then DELIVERED to the
//! function, over every form of event, package and runtime, is `asynchronous_invoke_run.rs`'s
//! subject, and this suite leaves the run capacity that costs to it.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The queue entries one function holds, read out of the service's own state because no
// modelled Lambda operation reports an accepted asynchronous invocation back out: the reader
// the sibling asynchronous-invoke suite publishes, rather than a second copy of it.
use crate::asynchronous_invoke_run::held as held_events;
use crate::function_lifecycle::{
    create_active_function, function_arn, json_call, make_update_due, package_bytes, router_on,
    send, Answer,
};

/// Where `CreateFunction` and the invoke path are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `InvokeAsync` is bound.
const INVOKE_ASYNC_URI: &str = "/2014-11-13/functions";

/// The header `Invoke$InvocationType` is bound to
/// (`smithy_query(query="operation", operation="Invoke")`: `X-Amz-Invocation-Type`).
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";

/// The whole `InvocationType` value set, from
/// `smithy_query(query="shape", shape="InvocationType")`: `Event`, `RequestResponse`,
/// `DryRun`. Every one of them resolves the same pair, so every one of them is driven.
const INVOCATION_TYPES: &[&str] = &["Event", "RequestResponse", "DryRun"];

/// The error code the model declares for the resource an invocation could not find
/// (`smithy_query(query="operation", operation="Invoke")`: `ResourceNotFoundException`,
/// HTTP 404).
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The fixed text of the sentence, which is the whole message bar the resource it names.
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The function this suite holds: one published version and one alias on it, so a row can
/// address a version and an alias it does NOT have.
const HELD: &str = "aiqr-held";

/// A name no function of this store is held under.
const ABSENT: &str = "aiqr-absent";

/// The alias the held function has, and the version it points at.
const HELD_ALIAS: &str = "live";
/// See [`HELD_ALIAS`].
const HELD_VERSION: &str = "1";

/// A version number no function of this suite ever published.
const MISSING_VERSION: &str = "9";

/// An alias name no function of this suite ever created.
const MISSING_ALIAS: &str = "staging";

/// The unpublished version, as this service spells it everywhere.
const LATEST: &str = "$LATEST";

/// `Invoke` on `reference`, with the `InvocationType` header and the optional `Qualifier`
/// query parameter.
async fn invoke(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
    invocation_type: &str,
    payload: &[u8],
) -> Answer {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    send(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{reference}/invocations{query}"),
        "application/octet-stream",
        payload.to_vec(),
        &[(INVOCATION_TYPE_HEADER, invocation_type.to_string())],
    )
    .await
}

/// `InvokeAsync` on `reference`, whose whole body is the `InvokeArgs` payload. It carries no
/// `Qualifier` of its own, so the only qualifier it can name is the one its `FunctionName`
/// carries.
async fn invoke_async(
    router: &MultiServiceRouter,
    reference: &str,
    payload: &[u8],
) -> Answer {
    send(
        router,
        "POST",
        &format!("{INVOKE_ASYNC_URI}/{reference}/invoke-async"),
        "application/octet-stream",
        payload.to_vec(),
        &[],
    )
    .await
}

/// The `message` of a wire error, under either casing the JSON envelope uses.
fn message(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_else(|| String::from_utf8_lossy(&answer.body).to_string())
}

/// The ARN the rule says a refusal names: the function's ARN in this endpoint's Region — built
/// by the harness every suite of this target builds one with — qualified by the qualifier the
/// request named.
fn addressed_arn(function: &str, qualifier: &str) -> String {
    format!("{}:{qualifier}", function_arn(function))
}

/// Create the function this suite holds, publish one version of it and give that version an
/// alias, so a row can name a version and an alias that resolve and ones that do not.
async fn create_held(router: &MultiServiceRouter, engine: &Arc<engine::InMemoryEngine>) {
    create_active_function(router, engine, HELD, &package_bytes(64)).await;
    let published = json_call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{HELD}/versions"),
        &json!({ "Description": "the version a qualifier resolves to" }),
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "PublishVersion on {HELD:?} must succeed, got {} {}",
        published.status,
        String::from_utf8_lossy(&published.body)
    );
    assert_eq!(
        published.string("Version"),
        Some(HELD_VERSION.to_string()),
        "PublishVersion must mint version {HELD_VERSION} of {HELD:?}, got {}",
        String::from_utf8_lossy(&published.body)
    );
    let aliased = json_call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{HELD}/aliases"),
        &json!({ "Name": HELD_ALIAS, "FunctionVersion": HELD_VERSION }),
    )
    .await;
    assert_eq!(
        aliased.status,
        201,
        "CreateAlias {HELD_ALIAS:?} on {HELD:?} must succeed, got {} {}",
        aliased.status,
        String::from_utf8_lossy(&aliased.body)
    );
    // The publish is a change to the function, which the model gives its own settle window
    // ("you can't modify the function" while an update is in progress); bringing the due
    // instants forward is what waiting would do, without waiting.
    make_update_due(engine);
}

/// One row of the refusal table.
struct Refused {
    /// What this row varies, for the assertion messages.
    label: &'static str,
    /// The `FunctionName` the request carries.
    reference: String,
    /// The `Qualifier` query parameter, when the row sends one.
    qualifier: Option<&'static str>,
    /// The function the reference names, which the resolved ARN is built from.
    function: &'static str,
    /// The qualifier the pair names, from the reference or from the parameter.
    named: &'static str,
}

/// Every row of the refusal table; see the module documentation for the dimensions.
fn refused_rows() -> Vec<Refused> {
    vec![
        Refused {
            label: "a version the held function never published, in the Qualifier parameter",
            reference: HELD.to_string(),
            qualifier: Some(MISSING_VERSION),
            function: HELD,
            named: MISSING_VERSION,
        },
        Refused {
            label: "a version the held function never published, inside the FunctionName",
            reference: format!("{HELD}:{MISSING_VERSION}"),
            qualifier: None,
            function: HELD,
            named: MISSING_VERSION,
        },
        Refused {
            label: "an alias the held function does not have, in the Qualifier parameter",
            reference: HELD.to_string(),
            qualifier: Some(MISSING_ALIAS),
            function: HELD,
            named: MISSING_ALIAS,
        },
        Refused {
            label: "an alias the held function does not have, inside the FunctionName",
            reference: format!("{HELD}:{MISSING_ALIAS}"),
            qualifier: None,
            function: HELD,
            named: MISSING_ALIAS,
        },
        Refused {
            label: "a version of a function that does not exist, in the Qualifier parameter",
            reference: ABSENT.to_string(),
            qualifier: Some(MISSING_VERSION),
            function: ABSENT,
            named: MISSING_VERSION,
        },
        Refused {
            label: "an alias of a function that does not exist, inside the FunctionName",
            reference: format!("{ABSENT}:{MISSING_ALIAS}"),
            qualifier: None,
            function: ABSENT,
            named: MISSING_ALIAS,
        },
    ]
}

/// The whole refusal half of the rule: every row, over every entry point that resolves a
/// qualifier, is answered 404 with the sentence naming the resolved ARN — and the function
/// the rows named is left holding no event.
#[tokio::test]
async fn an_unresolvable_qualifier_refuses_the_invocation_rather_than_accepting_it() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_held(&router, &engine).await;

    for row in refused_rows() {
        let expected = format!(
            "{NOT_FOUND_PREFIX}{}",
            addressed_arn(row.function, row.named)
        );
        // One call per `InvocationType`: the pair is resolved before the invocation is
        // admitted, so the asynchronous type is refused exactly as the synchronous one is.
        let mut answers: Vec<(String, Answer)> = Vec::new();
        for invocation_type in INVOCATION_TYPES {
            answers.push((
                format!("Invoke with InvocationType={invocation_type}"),
                invoke(
                    &router,
                    &row.reference,
                    row.qualifier,
                    invocation_type,
                    br#"{"event":"unresolved"}"#,
                )
                .await,
            ));
        }
        // `InvokeAsync` reads the qualifier out of the `FunctionName` alone, so it is driven
        // for the rows that carry it there.
        if row.qualifier.is_none() {
            answers.push((
                "InvokeAsync".to_string(),
                invoke_async(&router, &row.reference, br#"{"event":"unresolved"}"#).await,
            ));
        }

        for (operation, answer) in &answers {
            assert_eq!(
                answer.status,
                404,
                "case {:?} ({operation}): expected the modelled 404, got {} {}",
                row.label,
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert!(
                answer.error_code().contains(NOT_FOUND_CODE),
                "case {:?} ({operation}): expected {NOT_FOUND_CODE}, got {:?}",
                row.label,
                answer.error_code()
            );
            assert_eq!(
                message(answer),
                expected,
                "case {:?} ({operation}): expected {expected:?}, got {:?}",
                row.label,
                message(answer)
            );
        }
    }

    // A refused invocation was never admitted, so nothing was queued for the function the
    // rows addressed — the asynchronous answer promises the QUEUEING, and no refusal gave
    // one.
    let queued = held_events(&engine, HELD);
    assert!(
        queued.is_empty(),
        "a refused invocation must queue no event, got {:?}",
        queued
            .iter()
            .map(|entry| (entry.version.clone(), entry.outcome.clone()))
            .collect::<Vec<_>>()
    );

    // The control: the function the rows named still answers after every refusal, so no
    // refusal disturbed the store.
    let read = json_call(
        &router,
        "GET",
        &format!("{FUNCTIONS_URI}/{HELD}/configuration?Qualifier={HELD_VERSION}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        (read.status, read.string("Version")),
        (200, Some(HELD_VERSION.to_string())),
        "the held version must still be readable after the refusals, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
}

/// One row of the admitted table: how the accepted call addresses the function, and the
/// qualifier the queued event must therefore be held against.
struct Accepted {
    label: &'static str,
    reference: String,
    qualifier: Option<&'static str>,
    event: &'static str,
    addressed: &'static str,
}

/// Every row of the admitted table: the qualifier forms that DO resolve — the unpublished
/// version, a published version, an alias — over both places a qualifier travels.
fn accepted_rows() -> Vec<Accepted> {
    vec![
        Accepted {
            label: "no qualifier at all, which runs the unpublished version",
            reference: HELD.to_string(),
            qualifier: None,
            event: r#"{"event":"latest-implicit"}"#,
            addressed: LATEST,
        },
        Accepted {
            label: "the unpublished version named explicitly",
            reference: HELD.to_string(),
            qualifier: Some(LATEST),
            event: r#"{"event":"latest-explicit"}"#,
            addressed: LATEST,
        },
        Accepted {
            label: "a published version in the Qualifier parameter",
            reference: HELD.to_string(),
            qualifier: Some(HELD_VERSION),
            event: r#"{"event":"version-parameter"}"#,
            addressed: HELD_VERSION,
        },
        Accepted {
            label: "a published version inside the FunctionName",
            reference: format!("{HELD}:{HELD_VERSION}"),
            qualifier: None,
            event: r#"{"event":"version-in-name"}"#,
            addressed: HELD_VERSION,
        },
        Accepted {
            label: "an alias, which is a resource of its own",
            reference: HELD.to_string(),
            qualifier: Some(HELD_ALIAS),
            event: r#"{"event":"alias"}"#,
            addressed: HELD_ALIAS,
        },
    ]
}

/// The admitted half of the rule: a pair that resolves is accepted with 202 and the event is
/// queued against the qualifier the call addressed — so the resolution decides which resource
/// the accepted event belongs to, rather than being skipped.
#[tokio::test]
async fn an_accepted_async_invocation_is_queued_against_the_addressed_qualifier() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_held(&router, &engine).await;

    let rows = accepted_rows();
    for row in &rows {
        let accepted = invoke(
            &router,
            &row.reference,
            row.qualifier,
            "Event",
            row.event.as_bytes(),
        )
        .await;
        assert_eq!(
            accepted.status,
            202,
            "case {:?}: expected 202 Accepted, got {} {}",
            row.label,
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
        assert!(
            accepted.body.is_empty(),
            "case {:?}: an accepted asynchronous invocation answers an empty payload, got {:?}",
            row.label,
            String::from_utf8_lossy(&accepted.body)
        );
    }

    // The queue holds one entry per accepted call, in the order they were accepted, each
    // carrying its own event and the qualifier its call addressed.
    let queued = held_events(&engine, HELD);
    assert_eq!(
        queued.len(),
        rows.len(),
        "expected one queued event per accepted call, got {:?}",
        queued
            .iter()
            .map(|entry| (
                entry.version.clone(),
                String::from_utf8_lossy(&entry.payload).to_string()
            ))
            .collect::<Vec<_>>()
    );
    for (entry, row) in queued.iter().zip(&rows) {
        assert_eq!(
            String::from_utf8_lossy(&entry.payload),
            row.event,
            "case {:?}: expected the entry to hold its own event, got {:?}",
            row.label,
            String::from_utf8_lossy(&entry.payload)
        );
        assert_eq!(
            entry.version, row.addressed,
            "case {:?}: expected the event to be queued against {:?}, got {:?}",
            row.label, row.addressed, entry.version
        );
    }
}
