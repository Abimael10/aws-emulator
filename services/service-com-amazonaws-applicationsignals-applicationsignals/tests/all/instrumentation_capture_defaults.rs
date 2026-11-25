// SPDX-License-Identifier: CC0-1.0

//! One rule, over the whole capture structure: a stored instrumentation configuration carries
//! the `CaptureConfiguration` the request supplied member for member — a member the request
//! omitted is stored absent and is absent from every read that projects the record, because no
//! optional member of `CodeCaptureConfiguration` or of `CaptureLimitsConfig` is modelled with a
//! `@default` (`smithy_query(query="shape",shape="CodeCaptureConfiguration,CaptureLimitsConfig")`).
//! The "Defaults to …" sentence those members' docs carry describes what the instrumentation
//! agent captures when a member is unset; it is not a value the control plane writes into the
//! resource. The modelled counter-example is `MaxResults` on the list, which IS declared
//! `@default: 50` and is settled — the suite `defaulted_page_bound` owns that one.
//!
//! The rows differ along what the rule generalises over: WHICH members are supplied (each limit
//! alone, every limit at once, each boolean alone, each list, all of them), the VALUE a supplied
//! member carries (each published bound of each limit's own `@range`, and the value the member's
//! doc names as the capture-time default — a caller that spells it must get it back, and a
//! caller that omits it must get nothing back), and the COUNT of members supplied (none beyond
//! the required structure, one, two, all of them).
//!
//! The second test drives each limit's `@range` one step past either published bound, so a limit
//! the model does not admit is refused rather than stored.
//!
//! Every test drives the real `restJson1` surface through the router that production mounts, so
//! the routes, the status codes and the JSON bodies are the ones a client sees.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The SigV4 credential scope routes the request to this service. Signature verification is
/// off (see `set_request_authentication`), so the signature bytes are never read.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-2/application-signals/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The region in [`AUTHORIZATION`].
const REGION: &str = "eu-west-2";

/// The only `DynamicInstrumentationSignalType` the model declares
/// (`smithy_query(query="shape",shape="DynamicInstrumentationSignalType")` -> `["SNAPSHOT"]`).
const SNAPSHOT: &str = "SNAPSHOT";

/// The instrumentation types these configurations are created under
/// (`smithy_query(query="shape",shape="InstrumentationType")` -> `["BREAKPOINT", "PROBE"]`).
/// The capture rule is the same for both, so the table carries rows of each.
const BREAKPOINT: &str = "BREAKPOINT";
const PROBE: &str = "PROBE";

/// The service and environment every row of the table is created in.
const SERVICE: &str = "checkout";
const ENVIRONMENT: &str = "eks:cluster-prod/namespace";

/// One member of `CaptureLimitsConfig`: its name, the bounds its own `@range` publishes, and
/// the value its doc sentence names as the capture-time default — a value a caller may spell
/// like any other, and the one the emulator must NOT invent for an omission.
struct Limit {
    member: &'static str,
    documented: i64,
    min: i64,
    max: i64,
}

/// Every member of `CaptureLimitsConfig`, from
/// `smithy_query(query="shape",shape="CaptureLimitsConfig")`: each carries its own `@range` and
/// a doc sentence naming the value the agent captures with when the member is unset. None of
/// them carries a modelled `@default`, which is what this suite asserts the consequence of.
const LIMITS: &[Limit] = &[
    // "Defaults to 100", range 1..1000.
    Limit {
        member: "MaxHits",
        documented: 100,
        min: 1,
        max: 1000,
    },
    // "Defaults to 128", range 1..255.
    Limit {
        member: "MaxStringLength",
        documented: 128,
        min: 1,
        max: 255,
    },
    // "Defaults to 10", range 1..20.
    Limit {
        member: "MaxCollectionWidth",
        documented: 10,
        min: 1,
        max: 20,
    },
    // "Defaults to 3", range 1..5.
    Limit {
        member: "MaxCollectionDepth",
        documented: 3,
        min: 1,
        max: 5,
    },
    // "Defaults to 2", range 1..20.
    Limit {
        member: "MaxStackFrames",
        documented: 2,
        min: 1,
        max: 20,
    },
    // "Defaults to 1000", range 1..1000.
    Limit {
        member: "MaxStackTraceSize",
        documented: 1000,
        min: 1,
        max: 1000,
    },
    // "Defaults to 3", range 1..5.
    Limit {
        member: "MaxObjectDepth",
        documented: 3,
        min: 1,
        max: 5,
    },
    // "Defaults to 10", range 1..20.
    Limit {
        member: "MaxFieldsPerObject",
        documented: 10,
        min: 1,
        max: 20,
    },
];

