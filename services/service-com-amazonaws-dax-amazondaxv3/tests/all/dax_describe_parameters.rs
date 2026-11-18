// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DescribeParameters`: the detailed parameter list of ONE stored
//! DAX parameter group, over the wire.
//!
//! Every case drives `POST /` with `X-Amz-Target: AmazonDAXV3.DescribeParameters`
//! through the same `MultiServiceRouter` the production router builds, so what is
//! asserted is the HTTP behaviour a DAX SDK client sees — not an internal function call.
//!
//! The tables below span the CLASS each rule covers: the group AWS provides and groups
//! created under names of different shapes, every parameter the catalog defines, both
//! `Source` values its members document plus values nothing is defined with, each
//! published `MaxResults` bound plus one inside and one past each, every form of
//! continuation token, and the group list resumed at each of its parameter names.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_dax_amazondaxv3::{state, EmulatedService};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The status this service's `awsJson1_1` wire puts on every modelled client fault, the
/// per-shape `@httpError` being inert on an RPC protocol.
const CLIENT_FAULT_STATUS: u16 = 400;

/// The fixed text of the not-found template the read of a group's parameters answers with
/// (`CacheParameterGroup` followed by `not found: `, DAX's own wording). Written out here
/// rather than taken from the service, so the wording itself is pinned and not merely
/// echoed back; the only variable part is the requested `ParameterGroupName`.
const ADDRESSED_NOT_FOUND: &str = "CacheParameterGroupnot found: ";

/// Names spanning the class a create accepts (the name is read as a parameter-group
/// identifier, and the model constrains `ParameterGroupName` to no pattern or length): a
/// short name, a hyphenated one, one of letters and digits, and one with mixed case and a
/// digit.
const CREATED_GROUPS: [&str; 4] = ["pg", "my-dax-parameters", "params-v2", "Group9"];

