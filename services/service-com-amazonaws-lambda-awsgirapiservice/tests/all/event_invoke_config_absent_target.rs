// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how an operation on an ASYNCHRONOUS-INVOCATION CONFIGURATION
//! refuses a request whose `FunctionName`/`Qualifier` pair names no function, version or
//! alias: `PutFunctionEventInvokeConfig`, `GetFunctionEventInvokeConfig`,
//! `UpdateFunctionEventInvokeConfig`, `DeleteFunctionEventInvokeConfig` (all four bound at
//! `/2019-09-25/functions/{FunctionName}/event-invoke-config`) and
//! `ListFunctionEventInvokeConfigs` (`…/event-invoke-config/list`).
//!
//! The rule under test: **a request on one of these configurations whose
//! `FunctionName`/`Qualifier` pair names no function, version or alias is refused with the
//! modelled `ResourceNotFoundException` (HTTP 404) whose WHOLE message is the fixed sentence
//! `The function doesn't exist.`, carrying nothing of the request — neither the identifier
//! the caller sent nor the ARN the reference resolved to.** The sentence is the same however
//! the pair was spelled and whichever of the five operations was called, so a client that
//! branches on it recognises the refusal of any of them.
//!
//! The two refusals that are NOT "a resource this endpoint could not find" keep their own
//! sentences, which is what makes the rule above tellable apart from them: an identifier
//! naming a Region this endpoint does not serve states the two Regions that disagree (the
//! fact is settled by READING the identifier, before anything is looked up), and a function
//! whose addressed qualifier does exist but holds no configuration states that the function
//! has no `EventInvokeConfig` and names the ARN of the configuration asked about.
//!
//! The rows differ along what the rule generalises over: WHICH of the five operations the
//! request is (each row drives every one that can carry it); WHAT is missing (a function
//! nothing created, a version the function never published, an alias it does not have, a
//! version of a function that does not exist); WHICH documented `FunctionName` form named it
//! (bare name, the `function:` prefix form, the partial ARN, the full ARN of this endpoint's
//! Region); and WHERE the qualifier travels (the `Qualifier` query parameter, the
//! `name:qualifier` suffix of `FunctionName`).
//!
//! Nothing here waits on a clock: every answer is produced on the request path, and the
//! control rows read state written by the call before them, so no row depends on how fast
//! the host runs.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_invoke_config::{config_uri, configured_arn, publish_version};
use crate::event_source_mappings::{call, create_function, router_on, Answer};

/// Where `CreateFunction`, `PublishVersion` and `CreateAlias` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region the test harness's credential scope names, which is the one this endpoint
/// serves.
const REGION: &str = "us-east-1";

/// A Region this endpoint does NOT serve, for the cross-Region control.
const OTHER_REGION: &str = "us-west-2";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The wire error code of the modelled 404
/// (`smithy_query(query="operation", operation="UpdateFunctionEventInvokeConfig")`:
/// `ResourceNotFoundException`, httpError 404).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The whole sentence the rule requires for a pair naming no function, version or alias.
const FUNCTION_DOES_NOT_EXIST: &str = "The function doesn't exist.";

/// The function this suite HOLDS: one published version and one alias on it, so a row can
/// address a version and an alias it does not have.
const HELD: &str = "eica-held";

/// A name no function of this store is held under.
const ABSENT: &str = "eica-absent";

/// The alias the held function has, and the version that alias invokes.
const HELD_ALIAS: &str = "live";

/// A version number no function of this suite ever published.
const MISSING_VERSION: &str = "9";

/// An alias name no function of this suite ever created.
const MISSING_ALIAS: &str = "staging";

/// The body a write carries: one setting, so the refusal is about the RESOURCE the request
/// addressed rather than about anything the body says.
fn write_body() -> Value {
    json!({ "MaximumRetryAttempts": 1 })
}

/// The `message` member of a wire error, under either casing the JSON envelope uses.
fn message(answer: &Answer) -> String {
    answer
        .string("Message")
        .or_else(|| answer.string("message"))
        .unwrap_or_else(|| answer.text())
}

/// Where `ListFunctionEventInvokeConfigs` is bound, for the function `reference` names.
fn list_uri(reference: &str) -> String {
    format!("/2019-09-25/functions/{reference}/event-invoke-config/list")
}

/// Every operation on ONE configuration, driven on the pair `reference`/`qualifier`.
async fn single_configuration_answers(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
) -> Vec<(&'static str, Answer)> {
    let uri = config_uri(reference, qualifier);
    vec![
        (
            "PutFunctionEventInvokeConfig",
            call(router, "PUT", &uri, &write_body()).await,
        ),
        (
            "GetFunctionEventInvokeConfig",
            call(router, "GET", &uri, &Value::Null).await,
        ),
        (
            "UpdateFunctionEventInvokeConfig",
            call(router, "POST", &uri, &write_body()).await,
        ),
        (
            "DeleteFunctionEventInvokeConfig",
            call(router, "DELETE", &uri, &Value::Null).await,
        ),
    ]
}

