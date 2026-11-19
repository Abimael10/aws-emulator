// SPDX-License-Identifier: CC0-1.0

//! How `ListRecommendedActions` words a refusal of a value outside a MODELLED
//! CONSTRAINT — the `@range` on its page size and the `@length` on its list and
//! string members — as opposed to a value outside an enum's set, which
//! `tests/all/validation_message_wording.rs` pins.
//!
//! One rule, over every constraint class this input carries: a refused member is
//! stated twice and the two statements carry different halves of it. The summary
//! (`N validation error(s) detected: …`) names the member's dotted path and the
//! requirement it failed and states NO value at all — no clause of it carries the
//! `Value '<sent>' at …` frame; the member's own `fieldList` entry is the one place
//! the value the request sent is reported, and it names no path, because the entry's
//! `name` member is what identifies the member there. The requirement's wording comes from the constraint and the bound the
//! model publishes, so the same sentence covers a page size below its minimum, one
//! above its maximum, a list shorter than its minimum length and a string outside
//! its length — and a request that breaks two at once is summarised with one clause
//! per member and still no value.
//!
//! Each case drives a real awsJson request through [`MultiServiceRouter::dispatch`],
//! so the refusal is the one a client reads, whether the offending value was found
//! at the constraint boundary or by the handler itself.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bcmrecommendedactions_awsbillingandcostmanagementrecommendedactions as bcm;

/// The awsJson1_0 target this service answers.
const TARGET: &str = "AWSBillingAndCostManagementRecommendedActions.ListRecommendedActions";

/// A SigV4 `Authorization` header whose credential scope names this service's
/// signing name — the routing signal the router reads. The signature is not
/// checked: request authentication is off for these tests.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261029/us-east-1/bcm-recommended-actions/aws4_request, SignedHeaders=host;x-amz-target, Signature=0000000000000000000000000000000000000000000000000000000000000000";

/// The modelled `@range` of `MaxResults` (`smithy_query(query="shape",
/// shape="MaxResults")`: min 1, max 1000).
const MAX_RESULTS_RANGE: (i64, i64) = (1, 1000);

/// The modelled minimum `@length` of `FilterValues` (`smithy_query(query="shape",
/// shape="FilterValues")`: min 1).
const FILTER_VALUES_MIN_LENGTH: i64 = 1;

/// The modelled `@length` of `NextToken` (`smithy_query(query="shape",
/// shape="NextToken")`: min 1, max 8192).
const NEXT_TOKEN_LENGTH: (i64, i64) = (1, 8192);

/// The modelled `reason` every refusal of a value the service READ carries
/// (`ValidationExceptionReason$fieldValidationFailed`).
const REASON: &str = "fieldValidationFailed";

/// One emulator: a shared engine, and a router with this service mounted on it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// A fresh emulator, holding the inventory the service computed for this
    /// account.
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(bcm::EmulatedService::new(engine));
        Self { router }
    }

    /// `ListRecommendedActions` with `body` as the request payload, reporting the
    /// status and the parsed response payload.
    async fn list(&self, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "bcm-recommended-actions.us-east-1.amazonaws.com")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", TARGET)
            .header("authorization", AUTHORIZATION)
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("serialize the request body"),
            ))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or_else(|error| {
                panic!(
                    "response body is not JSON ({error}): {}",
                    String::from_utf8_lossy(response.body())
                )
            })
        };
        (status, payload)
    }
}

/// The `message` of a refusal, asserted to be a 400 `ValidationException` stating
/// the modelled reason for a value the service read and refused.
fn refusal_message(case: &str, status: u16, body: &Value) -> String {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} with {body}"
    );
    let code = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: error body carries no __type: {body}"));
    assert!(
        code.ends_with("ValidationException"),
        "case {case:?}: expected a ValidationException, got {code:?} in {body}"
    );
    let reason = body.get("reason").and_then(Value::as_str);
    assert_eq!(
        reason,
        Some(REASON),
        "case {case:?}: expected reason {REASON:?}, got {reason:?} in {body}"
    );
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a refusal must state a message, got {body}"))
        .to_string()
}

/// The `fieldList` entries of a refusal, as `(name, message)` pairs.
fn field_list(case: &str, body: &Value) -> Vec<(String, String)> {
    body.get("fieldList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected a fieldList array, got {body}"))
        .iter()
        .map(|field| {
            let name = field
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a fieldList entry names no member in {body}"));
            let message = field
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: entry {name:?} states no message in {body}")
                });
            (name.to_string(), message.to_string())
        })
        .collect()
}