/// A router with just this service mounted on a fresh engine.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_applicationsignals_applicationsignals::EmulatedService::new(engine),
    );
    router
}

/// Send one request and return its status and parsed JSON body (`Value::Null` when empty).
async fn post(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let payload = serde_json::to_vec(body).expect("serialize the request body");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://application-signals.{REGION}.amazonaws.com{uri}"
        ))
        .header("content-type", "application/json")
        .header("authorization", AUTHORIZATION)
        .body(bytes::Bytes::from(payload))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One row: a label, the `CodeCapture` body the create sends and the instrumentation type it
/// is created under.
struct Case {
    label: String,
    capture: Value,
    instrumentation_type: &'static str,
}

/// A `CodeCapture` body holding only `CaptureLimits`, with the named limits set.
fn limits(entries: &[(&str, i64)]) -> Value {
    let mut set = Map::new();
    for (member, value) in entries {
        set.insert((*member).to_string(), json!(value));
    }
    json!({ "CaptureLimits": Value::Object(set) })
}

/// The table the rule is asserted over.
fn cases() -> Vec<Case> {
    let mut cases = vec![Case {
        // The minimum the model admits: `CaptureLimits` is the only `@required` member of
        // `CodeCaptureConfiguration` and every member of its own target is optional, so this
        // row is the one where EVERY defaultable member is absent.
        label: "nothing beyond the required and empty CaptureLimits".to_string(),
        capture: json!({ "CaptureLimits": {} }),
        instrumentation_type: BREAKPOINT,
    }];

    // Each limit ALONE, at each published bound of its own range: the one named comes back and
    // the seven others stay absent.
    for limit in LIMITS {
        for (edge, value) in [("minimum", limit.min), ("maximum", limit.max)] {
            cases.push(Case {
                label: format!("only {} at its published {edge} ({value})", limit.member),
                capture: limits(&[(limit.member, value)]),
                instrumentation_type: BREAKPOINT,
            });
        }
        // A limit supplied at exactly the value its doc names is kept as a value the CALLER
        // chose, so a read reports that member and still reports none of its siblings.
        cases.push(Case {
            label: format!("only {} at the value its doc names", limit.member),
            capture: limits(&[(limit.member, limit.documented)]),
            instrumentation_type: PROBE,
        });
    }

    // Every limit at once, at each end of its own range.
    let all_minima: Vec<(&str, i64)> = LIMITS.iter().map(|l| (l.member, l.min)).collect();
    let all_maxima: Vec<(&str, i64)> = LIMITS.iter().map(|l| (l.member, l.max)).collect();
    cases.push(Case {
        label: "every limit at its published minimum".to_string(),
        capture: limits(&all_minima),
        instrumentation_type: BREAKPOINT,
    });
    cases.push(Case {
        label: "every limit at its published maximum".to_string(),
        capture: limits(&all_maxima),
        instrumentation_type: PROBE,
    });

    // The two booleans, each alone and each spelled both ways, including the value the doc
    // names: a boolean sent comes back, and its sibling stays absent.
    for value in [false, true] {
        cases.push(Case {
            label: format!("only CaptureReturn, sent as {value}"),
            capture: json!({ "CaptureReturn": value, "CaptureLimits": {} }),
            instrumentation_type: BREAKPOINT,
        });
        cases.push(Case {
            label: format!("only CaptureStackTrace, sent as {value}"),
            capture: json!({ "CaptureStackTrace": value, "CaptureLimits": {} }),
            instrumentation_type: PROBE,
        });
    }

    // The two lists, present, empty and absent: an omitted list comes back omitted and an
    // EMPTY one comes back empty, rather than either being turned into the other.
    cases.push(Case {
        label: "every argument captured, no locals named".to_string(),
        capture: json!({ "CaptureArguments": ["*"], "CaptureLimits": {} }),
        instrumentation_type: BREAKPOINT,
    });
    cases.push(Case {
        label: "both lists empty".to_string(),
        capture: json!({
            "CaptureArguments": [],
            "CaptureLocals": [],
            "CaptureLimits": {}
        }),
        instrumentation_type: PROBE,
    });
    cases.push(Case {
        label: "locals named, arguments omitted, two limits supplied".to_string(),
        capture: json!({
            "CaptureLocals": ["total", "discount"],
            "CaptureLimits": { "MaxHits": 25, "MaxStringLength": 120 }
        }),
        instrumentation_type: BREAKPOINT,
    });
    cases.push(Case {
        label: "every member of the structure supplied".to_string(),
        capture: json!({
            "CaptureArguments": ["cartId", "customerId"],
            "CaptureReturn": true,
            "CaptureStackTrace": false,
            "CaptureLocals": ["total"],
            "CaptureLimits": {
                "MaxHits": 7,
                "MaxStringLength": 64,
                "MaxCollectionWidth": 4,
                "MaxCollectionDepth": 2,
                "MaxStackFrames": 5,
                "MaxStackTraceSize": 512,
                "MaxObjectDepth": 4,
                "MaxFieldsPerObject": 9
            }
        }),
        instrumentation_type: PROBE,
    });

    cases
}