/// Create the function this suite holds, publish one version of it and give that version an
/// alias, so a row can name a version and an alias that do not exist on a function that
/// does.
async fn create_held(router: &MultiServiceRouter) {
    create_function(router, HELD).await;
    let published = publish_version(router, HELD).await;
    let aliased = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{HELD}/aliases"),
        &json!({ "Name": HELD_ALIAS, "FunctionVersion": published }),
    )
    .await;
    assert_eq!(
        aliased.status,
        201,
        "CreateAlias {HELD_ALIAS:?} of {HELD:?} must succeed, got {} {}",
        aliased.status,
        aliased.text()
    );
}

/// One row of the refusal table.
struct Absent {
    /// What this row varies, for the assertion messages.
    label: &'static str,
    /// The `FunctionName` the request carries.
    reference: String,
    /// The `Qualifier` query parameter, when the row sends one.
    qualifier: Option<&'static str>,
    /// Whether `ListFunctionEventInvokeConfigs` can be driven with this reference: it takes
    /// no `Qualifier` and refuses a `FunctionName` that carries one, so only the rows whose
    /// whole reference names an absent FUNCTION drive it.
    lists: bool,
}

/// Every row of the refusal table; see the module documentation for the dimensions.
fn absent_rows() -> Vec<Absent> {
    vec![
        Absent {
            label: "a function nothing created, named bare",
            reference: ABSENT.to_string(),
            qualifier: None,
            lists: true,
        },
        Absent {
            label: "a function nothing created, in the function: prefix form",
            reference: format!("function:{ABSENT}"),
            qualifier: None,
            lists: true,
        },
        Absent {
            label: "a function nothing created, as a partial ARN",
            reference: format!("{ACCOUNT}:function:{ABSENT}"),
            qualifier: None,
            lists: true,
        },
        Absent {
            label: "a function nothing created, as a full ARN of this Region",
            reference: format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{ABSENT}"),
            qualifier: None,
            lists: true,
        },
        Absent {
            label: "a version the held function never published, in the Qualifier parameter",
            reference: HELD.to_string(),
            qualifier: Some(MISSING_VERSION),
            lists: false,
        },
        Absent {
            label: "a version the held function never published, inside the FunctionName",
            reference: format!("{HELD}:{MISSING_VERSION}"),
            qualifier: None,
            lists: false,
        },
        Absent {
            label: "an alias the held function does not have, in the Qualifier parameter",
            reference: HELD.to_string(),
            qualifier: Some(MISSING_ALIAS),
            lists: false,
        },
        Absent {
            label: "an alias the held function does not have, inside the FunctionName",
            reference: format!("{HELD}:{MISSING_ALIAS}"),
            qualifier: None,
            lists: false,
        },
        Absent {
            label: "a version of a function that does not exist, in the Qualifier parameter",
            reference: ABSENT.to_string(),
            qualifier: Some(MISSING_VERSION),
            lists: false,
        },
        Absent {
            label: "an alias of a function that does not exist, as a qualified full ARN",
            reference: format!(
                "arn:aws:lambda:{REGION}:{ACCOUNT}:function:{ABSENT}:{MISSING_ALIAS}"
            ),
            qualifier: None,
            lists: false,
        },
    ]
}

/// The rule itself: every operation on one of these configurations answers the one fixed
/// sentence for a pair that names no function, version or alias — and nothing is created by
/// the writes it refused.
#[tokio::test]
async fn a_pair_naming_no_function_version_or_alias_is_refused_with_the_one_fixed_sentence() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_held(&router).await;

    for row in absent_rows() {
        let mut answers =
            single_configuration_answers(&router, &row.reference, row.qualifier).await;
        if row.lists {
            answers.push((
                "ListFunctionEventInvokeConfigs",
                call(&router, "GET", &list_uri(&row.reference), &Value::Null).await,
            ));
        }

        for (operation, answer) in &answers {
            assert_eq!(
                answer.status,
                404,
                "case {:?} ({operation}): expected the modelled 404, got {} {}",
                row.label,
                answer.status,
                answer.text()
            );
            assert!(
                answer.error_code().contains(NOT_FOUND),
                "case {:?} ({operation}): expected {NOT_FOUND}, got {:?}",
                row.label,
                answer.error_code()
            );
            let reported = message(answer);
            assert_eq!(
                reported, FUNCTION_DOES_NOT_EXIST,
                "case {:?} ({operation}): expected {FUNCTION_DOES_NOT_EXIST:?}, got \
                 {reported:?}",
                row.label
            );
        }
    }

    // Every refused write left the store as it was: the held function holds no
    // configuration at all, so no refused put created one on a qualifier it addressed.
    let listed = call(&router, "GET", &list_uri(HELD), &Value::Null).await;
    assert_eq!(
        listed.status,
        200,
        "the held function must still list its configurations, got {} {}",
        listed.status,
        listed.text()
    );
    let configs = listed
        .json()
        .get("FunctionEventInvokeConfigs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        configs.is_empty(),
        "a refused write must store no configuration, got {configs:?}"
    );
}