/// One member a refusal states: the dotted path it names the member by, the
/// requirement that member failed, and the value a refusal REPORTS for it in that
/// member's own entry (the value as sent for a scalar, the element count for a list
/// — the rendering the framework gives a collection).
struct RefusedMember {
    path: String,
    requirement: String,
    reported: String,
}

/// One constraint-refusal case: a request, and every member the refusal states.
struct Case {
    case: String,
    request: Value,
    refused: Vec<RefusedMember>,
}

/// The requirement clause of a `@range` minimum, as the bound the model publishes.
fn min_value(bound: i64) -> String {
    format!("Member must have value greater than or equal to {bound}")
}

/// The requirement clause of a `@range` maximum.
fn max_value(bound: i64) -> String {
    format!("Member must have value less than or equal to {bound}")
}

/// The requirement clause of a `@length` minimum.
fn min_length(bound: i64) -> String {
    format!("Member must have length greater than or equal to {bound}")
}

/// The requirement clause of a `@length` maximum.
fn max_length(bound: i64) -> String {
    format!("Member must have length less than or equal to {bound}")
}

/// The dotted path of a member of the `position`-th (1-based) `ActionFilter`.
fn filter_path(position: usize, member: &str) -> String {
    format!("filter.actions.{position}.member.{member}")
}

/// The value a refusal reports for a string the request sent: the string, or —
/// past the length a refusal echoes — its first bytes with the rest counted, the
/// elision the framework applies to a caller-chosen value (`MAX_ECHOED_VALUE` in
/// `vendor/smithy-schema-server/src/constraints.rs`).
fn reported(sent: &str) -> String {
    const MAX_ECHOED: usize = 256;
    if sent.len() <= MAX_ECHOED {
        return sent.to_string();
    }
    format!("{}... [{} bytes]", &sent[..MAX_ECHOED], sent.len())
}

/// A filter naming one or more `ActionFilter`s.
fn filters(actions: Vec<Value>) -> Value {
    json!({"filter": {"actions": actions}})
}