/// The create body for one row, at a location of its own so no two rows conflict.
fn create_body(case: &Case, index: usize) -> Value {
    json!({
        "InstrumentationType": case.instrumentation_type,
        "Service": SERVICE,
        "Environment": ENVIRONMENT,
        "SignalType": SNAPSHOT,
        "Location": { "CodeLocation": {
            "Language": "Java",
            "ClassName": "CartService",
            "MethodName": "total",
            "FilePath": "src/main/java/CartService.java",
            "LineNumber": index as i64 + 1
        }},
        "CaptureConfiguration": { "CodeCapture": case.capture }
    })
}

/// A get naming the configuration row `case` created, by its `LocationHash`.
fn get_body(case: &Case, hash: &str) -> Value {
    json!({
        "Service": SERVICE,
        "Environment": ENVIRONMENT,
        "SignalType": SNAPSHOT,
        "InstrumentationType": case.instrumentation_type,
        "LocationIdentifier": { "LocationHash": hash }
    })
}

/// The member names an object states, for a message that names the members a response invented
/// or dropped rather than only the two whole structures.
///
/// Sorted, so the comparison is of the member SETS: a request states its members in the order
/// it was written and a response in the order the schema serialises them.
pub(crate) fn member_names(value: &Value) -> Vec<String> {
    let mut names: Vec<String> = value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Assert that `reported` is the capture configuration `sent` supplied: the same members, with
/// the same values, and no member `sent` left out.
///
/// Shared with `instrumentation_capture_fidelity`, so the table here and the re-apply suite
/// state one expectation of one record.
pub(crate) fn assert_as_supplied(label: &str, where_from: &str, sent: &Value, reported: &Value) {
    let expected = json!({ "CodeCapture": sent.clone() });
    assert_eq!(
        member_names(&reported["CodeCapture"]),
        member_names(sent),
        "case {label:?}: expected {where_from} to state exactly the CodeCapture members the \
         request supplied ({:?}), got {:?}",
        member_names(sent),
        member_names(&reported["CodeCapture"])
    );
    assert_eq!(
        member_names(&reported["CodeCapture"]["CaptureLimits"]),
        member_names(&sent["CaptureLimits"]),
        "case {label:?}: expected {where_from} to state exactly the CaptureLimits members the \
         request supplied ({:?}), got {:?}",
        member_names(&sent["CaptureLimits"]),
        member_names(&reported["CodeCapture"]["CaptureLimits"])
    );
    assert_eq!(
        reported, &expected,
        "case {label:?}: expected {where_from} to report the supplied capture configuration \
         {expected}, got {reported:?}"
    );
}

/// A create stores the capture configuration the request supplied and nothing besides, and the
/// get and the list report that same structure.
#[tokio::test]
async fn an_omitted_capture_member_is_stored_and_reported_absent() {
    let router = router();
    let cases = cases();
    let mut created: Vec<(usize, String)> = Vec::new();

    for (index, case) in cases.iter().enumerate() {
        let label = &case.label;
        let body = create_body(case, index);

        let (status, answer) = post(&router, "/create-instrumentation-configuration", &body).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200 from the create, got {status} with {answer}"
        );
        assert_as_supplied(
            label,
            "the create",
            &case.capture,
            &answer["CaptureConfiguration"],
        );

        let hash = answer["LocationHash"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !hash.is_empty(),
            "case {label:?}: expected a LocationHash on the create answer, got {answer}"
        );

        // The record a read projects is the one the create wrote — twice, because a read must
        // not change what a later caller sees.
        for attempt in 1..=2 {
            let (status, fetched) = post(
                &router,
                "/get-instrumentation-configuration",
                &get_body(case, &hash),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {label:?} attempt {attempt}: expected HTTP 200 from the get, got {status} \
                 with {fetched}"
            );
            assert_as_supplied(
                label,
                &format!("the get on attempt {attempt}"),
                &case.capture,
                &fetched["Configuration"]["CaptureConfiguration"],
            );
        }

        created.push((index, hash));
    }

    // The list projects the same records, so every row appears in its scope's page carrying the
    // structure its request supplied.
    for instrumentation_type in [BREAKPOINT, PROBE] {
        let mut listed: Vec<Value> = Vec::new();
        let mut next_token: Option<String> = None;
        for _page in 0..32 {
            let mut body = json!({
                "Service": SERVICE,
                "Environment": ENVIRONMENT,
                "InstrumentationType": instrumentation_type
            });
            if let Some(token) = next_token.as_deref() {
                body["NextToken"] = json!(token);
            }
            let (status, answer) =
                post(&router, "/list-instrumentation-configurations", &body).await;
            assert_eq!(
                status, 200,
                "case {instrumentation_type:?}: expected HTTP 200 from the list, got {status} \
                 with {answer}"
            );
            listed.extend(
                answer["LatestConfigurations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
            next_token = answer["NextToken"].as_str().map(ToString::to_string);
            if next_token.is_none() {
                break;
            }
        }

        for (index, hash) in &created {
            let case = &cases[*index];
            if case.instrumentation_type != instrumentation_type {
                continue;
            }
            let entry = listed
                .iter()
                .find(|entry| entry["LocationHash"].as_str() == Some(hash.as_str()));
            let entry = entry.unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the list to report the configuration at {hash}, got {:?}",
                    case.label, listed
                )
            });
            assert_as_supplied(
                &case.label,
                "the list",
                &case.capture,
                &entry["CaptureConfiguration"],
            );
        }
    }
}