/// A router carrying only this service, over its own engine, so each test starts from a
/// fresh store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Call one operation and return its HTTP status and parsed JSON body.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonDAXV3.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/dax/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code of a failed call: the `__type` member, without any namespace.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `ParameterName` one reported parameter carries.
fn reported_name(parameter: &Value) -> String {
    parameter["ParameterName"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `ParameterName`s the response lists, in wire order.
fn reported_names(body: &Value) -> Vec<String> {
    body["Parameters"]
        .as_array()
        .map(|parameters| parameters.iter().map(reported_name).collect())
        .unwrap_or_default()
}

/// The reported parameters, keyed by `ParameterName`.
fn by_name(body: &Value) -> BTreeMap<String, Value> {
    body["Parameters"]
        .as_array()
        .map(|parameters| {
            parameters
                .iter()
                .map(|parameter| (reported_name(parameter), parameter.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// A continuation token as this service encodes one: base64 of the name a page resumes
/// at (`state::page` / `state::decode_token`).
fn token_for(name: &str) -> String {
    aws_smithy_types::base64::encode(name)
}

/// Create one parameter group, asserting the create itself succeeded.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(
        router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": name, "Description": "created for a parameter read" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name}: expected the group created, got {status} {body:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule: the operation reports, for the NAMED group, every parameter that group holds a
// value for — each rendered from this service's one catalog
// (`state::DEFAULT_PARAMETERS`) with the value the group carries, which for a group that
// has never been updated is the system-defined value.
// Source: `DescribeParameters` ("Returns the detailed parameter list for a particular
// parameter group") and `Parameter`'s members.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_group_reports_the_catalog_with_the_value_it_carries() {
    let router = emulator();
    for name in CREATED_GROUPS {
        create_group(&router, name).await;
    }

    // Rows: the group AWS provides itself, and every created group — a group is read
    // back through this operation after the create that stored it.
    let mut groups = vec![state::DEFAULT_PARAMETER_GROUP];
    groups.extend(CREATED_GROUPS);

    for group in groups {
        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({ "ParameterGroupName": group }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {group}: expected the parameter list, got {status} {body:?}"
        );

        let reported = by_name(&body);
        assert_eq!(
            reported.len(),
            state::DEFAULT_PARAMETERS.len(),
            "case {group}: expected all {} defined parameters, got {body:?}",
            state::DEFAULT_PARAMETERS.len()
        );

        // Each parameter the catalog defines, asserted member by member against its
        // definition, so no parameter is reported with another's rendering.
        for definition in state::DEFAULT_PARAMETERS {
            let case = format!("{group}/{}", definition.name);
            let parameter = reported
                .get(definition.name)
                .unwrap_or_else(|| panic!("case {case}: expected it reported, got {body:?}"));
            assert_eq!(
                parameter["ParameterValue"],
                json!(definition.default_value),
                "case {case}: expected the value the group carries {:?}, got {parameter:?}",
                definition.default_value
            );
            assert_eq!(
                parameter["Description"],
                json!(definition.description),
                "case {case}: expected the catalog description, got {parameter:?}"
            );
            assert_eq!(
                parameter["Source"],
                json!(definition.source),
                "case {case}: expected the Source its catalog entry publishes, got {parameter:?}"
            );
            assert_eq!(
                parameter["DataType"],
                json!(definition.data_type),
                "case {case}: expected DataType {:?}, got {parameter:?}",
                definition.data_type
            );
            assert_eq!(
                parameter["AllowedValues"],
                json!(definition.allowed_values),
                "case {case}: expected AllowedValues {:?}, got {parameter:?}",
                definition.allowed_values
            );
            assert_eq!(
                parameter["IsModifiable"],
                json!(definition.is_modifiable),
                "case {case}: expected IsModifiable {:?}, got {parameter:?}",
                definition.is_modifiable
            );
            assert_eq!(
                parameter["ChangeType"],
                json!(definition.change_type),
                "case {case}: expected ChangeType {:?}, got {parameter:?}",
                definition.change_type
            );
            assert_eq!(
                parameter["ParameterType"],
                json!(state::PARAMETER_TYPE_DEFAULT),
                "case {case}: a parameter with no per-node-type value is DEFAULT, \
                 got {parameter:?}"
            );
            assert_eq!(
                parameter["NodeTypeSpecificValues"],
                json!([]),
                "case {case}: expected no node-type-specific values, got {parameter:?}"
            );
        }

        // The group's parameters page in one stable, duplicate-free name order.
        let names = reported_names(&body);
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            names, sorted,
            "case {group}: expected sorted, duplicate-free name order, got {names:?}"
        );
    }
}

/// Rule: a group that has never been updated carries the system-defined values, so its
/// parameter list is the one `DescribeDefaultParameters` reports for the caching
/// software — including each parameter's `Source`.
#[tokio::test]
async fn an_unmodified_group_reports_the_system_defined_parameters() {
    let router = emulator();
    for name in CREATED_GROUPS {
        create_group(&router, name).await;
    }

    let (status, defaults) = call(&router, "DescribeDefaultParameters", json!({})).await;
    assert_eq!(status, 200, "DescribeDefaultParameters: got {defaults:?}");

    // Rows: the group AWS provides, and every created group.
    let mut groups = vec![state::DEFAULT_PARAMETER_GROUP];
    groups.extend(CREATED_GROUPS);

    for group in groups {
        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({ "ParameterGroupName": group }),
        )
        .await;
        assert_eq!(status, 200, "case {group}: got {status} {body:?}");
        assert_eq!(
            by_name(&body),
            by_name(&defaults),
            "case {group}: expected the system-defined parameter list {defaults:?}, \
             got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: the value each parameter reports is the one THAT group stores, so a read issued
// after a change to a group's parameters reports the values that change persisted, while
// every other member of the parameter keeps the catalog entry's rendering and every other
// group keeps its own values.
// Source: `DescribeParameters` ("the detailed parameter list for a particular parameter
// group") with `UpdateParameterGroup` ("Modifies the parameters of a parameter group").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_read_reports_the_values_the_group_stores_not_the_catalog() {
    let router = emulator();
    for name in CREATED_GROUPS {
        create_group(&router, name).await;
    }

    // Rows: each parameter the catalog publishes changed on its own, both of them in one
    // change, and a change back to the catalog's own value — the values spanning the
    // published range rather than repeating one cited number.
    let mut cases: Vec<(String, Vec<(&str, String)>)> = Vec::new();
    for definition in state::DEFAULT_PARAMETERS {
        for value in ["0", "1", "75000", definition.default_value] {
            cases.push((
                format!("{} = {value}", definition.name),
                vec![(definition.name, value.to_string())],
            ));
        }
    }
    cases.push((
        "every published parameter at once".to_string(),
        state::DEFAULT_PARAMETERS
            .iter()
            .enumerate()
            .map(|(index, definition)| (definition.name, format!("{}", 10_000 + index * 13_000)))
            .collect(),
    ));

    for (index, (case, pairs)) in cases.iter().enumerate() {
        // A group per row, so each row starts from the values a group that has never been
        // changed holds; the untouched group beside it is read at the end of the row.
        let group = format!("read-back-{index:03}");
        create_group(&router, &group).await;
        let (status, updated) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": group,
                "ParameterNameValues": pairs
                    .iter()
                    .map(|(name, value)| json!({
                        "ParameterName": name,
                        "ParameterValue": value,
                    }))
                    .collect::<Vec<Value>>(),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the change accepted, got {status} {updated:?}"
        );

        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({ "ParameterGroupName": group }),
        )
        .await;
        assert_eq!(status, 200, "case {case}: got {status} {body:?}");
        let reported = by_name(&body);

        for definition in state::DEFAULT_PARAMETERS {
            let expected = pairs
                .iter()
                .find(|(name, _)| *name == definition.name)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| definition.default_value.to_string());
            let parameter = reported
                .get(definition.name)
                .unwrap_or_else(|| panic!("case {case}: expected it reported, got {body:?}"));
            assert_eq!(
                parameter["ParameterValue"],
                json!(expected),
                "case {case}: expected {} to report the value the group stores \
                 ({expected:?}), got {parameter:?}",
                definition.name
            );
            // Only the value is the group's; every other member is the catalog entry's.
            assert_eq!(
                parameter["Description"],
                json!(definition.description),
                "case {case}: expected the catalog description, got {parameter:?}"
            );
            assert_eq!(
                parameter["Source"],
                json!(definition.source),
                "case {case}: expected the catalog Source, got {parameter:?}"
            );
            assert_eq!(
                parameter["DataType"],
                json!(definition.data_type),
                "case {case}: expected the catalog DataType, got {parameter:?}"
            );
            assert_eq!(
                parameter["AllowedValues"],
                json!(definition.allowed_values),
                "case {case}: expected the catalog AllowedValues, got {parameter:?}"
            );
        }

        // A group nothing changed still reports the system-defined values, so the change
        // above reached one group's own stored values and not the catalog behind them.
        let (status, untouched) = call(
            &router,
            "DescribeParameters",
            json!({ "ParameterGroupName": CREATED_GROUPS[0] }),
        )
        .await;
        assert_eq!(status, 200, "case {case}: got {status} {untouched:?}");
        for definition in state::DEFAULT_PARAMETERS {
            let parameter = by_name(&untouched)
                .get(definition.name)
                .cloned()
                .unwrap_or(Value::Null);
            assert_eq!(
                parameter["ParameterValue"],
                json!(definition.default_value),
                "case {case}: an unchanged group keeps the system-defined value of {}, \
                 got {parameter:?}",
                definition.name
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule: `Source` selects the parameters defined that way — "How the parameter is
// defined. For example, system denotes a system-defined parameter."
// (`DescribeParametersRequest$Source` / `Parameter$Source`) — so a value no parameter is
// defined with selects none of them.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn source_selects_the_parameters_defined_that_way() {
    let router = emulator();
    create_group(&router, CREATED_GROUPS[0]).await;

    // Rows: both values the member's documentation names (`state::PARAMETER_SOURCES`,
    // tested here for `system` as well as the `user` the catalog actually publishes),
    // plus values nothing is defined with: another service's vocabulary, the right value
    // in the wrong case, and the empty string.
    let mut sources: Vec<&str> = state::PARAMETER_SOURCES.to_vec();
    sources.extend(["engine-default", "USER", ""]);

    for group in [state::DEFAULT_PARAMETER_GROUP, CREATED_GROUPS[0]] {
        for source in &sources {
            let (status, body) = call(
                &router,
                "DescribeParameters",
                json!({ "ParameterGroupName": group, "Source": source }),
            )
            .await;
            let case = format!("{group}/Source={source:?}");
            assert_eq!(status, 200, "case {case}: got {status} {body:?}");

            let expected: Vec<String> = state::DEFAULT_PARAMETERS
                .iter()
                .filter(|definition| definition.source == *source)
                .map(|definition| definition.name.to_string())
                .collect();
            assert_eq!(
                reported_names(&body),
                expected,
                "case {case}: expected exactly the parameters defined that way \
                 ({expected:?}), got {body:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule: the parameters belong to the named group, so a name this account holds no
// parameter group for is refused with `ParameterGroupNotFoundFault`, on the client-fault
// status this service's `awsJson1_1` wire puts on every client fault (the shape's
// `@httpError` 404 being inert there, as `dax_client_fault_wire_status.rs` pins).
// Source: `DescribeParameters`'s modelled errors ("The specified parameter group does
// not exist").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_group_that_does_not_exist_is_refused() {
    let router = emulator();
    create_group(&router, CREATED_GROUPS[0]).await;

    // Rows: a name nothing created, a name differing from an existing one only in case,
    // the default group's family without its prefix, and the empty name.
    let missing = [
        "no-such-group",
        "PG",
        state::DEFAULT_PARAMETER_GROUP_FAMILY,
        "",
    ];

    for name in missing {
        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({ "ParameterGroupName": name }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?}: expected a refusal, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ParameterGroupNotFoundFault",
            "case {name:?}: expected ParameterGroupNotFoundFault, got {body:?}"
        );
        assert_eq!(
            body["message"],
            json!(format!("{ADDRESSED_NOT_FOUND}{name}")),
            "case {name:?}: expected the fault to name the group asked for, got {body:?}"
        );
    }

    // And a group that existed and was deleted resolves to nothing from then on.
    let (status, deleted) = call(
        &router,
        "DeleteParameterGroup",
        json!({ "ParameterGroupName": CREATED_GROUPS[0] }),
    )
    .await;
    assert_eq!(status, 200, "DeleteParameterGroup: got {status} {deleted:?}");
    let (status, body) = call(
        &router,
        "DescribeParameters",
        json!({ "ParameterGroupName": CREATED_GROUPS[0] }),
    )
    .await;
    assert_eq!(
        status, CLIENT_FAULT_STATUS,
        "case deleted group: expected a refusal, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ParameterGroupNotFoundFault",
        "case deleted group: expected ParameterGroupNotFoundFault, got {body:?}"
    );
    assert_eq!(
        body["message"],
        json!(format!("{ADDRESSED_NOT_FOUND}{}", CREATED_GROUPS[0])),
        "case deleted group: expected the fault to name the group asked for, got {body:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule: when the parameter group whose parameters this read was asked for cannot be
// resolved, the fault's message carries the `ParameterGroupName` the request sent,
// verbatim, so the caller is told WHICH group was missing. The template's fixed text is
// written out here rather than taken from the service, so the wording itself is pinned
// and not merely echoed back; the only variable part is the requested name. The other
// paths that raise this fault keep each their own recorded wording — the group list
// (`DescribeParameterGroups`) and the removal (`DeleteParameterGroup`) name no
// identifier, a cluster that references a group names it — which
// `dax_client_fault_wire_status.rs` pins side by side.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_not_found_fault_names_the_group_the_request_addressed() {
    let router = emulator();
    create_group(&router, CREATED_GROUPS[0]).await;

    // Rows spanning the names a request may ask for: one letter, a hyphenated name, one
    // carrying digits, a mixed-case one, a long one, one differing from a stored group by
    // a single character, one differing from it only in case, the empty name the model's
    // `@required` admits, a dotted name shaped like the group this service provides, and
    // one carrying a character no parameter-group identifier may hold. None of them names
    // a group this account holds.
    let absent = [
        "a",
        "no-such-group",
        "group9",
        "NoSuchGroup",
        "a-rather-long-but-valid-parameter-group-name-0123456789",
        "p",
        "PG",
        "",
        "default.no-such-family",
        "under_score",
    ];

    for name in absent {
        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({ "ParameterGroupName": name }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {name:?}: expected a refusal, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ParameterGroupNotFoundFault",
            "case {name:?}: expected ParameterGroupNotFoundFault, got {body:?}"
        );
        let expected = format!("{ADDRESSED_NOT_FOUND}{name}");
        assert_eq!(
            body["message"],
            json!(expected),
            "case {name:?}: expected the message {expected:?}, got {body:?}"
        );
    }

    // Every refusal above wrote nothing: the one stored group is still readable, with the
    // parameters it was created holding.
    let (status, body) = call(
        &router,
        "DescribeParameters",
        json!({ "ParameterGroupName": CREATED_GROUPS[0] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the stored group must still be readable, got {status} {body:?}"
    );
    let expected_names: Vec<String> = state::default_parameter_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(
        reported_names(&body),
        expected_names,
        "the stored group must still report every parameter it holds, got {body:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule: the paginated parameter read accepts a `MaxResults` inside the documented
// 20..=100 window and refuses anything outside it with the modelled
// `InvalidParameterValueException` (HTTP 400).
// Source: `DescribeParametersRequest$MaxResults` ("The value for MaxResults must be
// between 20 and 100").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_documented_max_results_window_is_honoured() {
    let router = emulator();
    // Rows: each published bound, one inside each bound, and one past each bound.
    let accepted = [
        state::MIN_MAX_RESULTS,
        state::MIN_MAX_RESULTS + 1,
        state::MAX_MAX_RESULTS - 1,
        state::MAX_MAX_RESULTS,
    ];
    let refused = [
        state::MIN_MAX_RESULTS - 1,
        1,
        0,
        -1,
        state::MAX_MAX_RESULTS + 1,
        1000,
    ];

    for max_results in accepted {
        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({
                "ParameterGroupName": state::DEFAULT_PARAMETER_GROUP,
                "MaxResults": max_results,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case MaxResults={max_results}: expected it accepted, got {status} {body:?}"
        );
        assert_eq!(
            by_name(&body).len(),
            state::DEFAULT_PARAMETERS.len(),
            "case MaxResults={max_results}: the group's parameters fit in one page this \
             size, got {body:?}"
        );
        assert!(
            body["NextToken"].is_null(),
            "case MaxResults={max_results}: a complete page carries no token, got {body:?}"
        );
    }

    for max_results in refused {
        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({
                "ParameterGroupName": state::DEFAULT_PARAMETER_GROUP,
                "MaxResults": max_results,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case MaxResults={max_results}: expected a refusal, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValueException",
            "case MaxResults={max_results}: expected InvalidParameterValueException, \
             got {body:?}"
        );
        assert_eq!(
            body["message"],
            json!(state::max_results_out_of_range_message()),
            "case MaxResults={max_results}: expected the fault's own template, got {body:?}"
        );
    }
}

/// Rule: a `NextToken` this service did not issue is refused with HTTP 400, never
/// answered with some arbitrary page and a 200.
#[tokio::test]
async fn a_continuation_token_this_service_did_not_issue_is_refused() {
    let router = emulator();
    // Rows: not base64, base64 of non-UTF-8 bytes, base64 of the empty string.
    let tokens = ["not-a-token!!", "/w==", ""];

    for token in tokens {
        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({
                "ParameterGroupName": state::DEFAULT_PARAMETER_GROUP,
                "MaxResults": state::MIN_MAX_RESULTS,
                "NextToken": token,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case NextToken={token:?}: expected a refusal, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValueException",
            "case NextToken={token:?}: expected InvalidParameterValueException, got {body:?}"
        );
    }
}

/// Rule: a continuation token resumes the group's parameter list AT the name it carries
/// — the page holds the parameters at or after it — and a token past the last parameter
/// yields an empty final page with no further token.
#[tokio::test]
async fn a_continuation_token_resumes_the_list_at_its_name() {
    let router = emulator();
    create_group(&router, CREATED_GROUPS[1]).await;
    let all = state::default_parameter_names();

    for group in [state::DEFAULT_PARAMETER_GROUP, CREATED_GROUPS[1]] {
        // Rows: resume at each parameter the group holds, so the first row is the whole
        // list and the last row is a single parameter.
        for (index, name) in all.iter().enumerate() {
            let (status, body) = call(
                &router,
                "DescribeParameters",
                json!({ "ParameterGroupName": group, "NextToken": token_for(name) }),
            )
            .await;
            let case = format!("{group}/NextToken={name}");
            assert_eq!(
                status, 200,
                "case {case}: expected it accepted, got {status} {body:?}"
            );
            let expected: Vec<String> = all[index..].iter().map(|name| (*name).to_string()).collect();
            assert_eq!(
                reported_names(&body),
                expected,
                "case {case}: expected the list from {name} on, got {body:?}"
            );
        }

        // And one row past the end: a name sorting after every parameter is an empty
        // page with no further token.
        let (status, body) = call(
            &router,
            "DescribeParameters",
            json!({
                "ParameterGroupName": group,
                "NextToken": token_for("zzz-past-the-end"),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {group}/NextToken past the end: expected it accepted, got {status} {body:?}"
        );
        assert_eq!(
            reported_names(&body),
            Vec::<String>::new(),
            "case {group}/NextToken past the end: expected an empty page, got {body:?}"
        );
        assert!(
            body["NextToken"].is_null(),
            "case {group}/NextToken past the end: expected no further token, got {body:?}"
        );
    }
}

/// Rule: the read reports the group's stored parameters and writes nothing, so a later
/// caller — of this read, of `DescribeParameterGroups`, or of `DescribeDefaultParameters`
/// — sees exactly what the first caller saw.
#[tokio::test]
async fn the_read_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    create_group(&router, CREATED_GROUPS[2]).await;

    let (status, groups_before) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "DescribeParameterGroups: got {groups_before:?}");
    let (status, defaults_before) = call(&router, "DescribeDefaultParameters", json!({})).await;
    assert_eq!(
        status, 200,
        "DescribeDefaultParameters: got {defaults_before:?}"
    );

    // Rows: the same read repeated, with a page size, and filtered by `Source` — none of
    // them may change what the next read reports.
    let requests = [
        ("plain", json!({ "ParameterGroupName": CREATED_GROUPS[2] })),
        (
            "with a page size",
            json!({
                "ParameterGroupName": CREATED_GROUPS[2],
                "MaxResults": state::MAX_MAX_RESULTS,
            }),
        ),
        (
            "filtered by Source",
            json!({
                "ParameterGroupName": CREATED_GROUPS[2],
                "Source": state::PARAMETER_SOURCE_USER,
            }),
        ),
    ];

    let (status, first) = call(
        &router,
        "DescribeParameters",
        json!({ "ParameterGroupName": CREATED_GROUPS[2] }),
    )
    .await;
    assert_eq!(status, 200, "first read: got {status} {first:?}");

    for (case, body) in requests {
        let (status, read) = call(&router, "DescribeParameters", body).await;
        assert_eq!(status, 200, "case {case}: got {status} {read:?}");

        let (status, again) = call(
            &router,
            "DescribeParameters",
            json!({ "ParameterGroupName": CREATED_GROUPS[2] }),
        )
        .await;
        assert_eq!(status, 200, "case {case}: got {status} {again:?}");
        assert_eq!(
            again, first,
            "case {case}: expected the same parameter list as the first read {first:?}, \
             got {again:?}"
        );
    }

    let (status, groups_after) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "DescribeParameterGroups: got {groups_after:?}");
    assert_eq!(
        groups_after, groups_before,
        "expected the groups unchanged by the reads, got {groups_after:?}"
    );
    let (status, defaults_after) = call(&router, "DescribeDefaultParameters", json!({})).await;
    assert_eq!(
        status, 200,
        "DescribeDefaultParameters: got {defaults_after:?}"
    );
    assert_eq!(
        defaults_after, defaults_before,
        "expected the system catalog unchanged by the reads, got {defaults_after:?}"
    );
}
