// SPDX-License-Identifier: CC0-1.0

//! What `CreateEventSourceMapping` tells a caller whose `FunctionName` resolves to no
//! function of this endpoint.
//!
//! RULE: `FunctionName` is "the name or ARN of the Lambda function" the mapping invokes
//! (`smithy_query(query="operation", operation="CreateEventSourceMapping")`), so it is a
//! PARAMETER of the mapping this call creates rather than the resource the call addresses —
//! the mapping has no identifier yet for anything to be found under. A reference that
//! resolves to no function, version or alias is therefore refused with the modelled
//! `InvalidParameterValueException` (HTTP 400) rather than with the modelled
//! `ResourceNotFoundException`, and its message is AWS's fixed sentence for an absent
//! function, `Function does not exist`, which carries nothing of the request; a reference
//! naming a Region this endpoint does not serve keeps the sentence that states the two
//! Regions that disagree. No mapping is stored either way.
//!
//! The rows below differ along what the rule generalises over: which documented
//! `FunctionName` form the reference is written in (bare name, the `function:` prefix form,
//! a partial ARN and a full ARN), whether it is qualified and by what (nothing, the
//! unpublished version by name, a version number, an alias name), and WHY it resolves to
//! nothing (no function of that name at all, a function that exists without the version or
//! alias named, a Region this endpoint does not serve). The last rows are the boundary: a
//! reference that DOES resolve, which this refusal must not touch, read back through
//! `GetEventSourceMapping`.

use std::sync::Arc;

use serde_json::json;

use crate::event_source_mappings::{
    create_function, create_mapping, create_request, get_mapping, list_mappings, refusal_message,
    router_on,
};

/// The modelled error a parameter of the mapping that cannot be satisfied is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The modelled error this refusal must NOT be: the absence of a resource the call
/// addresses.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The class of fault every 4xx error body of this service names in its `Type` member: a
/// request the caller can fix.
const USER_FAULT: &str = "User";

/// The Region the test harness signs for, which is the Region this endpoint serves.
const REGION: &str = "us-east-1";

/// A Region this endpoint does not serve, for the reference that names one.
const OTHER_REGION: &str = "us-west-2";

/// The account id every emulator ARN carries.
const ACCOUNT: &str = "000000000000";

/// The function that IS created, so that a reference qualified by a version or an alias it
/// does not have resolves to nothing for a reason other than the function's own absence.
const HELD: &str = "esm-absent-fn-held";

/// A function name nothing is created under.
const GHOST: &str = "esm-absent-fn-ghost";

/// The whole sentence a reference that resolves to no function is refused with — fixed text,
/// carrying nothing of the request.
const ABSENT_FUNCTION: &str = "Function does not exist";

/// The unqualified ARN of the function `name` in the Region this endpoint serves.
fn function_arn(region: &str, name: &str) -> String {
    format!("arn:aws:lambda:{region}:{ACCOUNT}:function:{name}")
}

/// The sentence a reference naming the Region `named` is refused with.
fn foreign_region(named: &str) -> String {
    format!("Functions from '{named}' are not reachable in this region ('{REGION}')")
}

/// One row of the table: the `FunctionName` a create states, the event source it maps and
/// what the create must answer.
struct Row {
    case: &'static str,
    /// The `FunctionName` the request carries.
    function_name: String,
    /// The `EventSourceArn` the request carries. Each ACCEPTED row names its own queue, so
    /// that no two accepted rows map the same (event source, function) pair — which is a
    /// conflict rather than a parameter fault.
    event_source: String,
    /// The sentence the refusal must carry, or `None` for a row the create must accept.
    refusal: Option<String>,
}

/// A queue of this account and Region, named `suffix`: the source every row maps. Nothing
/// here has to exist — what the rows decide is the FUNCTION half of the mapping.
fn queue(suffix: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:esm-absent-fn-{suffix}")
}