/// A limit one step outside its own published `@range` is refused with the modelled
/// `ValidationException` (400) and nothing is stored, so no record ever carries a limit the
/// model does not admit — while both published bounds are served (asserted by the table above).
#[tokio::test]
async fn a_capture_limit_outside_its_published_range_is_refused() {
    let router = router();

    for (position, limit) in LIMITS.iter().enumerate() {
        for (edge_index, (edge, value)) in [
            ("below the minimum", limit.min - 1),
            ("above the maximum", limit.max + 1),
        ]
        .into_iter()
        .enumerate()
        {
            // A location of its own per row, so the follow-up create below is the FIRST one
            // ever served at it.
            let index = position * 2 + edge_index;
            let label = format!("{} {edge} ({value})", limit.member);
            let refused = Case {
                label: label.clone(),
                capture: limits(&[(limit.member, value)]),
                instrumentation_type: BREAKPOINT,
            };
            let (status, error) = post(
                &router,
                "/create-instrumentation-configuration",
                &create_body(&refused, index),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {label:?}: expected HTTP 400 for a limit outside the modelled range, got \
                 {status} with {error}"
            );
            assert_eq!(
                error["__type"]
                    .as_str()
                    .map(|kind| kind.rsplit('#').next().unwrap_or(kind).to_string()),
                Some("ValidationException".to_string()),
                "case {label:?}: expected a ValidationException, got {error}"
            );

            // The refused create stored nothing, so a create at that same location is served
            // rather than refused as a conflict — and it carries what it supplied.
            let accepted = Case {
                label: label.clone(),
                capture: limits(&[(limit.member, limit.min)]),
                instrumentation_type: BREAKPOINT,
            };
            let (status, answer) = post(
                &router,
                "/create-instrumentation-configuration",
                &create_body(&accepted, index),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected the refused create to have stored nothing, so the next \
                 create at that location answers 200, got {status} with {answer}"
            );
            assert_as_supplied(
                &label,
                "the create",
                &accepted.capture,
                &answer["CaptureConfiguration"],
            );
        }
    }
}