/// Requests whose values are inside the published value sets but outside a
/// modelled constraint.
///
/// The rows differ along the CONSTRAINT class (a numeric range, a list's length, a
/// string's length), the bound crossed (minimum and maximum, one step past it and
/// far past it), the kind of member (a top-level scalar, a list nested in a
/// structure in a list), which element of a list carries the offending member, and
/// how many members one request refuses at once.
fn cases() -> Vec<Case> {
    let (min_results, max_results) = MAX_RESULTS_RANGE;
    let (min_token, max_token) = NEXT_TOKEN_LENGTH;
    let long_token = "x".repeat(max_token as usize + 1);
    let good_filter =
        json!({"key": "FEATURE", "matchOption": "EQUALS", "values": ["BUDGETS"]});
    let empty_values = json!({"key": "SEVERITY", "matchOption": "NOT_EQUALS", "values": []});
    vec![
        Case {
            case: "a page size one below the published minimum".to_string(),
            request: json!({"maxResults": min_results - 1}),
            refused: vec![RefusedMember {
                path: "maxResults".to_string(),
                requirement: min_value(min_results),
                reported: (min_results - 1).to_string(),
            }],
        },
        Case {
            case: "a negative page size".to_string(),
            request: json!({"maxResults": -7}),
            refused: vec![RefusedMember {
                path: "maxResults".to_string(),
                requirement: min_value(min_results),
                reported: "-7".to_string(),
            }],
        },
        Case {
            case: "a page size one past the published maximum".to_string(),
            request: json!({"maxResults": max_results + 1}),
            refused: vec![RefusedMember {
                path: "maxResults".to_string(),
                requirement: max_value(max_results),
                reported: (max_results + 1).to_string(),
            }],
        },
        Case {
            case: "a page size far past the published maximum".to_string(),
            request: json!({"maxResults": 1_000_000}),
            refused: vec![RefusedMember {
                path: "maxResults".to_string(),
                requirement: max_value(max_results),
                reported: "1000000".to_string(),
            }],
        },
        Case {
            case: "an empty values list in the only filter".to_string(),
            request: filters(vec![empty_values.clone()]),
            refused: vec![RefusedMember {
                path: filter_path(1, "values"),
                requirement: min_length(FILTER_VALUES_MIN_LENGTH),
                reported: "0 items".to_string(),
            }],
        },
        Case {
            case: "an empty values list in the second filter".to_string(),
            request: filters(vec![good_filter.clone(), empty_values.clone()]),
            refused: vec![RefusedMember {
                path: filter_path(2, "values"),
                requirement: min_length(FILTER_VALUES_MIN_LENGTH),
                reported: "0 items".to_string(),
            }],
        },
        Case {
            case: "two filters whose values lists are both empty".to_string(),
            request: filters(vec![empty_values.clone(), empty_values.clone()]),
            refused: vec![
                RefusedMember {
                    path: filter_path(1, "values"),
                    requirement: min_length(FILTER_VALUES_MIN_LENGTH),
                    reported: "0 items".to_string(),
                },
                RefusedMember {
                    path: filter_path(2, "values"),
                    requirement: min_length(FILTER_VALUES_MIN_LENGTH),
                    reported: "0 items".to_string(),
                },
            ],
        },
        Case {
            case: "a token one character short of the published minimum".to_string(),
            request: json!({"nextToken": ""}),
            refused: vec![RefusedMember {
                path: "nextToken".to_string(),
                requirement: min_length(min_token),
                reported: String::new(),
            }],
        },
        Case {
            case: "a token one character past the published maximum".to_string(),
            request: json!({"nextToken": long_token.clone()}),
            refused: vec![RefusedMember {
                path: "nextToken".to_string(),
                requirement: max_length(max_token),
                reported: reported(&long_token),
            }],
        },
        Case {
            case: "a page size and a values list, each outside its own constraint".to_string(),
            request: json!({
                "maxResults": min_results - 1,
                "filter": {"actions": [empty_values.clone()]},
            }),
            refused: vec![
                RefusedMember {
                    path: "maxResults".to_string(),
                    requirement: min_value(min_results),
                    reported: (min_results - 1).to_string(),
                },
                RefusedMember {
                    path: filter_path(1, "values"),
                    requirement: min_length(FILTER_VALUES_MIN_LENGTH),
                    reported: "0 items".to_string(),
                },
            ],
        },
        Case {
            case: "a page size outside its range and a token outside its length".to_string(),
            request: json!({"maxResults": max_results + 1, "nextToken": ""}),
            refused: vec![
                RefusedMember {
                    path: "maxResults".to_string(),
                    requirement: max_value(max_results),
                    reported: (max_results + 1).to_string(),
                },
                RefusedMember {
                    path: "nextToken".to_string(),
                    requirement: min_length(min_token),
                    reported: String::new(),
                },
            ],
        },
    ]
}

#[tokio::test]
async fn a_constraint_refusal_summarises_the_member_and_its_requirement_without_the_value() {
    let emulator = Emulator::new();

    for Case {
        case,
        request,
        refused,
    } in cases()
    {
        let (status, body) = emulator.list(&request).await;
        let message = refusal_message(&case, status, &body);

        // The summary: one clause per refused member, naming the member path and
        // the requirement it failed. The clauses are compared as a set because the
        // summary states one per member and the rule says nothing about which
        // member a request is told about first.
        let expected: BTreeSet<String> = refused
            .iter()
            .map(|member| {
                format!(
                    "Value at '{}' failed to satisfy constraint: {}",
                    member.path, member.requirement
                )
            })
            .collect();
        let count = refused.len();
        let prefix = if count == 1 {
            "1 validation error detected: ".to_string()
        } else {
            format!("{count} validation errors detected: ")
        };
        assert!(
            message.starts_with(&prefix),
            "case {case:?}: expected a summary opening {prefix:?}, got {message:?}"
        );
        let stated: BTreeSet<String> = message[prefix.len()..]
            .split("; ")
            .map(str::to_string)
            .collect();
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the summary to state {expected:?}, got {stated:?}"
        );

        // No clause of the summary carries a value: the frame an echoed value
        // appears in is absent from it altogether, whichever constraint failed.
        assert!(
            !message.contains("Value '"),
            "case {case:?}: the summary must state no value, got {message:?}"
        );
        for member in &refused {
            assert!(
                !message.contains(&format!("Value '{}'", member.reported)),
                "case {case:?}: the summary must not report the value of {:?}, got {message:?}",
                member.path
            );
        }
        // The member's own entry: the same requirement, plus the value the request
        // sent — the half of the refusal the summary withholds — and no member path,
        // which the entry's `name` carries instead.
        let fields = field_list(&case, &body);
        assert_eq!(
            fields.len(),
            count,
            "case {case:?}: expected {count} fieldList entries, got {fields:?}"
        );
        for member in &refused {
            let stated = fields
                .iter()
                .find(|(name, _)| name == &member.path)
                .map(|(_, message)| message.clone())
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected an entry naming {:?}, got {fields:?}",
                        member.path
                    )
                });
            let expected_entry = format!(
                "Value '{}' failed to satisfy constraint: {}",
                member.reported, member.requirement
            );
            assert_eq!(
                stated, expected_entry,
                "case {case:?}: expected the entry for {:?} to state {expected_entry:?}, got \
                 {stated:?}",
                member.path
            );
        }
    }
}