/// The rows: see the module docs.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bare name nothing was created under",
            function_name: GHOST.to_string(),
            event_source: queue("ghost-bare"),
            refusal: Some(ABSENT_FUNCTION.to_string()),
        },
        Row {
            case: "the function: prefix form of a name nothing was created under",
            function_name: format!("function:{GHOST}"),
            event_source: queue("ghost-prefixed"),
            refusal: Some(ABSENT_FUNCTION.to_string()),
        },
        Row {
            case: "the partial ARN of a name nothing was created under",
            function_name: format!("{ACCOUNT}:function:{GHOST}"),
            event_source: queue("ghost-partial"),
            refusal: Some(ABSENT_FUNCTION.to_string()),
        },
        Row {
            case: "the full ARN of a name nothing was created under",
            function_name: function_arn(REGION, GHOST),
            event_source: queue("ghost-arn"),
            refusal: Some(ABSENT_FUNCTION.to_string()),
        },
        Row {
            case: "a name nothing was created under, qualified by the unpublished version",
            function_name: format!("{GHOST}:$LATEST"),
            event_source: queue("ghost-latest"),
            refusal: Some(ABSENT_FUNCTION.to_string()),
        },
        Row {
            case: "a version number the held function does not have",
            function_name: format!("{HELD}:1"),
            event_source: queue("held-version"),
            refusal: Some(ABSENT_FUNCTION.to_string()),
        },
        Row {
            case: "an alias the held function does not have",
            function_name: format!("{HELD}:live"),
            event_source: queue("held-alias"),
            refusal: Some(ABSENT_FUNCTION.to_string()),
        },
        Row {
            case: "the full ARN of the held function qualified by a version it does not have",
            function_name: format!("{}:2", function_arn(REGION, HELD)),
            event_source: queue("held-arn-version"),
            refusal: Some(ABSENT_FUNCTION.to_string()),
        },
        Row {
            case: "the full ARN of the held function in a Region this endpoint does not serve",
            function_name: function_arn(OTHER_REGION, HELD),
            event_source: queue("held-other-region"),
            refusal: Some(foreign_region(OTHER_REGION)),
        },
        // The boundary: references that DO resolve, each on its own queue.
        Row {
            case: "the bare name of the held function",
            function_name: HELD.to_string(),
            event_source: queue("held-bare"),
            refusal: None,
        },
        Row {
            case: "the full ARN of the held function",
            function_name: function_arn(REGION, HELD),
            event_source: queue("held-arn"),
            refusal: None,
        },
        Row {
            case: "the held function qualified by the unpublished version",
            function_name: format!("{HELD}:$LATEST"),
            event_source: queue("held-latest"),
            refusal: None,
        },
    ]
}

/// RULE: a `CreateEventSourceMapping` whose `FunctionName` resolves to no function of this
/// endpoint is refused with the modelled 400 over that parameter — never with the not-found
/// of an addressed resource — carrying the fixed absent-function sentence, or the two-Region
/// sentence where the reference named a Region this endpoint does not serve; it stores no
/// mapping, while a reference that resolves is accepted and stored against the function it
/// resolved.
#[tokio::test]
async fn a_create_whose_function_name_resolves_to_nothing_is_refused_as_an_invalid_parameter() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, HELD).await;
    let held_arn = function_arn(REGION, HELD);

    // Every accepted row stores one mapping, and every refused row stores none, so the
    // listing after each row is what says the refusal persisted nothing.
    let mut stored = 0usize;
    for row in rows() {
        let case = row.case;
        let answer = create_mapping(
            &router,
            &create_request(&row.function_name, Some(&row.event_source), &json!({})),
        )
        .await;

        match &row.refusal {
            None => {
                assert_eq!(
                    answer.status,
                    202,
                    "case {case:?}: {} resolves to a function, so the create must answer 202, \
                     got {} {}",
                    row.function_name,
                    answer.status,
                    answer.text()
                );
                let uuid = answer.string("UUID").unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: an accepted create must answer a UUID, got {}",
                        answer.text()
                    )
                });
                stored += 1;
                // The round trip: the stored mapping names the function the reference
                // resolved, whichever form it was written in.
                let read = get_mapping(&router, &uuid).await;
                assert_eq!(
                    read.status,
                    200,
                    "case {case:?}: the created mapping must read back, got {} {}",
                    read.status,
                    read.text()
                );
                let reported = read.string("FunctionArn");
                assert_eq!(
                    reported.as_deref(),
                    Some(held_arn.as_str()),
                    "case {case:?}: expected the mapping to report FunctionArn {held_arn:?}, \
                     got {reported:?}"
                );
            }
            Some(expected) => {
                assert_eq!(
                    answer.status,
                    400,
                    "case {case:?}: {} resolves to no function, so the create must answer 400, \
                     got {} {}",
                    row.function_name,
                    answer.status,
                    answer.text()
                );
                let code = answer.error_code();
                assert!(
                    code.contains(INVALID_PARAMETER),
                    "case {case:?}: the refusal must be {INVALID_PARAMETER}, got {code:?}"
                );
                assert!(
                    !code.contains(NOT_FOUND),
                    "case {case:?}: the refusal must not be {NOT_FOUND}, got {code:?}"
                );
                let message = refusal_message(&answer).unwrap_or_default();
                assert_eq!(
                    &message, expected,
                    "case {case:?}: expected the refusal {expected:?}, got {message:?}"
                );
                let class = answer.string("Type");
                assert_eq!(
                    class.as_deref(),
                    Some(USER_FAULT),
                    "case {case:?}: expected the fault class {USER_FAULT:?}, got {class:?}"
                );
            }
        }

        let listed = list_mappings(&router, "").await;
        let held = listed.mappings().len();
        assert_eq!(
            held, stored,
            "case {case:?}: expected {stored} stored mapping(s) after this row, got {held} in {}",
            listed.text()
        );
    }
}