/// A Region this endpoint does not serve is not a resource it could not find: it is settled
/// by reading the identifier, so it keeps the sentence that states the two Regions which
/// disagree rather than the fixed sentence above.
#[tokio::test]
async fn a_cross_region_identifier_states_the_two_regions_rather_than_the_fixed_sentence() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_held(&router).await;

    let expected =
        format!("Functions from '{OTHER_REGION}' are not reachable in this region ('{REGION}')");
    // The same function name in both forms: one held here, one named in another Region. The
    // refusal is about the identifier, so it does not depend on whether the function exists.
    for name in [HELD, ABSENT] {
        let reference = format!("arn:aws:lambda:{OTHER_REGION}:{ACCOUNT}:function:{name}");
        let mut answers = single_configuration_answers(&router, &reference, None).await;
        answers.push((
            "ListFunctionEventInvokeConfigs",
            call(&router, "GET", &list_uri(&reference), &Value::Null).await,
        ));
        for (operation, answer) in &answers {
            assert_eq!(
                answer.status,
                404,
                "case {name:?} ({operation}): expected the modelled 404, got {} {}",
                answer.status,
                answer.text()
            );
            let reported = message(answer);
            assert_eq!(
                reported, expected,
                "case {name:?} ({operation}): expected {expected:?}, got {reported:?}"
            );
        }
    }
}

/// The other half of the rule: a pair that DOES name a function, version or alias is not
/// refused for the function at all — a put on it succeeds and is read back, and a qualifier
/// that holds no configuration states THAT instead, naming the configuration's own ARN.
#[tokio::test]
async fn a_pair_that_resolves_is_answered_for_the_configuration_rather_than_the_function() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_held(&router).await;

    // A qualifier of a function this store holds that nobody has configured: the resource
    // that is missing is the CONFIGURATION, and the sentence says so. The put is left out of
    // this half because it CREATES one rather than refusing — it is the control below.
    for (label, qualifier) in [
        ("the unpublished version", None),
        ("a published version", Some("1")),
        ("an alias", Some(HELD_ALIAS)),
    ] {
        let expected = format!(
            "The function {} doesn't have an EventInvokeConfig",
            configured_arn(HELD, qualifier)
        );
        let uri = config_uri(HELD, qualifier);
        for (operation, answer) in [
            (
                "GetFunctionEventInvokeConfig",
                call(&router, "GET", &uri, &Value::Null).await,
            ),
            (
                "UpdateFunctionEventInvokeConfig",
                call(&router, "POST", &uri, &write_body()).await,
            ),
            (
                "DeleteFunctionEventInvokeConfig",
                call(&router, "DELETE", &uri, &Value::Null).await,
            ),
        ] {
            assert_eq!(
                answer.status,
                404,
                "case {label:?} ({operation}): expected the modelled 404, got {} {}",
                answer.status,
                answer.text()
            );
            let reported = message(&answer);
            assert_eq!(
                reported, expected,
                "case {label:?} ({operation}): expected {expected:?}, got {reported:?}"
            );
        }
    }

    // The control: a put on a qualifier that resolves stores the configuration, and the read
    // of the same pair reports it on the qualified ARN — so the resolution that produces the
    // refusals above still resolves what it should.
    let put = call(
        &router,
        "PUT",
        &config_uri(HELD, Some(HELD_ALIAS)),
        &write_body(),
    )
    .await;
    assert_eq!(
        put.status,
        200,
        "PutFunctionEventInvokeConfig on the alias must succeed, got {} {}",
        put.status,
        put.text()
    );
    let read = call(
        &router,
        "GET",
        &config_uri(HELD, Some(HELD_ALIAS)),
        &Value::Null,
    )
    .await;
    let reported = read.json();
    let expected_arn = configured_arn(HELD, Some(HELD_ALIAS));
    assert_eq!(
        (
            read.status,
            reported.get("FunctionArn").and_then(Value::as_str),
            reported.get("MaximumRetryAttempts").and_then(Value::as_i64)
        ),
        (200, Some(expected_arn.as_str()), Some(1)),
        "the stored configuration must be read back on the qualified ARN, got {} {}",
        read.status,
        read.text()
    );
}