#[tokio::test]
async fn an_absent_required_member_is_stated_as_absent_and_not_as_a_withheld_value() {
    let emulator = Emulator::new();

    // The other edge of the same rule: what the summary withholds is the value the
    // request SENT, and a member the request left out sent none — so its clause is
    // the one for an absent member, stating that the member must not be null, in
    // both the summary and that member's own entry. `ActionFilter$key`,
    // `$matchOption` and `$values` are each @required
    // (`smithy_query(query="shape", shape="ActionFilter")`).
    let cases: Vec<(String, Value, String)> = vec![
        (
            "a filter with no key".to_string(),
            filters(vec![json!({"matchOption": "EQUALS", "values": ["BUDGETS"]})]),
            filter_path(1, "key"),
        ),
        (
            "a filter with no matchOption".to_string(),
            filters(vec![json!({"key": "FEATURE", "values": ["BUDGETS"]})]),
            filter_path(1, "matchOption"),
        ),
        (
            "a filter with no values".to_string(),
            filters(vec![json!({"key": "TYPE", "matchOption": "NOT_EQUALS"})]),
            filter_path(1, "values"),
        ),
        (
            "the second filter has no key".to_string(),
            filters(vec![
                json!({"key": "FEATURE", "matchOption": "EQUALS", "values": ["BUDGETS"]}),
                json!({"matchOption": "EQUALS", "values": ["IAM"]}),
            ]),
            filter_path(2, "key"),
        ),
    ];

    for (case, request, path) in cases {
        let (status, body) = emulator.list(&request).await;
        let message = refusal_message(&case, status, &body);
        let requirement = "Member must not be null";
        let clause = format!("Value null at '{path}' failed to satisfy constraint: {requirement}");
        let expected = format!("1 validation error detected: {clause}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
        // The entry states the same absence and the same requirement, and names the
        // member only in its `name`.
        let entry = format!("Value null failed to satisfy constraint: {requirement}");
        let fields = field_list(&case, &body);
        assert_eq!(
            fields,
            vec![(path.clone(), entry.clone())],
            "case {case:?}: expected the entry for {path:?} to state {entry:?}, got {fields:?}"
        );
    }
}

#[tokio::test]
async fn a_request_inside_every_published_bound_is_served() {
    let emulator = Emulator::new();

    // The refusal wording is pinned above on values OUTSIDE each bound; a value ON
    // a bound is inside it, so the same requests one step in are served and the
    // rule refuses nothing it has nothing to refuse.
    let (min_results, max_results) = MAX_RESULTS_RANGE;
    let served: Vec<(String, Value)> = vec![
        (
            "a page size at the published minimum".to_string(),
            json!({"maxResults": min_results}),
        ),
        (
            "a page size at the published maximum".to_string(),
            json!({"maxResults": max_results}),
        ),
        (
            "a values list at its published minimum length".to_string(),
            filters(vec![
                json!({"key": "SEVERITY", "matchOption": "NOT_EQUALS", "values": ["CRITICAL"]}),
            ]),
        ),
        (
            "a filter whose values list holds several values".to_string(),
            filters(vec![
                json!({"key": "FEATURE", "matchOption": "EQUALS", "values": ["BUDGETS", "IAM"]}),
            ]),
        ),
        ("no member at all".to_string(), json!({})),
    ];

    for (case, request) in served {
        let (status, body) = emulator.list(&request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        assert!(
            body.get("recommendedActions").is_some(),
            "case {case:?}: expected a reported listing, got {body}"
        );
    }
}
