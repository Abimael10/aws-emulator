// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH SERIES a client reads one invocation's metrics under when the
//! call addressed a qualifier — read back where a client reads them, the `AWS/Lambda` series
//! CloudWatch answers for, with CloudWatch mounted beside Lambda on one engine.
//!
//! The RULE asserted over the tables below: every `AWS/Lambda` series about one invocation is
//! published once in EACH dimension set that invocation belongs to — the function
//! (`FunctionName`), the resource the call addressed (`FunctionName` plus `Resource`, the
//! function itself for an unqualified call and `name:qualifier` for an alias or a published
//! version) and, for a call that addressed a QUALIFIER, the version that actually ran
//! (`FunctionName` plus `ExecutedVersion` plus `Resource`). `ExecutedVersion` is "the version
//! of the function that executed. When you invoke a function with an alias, this indicates
//! which version the alias resolved to" (`Invoke`), so that third set is how the traffic of an
//! alias is attributed to the version that served each invocation — and an unqualified call,
//! whose executed version is the unpublished version the function is addressed by, publishes
//! only the first two.
//!
//! The rows differ along what the rule generalises over: the qualifier form (none at all, an
//! explicit `$LATEST`, a published version through the `Qualifier` parameter, an alias through
//! the `name:qualifier` form of `FunctionName`, an alias through the `Qualifier` parameter),
//! WHICH version the alias resolves to (two published versions with an alias each), how many
//! invocations are made, and a mixture split across three resources of one function. A second
//! test drives a WEIGHTED alias, where the version that runs differs per invocation, and
//! compares each `ExecutedVersion` series against the versions the answers themselves reported.
//!
//! Every metric, dimension and unit name here is written as a literal, taken from the AWS
//! Lambda metric reference rather than from this crate's constants, so a series published under
//! a name no AWS reader would ask for fails here instead of agreeing with itself. The
//! Lambda-side harness (`send`, `Answer`, `CreateFunction` and the activation window) is
//! `function_lifecycle.rs`'s, the runnable deployment archive is `synchronous_invoke_run.rs`'s,
//! and the CloudWatch call is `kinesis_event_source_delivery.rs`'s.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_active_function, json_call, make_activation_due, package_bytes, send, Answer, ROLE,
};
use crate::kinesis_event_source_delivery::cloudwatch;
use crate::synchronous_invoke_run::package;

/// The namespace Lambda's own metrics live in (the AWS Lambda metric reference).
const NAMESPACE: &str = "AWS/Lambda";

/// The names that reference gives the series under test, and the units it reports them in.
const INVOCATIONS_SERIES: &str = "Invocations";
/// See [`INVOCATIONS_SERIES`].
const DURATION_SERIES: &str = "Duration";
/// See [`INVOCATIONS_SERIES`].
const ERRORS_SERIES: &str = "Errors";
/// See [`INVOCATIONS_SERIES`].
const COUNT_UNIT: &str = "Count";
/// See [`INVOCATIONS_SERIES`].
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The dimensions that reference reports a function's metrics per: the function, the resource
/// of it an invocation addressed, and the version that executed.
const FUNCTION_DIMENSION: &str = "FunctionName";
/// See [`FUNCTION_DIMENSION`].
const RESOURCE_DIMENSION: &str = "Resource";
/// See [`FUNCTION_DIMENSION`].
const EXECUTED_VERSION_DIMENSION: &str = "ExecutedVersion";

/// The function every row invokes.
const FUNCTION: &str = "attributed";

/// The aliases every row can address, and the published versions they name.
const BLUE: &str = "blue";
/// See [`BLUE`].
const GREEN: &str = "green";
/// See [`BLUE`].
const FIRST_VERSION: &str = "1";
/// See [`BLUE`].
const SECOND_VERSION: &str = "2";

/// Where `CreateFunction`, `PublishVersion`, `CreateAlias` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The header `Invoke$ExecutedVersion` is bound to: "The version of the function that
/// executed."
const EXECUTED_VERSION_HEADER: &str = "x-amz-executed-version";

/// The `Period`, in seconds, every query reads at: the one-minute granularity Lambda's own
/// series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a query's window reaches, in seconds. Wide enough that the
/// minutes the invocations were published in are inside it however a row straddles a minute
/// boundary.
const WINDOW_SECS: f64 = 600.0;

/// The status this suite's unreadable package answers a synchronous invocation with: its bytes
/// are not an archive ("Lambda could not unzip the deployment package"), which is past the
/// point an invocation is metered at, so the counting rows need no handler to run.
const RUN_NOT_IMPLEMENTED: u16 = 502;

/// The `Timeout` the weighted-alias function is given, in seconds.
///
/// Generous on purpose: no assertion is about it, and a handler stopped at this deadline would
/// be reported as a run that errored, so a tight value would turn a slow or loaded host into a
/// failure the handler never raised. Well inside the range the model allows the member
/// (`smithy_query(query="shape", shape="Timeout")`: 1 to 5400).
const GENEROUS_TIMEOUT_SECS: u32 = 300;

/// The handler the weighted-alias test runs: it returns, so every invocation of it is a run
/// that measured a duration and failed nothing.
const SOURCE: &str = r#"
def lambda_handler(event, context):
    return {"ran": True}
"#;

/// Lambda and CloudWatch on one engine: the series Lambda publishes is CloudWatch's, so it is
/// read in-process through CloudWatch's own surface — the shape `core/router/src/lib.rs`
/// mounts them in.
fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
    router
}

/// Now, in the epoch seconds a stored datapoint is stamped with.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// One invocation: the `FunctionName` label the call carries, and the query string with it.
#[derive(Clone, Copy, Debug)]
struct Call {
    /// The `FunctionName` path label, which may carry the `name:qualifier` form.
    reference: &'static str,
    /// The query string, which may carry the `Qualifier` parameter.
    query: &'static str,
    /// How many of these calls the row makes.
    times: u32,
}

/// One row: the calls it makes, and what each series must then report.
struct Row {
    case: &'static str,
    calls: &'static [Call],
    /// What the series dimensioned on the function alone must sum to.
    function_sum: f64,
    /// What the series of each addressed resource must sum to: `(Resource, sum)`.
    resource_sums: &'static [(&'static str, f64)],
    /// What the series of each executed version of a resource must sum to:
    /// `(ExecutedVersion, Resource, sum)`. A resource that is not here must have no such
    /// series at all.
    executed_version_sums: &'static [(&'static str, &'static str, f64)],
}

/// The rows. Every expected count is written out rather than computed from the calls, so a row
/// published under one dimension set and a row published under three cannot both pass.
const ROWS: &[Row] = &[
    Row {
        case: "an unqualified invocation, whose executed version the function's own series is",
        calls: &[Call {
            reference: FUNCTION,
            query: "",
            times: 2,
        }],
        function_sum: 2.0,
        resource_sums: &[("attributed", 2.0)],
        executed_version_sums: &[],
    },
    Row {
        case: "an explicit $LATEST qualifier, which addresses the function itself",
        calls: &[Call {
            reference: FUNCTION,
            query: "?Qualifier=$LATEST",
            times: 1,
        }],
        function_sum: 1.0,
        resource_sums: &[("attributed", 1.0)],
        executed_version_sums: &[],
    },
    Row {
        case: "a published version addressed by the Qualifier parameter",
        calls: &[Call {
            reference: FUNCTION,
            query: "?Qualifier=1",
            times: 3,
        }],
        function_sum: 3.0,
        resource_sums: &[("attributed:1", 3.0)],
        executed_version_sums: &[("1", "attributed:1", 3.0)],
    },
    Row {
        case: "an alias on the first version, addressed in the name:qualifier form",
        calls: &[Call {
            reference: "attributed:blue",
            query: "",
            times: 2,
        }],
        function_sum: 2.0,
        resource_sums: &[("attributed:blue", 2.0)],
        executed_version_sums: &[("1", "attributed:blue", 2.0)],
    },
    Row {
        case: "an alias on the second version, addressed by the Qualifier parameter",
        calls: &[Call {
            reference: FUNCTION,
            query: "?Qualifier=green",
            times: 2,
        }],
        function_sum: 2.0,
        resource_sums: &[("attributed:green", 2.0)],
        executed_version_sums: &[("2", "attributed:green", 2.0)],
    },
    Row {
        case: "invocations split across two aliases of different versions and the function",
        calls: &[
            Call {
                reference: "attributed:blue",
                query: "",
                times: 1,
            },
            Call {
                reference: FUNCTION,
                query: "?Qualifier=green",
                times: 3,
            },
            Call {
                reference: FUNCTION,
                query: "",
                times: 2,
            },
        ],
        function_sum: 6.0,
        resource_sums: &[
            ("attributed", 2.0),
            ("attributed:blue", 1.0),
            ("attributed:green", 3.0),
        ],
        executed_version_sums: &[
            ("1", "attributed:blue", 1.0),
            ("2", "attributed:green", 3.0),
        ],
    },
    Row {
        case: "an alias and the version it resolves to, which are two resources of one version",
        calls: &[
            Call {
                reference: "attributed:blue",
                query: "",
                times: 2,
            },
            Call {
                reference: FUNCTION,
                query: "?Qualifier=1",
                times: 1,
            },
        ],
        function_sum: 3.0,
        resource_sums: &[("attributed:blue", 2.0), ("attributed:1", 1.0)],
        executed_version_sums: &[("1", "attributed:blue", 2.0), ("1", "attributed:1", 1.0)],
    },
];

/// `PublishVersion`, with a description of its own so each call publishes a new version rather
/// than answering the last one.
async fn publish_version(router: &MultiServiceRouter, case: &str, ordinal: u32) {
    let answer = json_call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/versions"),
        &json!({ "Description": format!("snapshot {ordinal}") }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: PublishVersion {ordinal} must succeed, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// `CreateAlias` on one published version, with the routing configuration the caller hands it.
async fn create_alias(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    version: &str,
    routing: Option<Value>,
) {
    let mut body = json!({ "Name": name, "FunctionVersion": version });
    if let Some(routing) = routing {
        body["RoutingConfig"] = routing;
    }
    let answer = json_call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/aliases"),
        &body,
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: CreateAlias {name:?} must succeed, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// The function every counting row invokes: two published versions, and an alias on each.
async fn create_function_with_two_aliased_versions(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
) {
    create_active_function(router, engine, FUNCTION, &package_bytes(64)).await;
    publish_version(router, case, 1).await;
    publish_version(router, case, 2).await;
    create_alias(router, case, BLUE, FIRST_VERSION, None).await;
    create_alias(router, case, GREEN, SECOND_VERSION, None).await;
}

/// One `Invoke` of the resource `call` addresses.
async fn invoke(router: &MultiServiceRouter, call: &Call) -> Answer {
    send(
        router,
        "POST",
        &format!(
            "{FUNCTIONS_URI}/{}/invocations{}",
            call.reference, call.query
        ),
        "application/json",
        b"{}".to_vec(),
        &[],
    )
    .await
}

/// The dimension set of the series reported per function.
fn function_dimensions() -> Value {
    json!([{ "Name": FUNCTION_DIMENSION, "Value": FUNCTION }])
}

/// The dimension set of the series reported per invoked resource.
fn resource_dimensions(resource: &str) -> Value {
    json!([
        { "Name": FUNCTION_DIMENSION, "Value": FUNCTION },
        { "Name": RESOURCE_DIMENSION, "Value": resource },
    ])
}

/// The dimension set of the series reported per version that executed one resource's traffic.
fn executed_version_dimensions(executed_version: &str, resource: &str) -> Value {
    json!([
        { "Name": FUNCTION_DIMENSION, "Value": FUNCTION },
        { "Name": EXECUTED_VERSION_DIMENSION, "Value": executed_version },
        { "Name": RESOURCE_DIMENSION, "Value": resource },
    ])
}

/// The `Datapoints` `GetMetricStatistics` answers for the series `metric` and `dimensions`
/// identify, selected by the unit the AWS Lambda metric reference gives that series.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    unit: &str,
    dimensions: &Value,
) -> Vec<Value> {
    let now = now_secs();
    let (status, stats) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount", "Maximum"],
            "Unit": unit,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics({metric}, {dimensions}) answered {status} {stats}"
    );
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The total of one statistic over every datapoint of a series.
fn total(points: &[Value], statistic: &str) -> f64 {
    points
        .iter()
        .filter_map(|point| point[statistic].as_f64())
        .sum()
}

/// Every series `ListMetrics` discovers in [`NAMESPACE`], as `(MetricName, dimension pairs)`
/// with the dimensions ordered by name — which is how a client enumerating what it can alarm
/// on reads them.
async fn listed_series(
    router: &MultiServiceRouter,
    case: &str,
) -> Vec<(String, Vec<(String, String)>)> {
    let (status, listed) =
        cloudwatch(router, "ListMetrics", &json!({ "Namespace": NAMESPACE })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListMetrics answered {status} {listed}"
    );
    listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|series| {
            let mut dimensions: Vec<(String, String)> = series["Dimensions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|dimension| {
                    (
                        dimension["Name"].as_str().unwrap_or_default().to_string(),
                        dimension["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            dimensions.sort();
            (
                series["MetricName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                dimensions,
            )
        })
        .collect()
}

/// Every `(ExecutedVersion, Resource)` pair the [`INVOCATIONS_SERIES`] series are discoverable
/// under, which is what tells "no such series" from "a series reading nothing".
async fn listed_executed_version_pairs(
    router: &MultiServiceRouter,
    case: &str,
) -> Vec<(String, String)> {
    listed_series(router, case)
        .await
        .into_iter()
        .filter(|(metric, dimensions)| {
            metric == INVOCATIONS_SERIES
                && dimensions
                    .iter()
                    .any(|(name, _)| name == EXECUTED_VERSION_DIMENSION)
        })
        .filter_map(|(_, dimensions)| {
            let executed_version = dimensions
                .iter()
                .find(|(name, _)| name == EXECUTED_VERSION_DIMENSION)
                .map(|(_, value)| value.clone())?;
            let resource = dimensions
                .iter()
                .find(|(name, _)| name == RESOURCE_DIMENSION)
                .map(|(_, value)| value.clone())?;
            Some((executed_version, resource))
        })
        .collect()
}

/// RULE: a qualified invocation's series are published in the dimension set that names the
/// version which RAN, beside the function's own and the invoked resource's; an unqualified
/// invocation publishes no such set at all.
#[tokio::test]
async fn a_qualified_invocation_is_also_metered_under_the_version_that_ran() {
    for row in ROWS {
        // One account per row, so the only invocations in the window are this row's.
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);
        let case = row.case;
        create_function_with_two_aliased_versions(&router, &engine, case).await;

        for call in row.calls {
            for nth in 1..=call.times {
                let answer = invoke(&router, call).await;
                assert_eq!(
                    answer.status,
                    RUN_NOT_IMPLEMENTED,
                    "case {case:?}: call {nth} of {call:?} must answer {RUN_NOT_IMPLEMENTED}, \
                     got {} {}",
                    answer.status,
                    String::from_utf8_lossy(&answer.body)
                );
            }
        }

        // The function's own series carries every invocation of it, whichever resource each one
        // addressed and whichever version ran.
        let points = datapoints(
            &router,
            case,
            INVOCATIONS_SERIES,
            COUNT_UNIT,
            &function_dimensions(),
        )
        .await;
        let sum = total(&points, "Sum");
        assert_eq!(
            sum, row.function_sum,
            "case {case:?}: {INVOCATIONS_SERIES} of the function must sum to {}, got {sum} over \
             {points:?}",
            row.function_sum
        );

        // Each addressed resource's series carries the invocations of that resource.
        for (resource, expected) in row.resource_sums {
            let points = datapoints(
                &router,
                case,
                INVOCATIONS_SERIES,
                COUNT_UNIT,
                &resource_dimensions(resource),
            )
            .await;
            let resource_sum = total(&points, "Sum");
            assert_eq!(
                resource_sum, *expected,
                "case {case:?}: {INVOCATIONS_SERIES} of {resource:?} must sum to {expected}, got \
                 {resource_sum} over {points:?}"
            );
        }

        // And each resource's traffic is attributed to the version that served it.
        for (executed_version, resource, expected) in row.executed_version_sums {
            let dimensions = executed_version_dimensions(executed_version, resource);
            let points =
                datapoints(&router, case, INVOCATIONS_SERIES, COUNT_UNIT, &dimensions).await;
            let attributed = total(&points, "Sum");
            assert_eq!(
                attributed, *expected,
                "case {case:?}: {INVOCATIONS_SERIES} of {resource:?} at version \
                 {executed_version:?} must sum to {expected}, got {attributed} over {points:?}"
            );
            assert!(
                points
                    .iter()
                    .all(|point| point["Unit"].as_str() == Some(COUNT_UNIT)),
                "case {case:?}: every datapoint of {dimensions} must carry the unit \
                 {COUNT_UNIT:?}, got {points:?}"
            );
        }

        // No OTHER executed-version series exists: an unqualified row has none at all, and a
        // qualified one has exactly those of the versions that ran its resources.
        let mut discovered = listed_executed_version_pairs(&router, case).await;
        discovered.sort();
        let mut expected: Vec<(String, String)> = row
            .executed_version_sums
            .iter()
            .map(|(executed_version, resource, _)| {
                ((*executed_version).to_string(), (*resource).to_string())
            })
            .collect();
        expected.sort();
        assert_eq!(
            discovered, expected,
            "case {case:?}: the {INVOCATIONS_SERIES} series carrying \
             {EXECUTED_VERSION_DIMENSION} must be exactly {expected:?}, got {discovered:?}"
        );

        // Reading the series changes nothing a later reader sees.
        let again = total(
            &datapoints(
                &router,
                case,
                INVOCATIONS_SERIES,
                COUNT_UNIT,
                &function_dimensions(),
            )
            .await,
            "Sum",
        );
        assert_eq!(
            again, sum,
            "case {case:?}: a second read of {INVOCATIONS_SERIES} must report {sum}, got {again}"
        );
    }
}

/// How many invocations the weighted-alias test makes through the split alias.
const WEIGHTED_INVOCATIONS: u32 = 6;

/// RULE, where it earns its keep: an alias whose `RoutingConfig` splits its traffic resolves
/// per invocation, and each invocation's series land under the version that actually ran — so
/// the `ExecutedVersion` sets of one alias add up to its traffic and each one holds exactly the
/// invocations whose answer reported that version.
#[tokio::test]
async fn a_weighted_alias_attributes_every_series_to_the_version_that_ran() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let case = "a weighted alias over two published versions";

    // A function whose handler RUNS, so the series about the run — the duration it took and
    // the outcome it came to — are published beside the count.
    let created = send(
        &router,
        "POST",
        FUNCTIONS_URI,
        "application/json",
        serde_json::to_vec(&json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": GENEROUS_TIMEOUT_SECS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(SOURCE)),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction must succeed, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&engine);
    publish_version(&router, case, 1).await;
    publish_version(&router, case, 2).await;
    // "The percentage of invocation requests" the second version receives: half of them, so
    // which version runs differs per invocation and neither is a foregone conclusion.
    let mut weights = serde_json::Map::new();
    weights.insert(SECOND_VERSION.to_string(), json!(0.5));
    create_alias(
        &router,
        case,
        BLUE,
        FIRST_VERSION,
        Some(json!({ "AdditionalVersionWeights": Value::Object(weights) })),
    )
    .await;

    // Which version each invocation reported having executed, counted from the answers
    // themselves — never from the series under test.
    let mut ran: BTreeMap<String, f64> = BTreeMap::new();
    for nth in 1..=WEIGHTED_INVOCATIONS {
        let answer = invoke(
            &router,
            &Call {
                reference: "attributed:blue",
                query: "",
                times: 1,
            },
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: invocation {nth} must answer 200, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let executed_version = answer.header(EXECUTED_VERSION_HEADER).unwrap_or_else(|| {
            panic!("case {case:?}: invocation {nth} must report the version that executed")
        });
        *ran.entry(executed_version).or_insert(0.0) += 1.0;
    }
    let total_invocations = f64::from(WEIGHTED_INVOCATIONS);
    assert_eq!(
        ran.values().sum::<f64>(),
        total_invocations,
        "case {case:?}: every invocation must report a version, got {ran:?}"
    );

    // The alias's own series carries all of its traffic...
    let alias_resource = format!("{FUNCTION}:{BLUE}");
    let alias_points = datapoints(
        &router,
        case,
        INVOCATIONS_SERIES,
        COUNT_UNIT,
        &resource_dimensions(&alias_resource),
    )
    .await;
    let alias_sum = total(&alias_points, "Sum");
    assert_eq!(
        alias_sum, total_invocations,
        "case {case:?}: {INVOCATIONS_SERIES} of {alias_resource:?} must sum to \
         {total_invocations}, got {alias_sum} over {alias_points:?}"
    );

    // ... and each version's share of it is exactly what that version's answers reported, on
    // every series the invocation is metered on.
    let mut attributed_total = 0.0;
    for version in [FIRST_VERSION, SECOND_VERSION] {
        let expected = ran.get(version).copied().unwrap_or(0.0);
        let dimensions = executed_version_dimensions(version, &alias_resource);
        let counted = total(
            &datapoints(&router, case, INVOCATIONS_SERIES, COUNT_UNIT, &dimensions).await,
            "Sum",
        );
        assert_eq!(
            counted, expected,
            "case {case:?}: {INVOCATIONS_SERIES} of {alias_resource:?} at version {version:?} \
             must sum to the {expected} invocations the answers attributed to it, got {counted}"
        );
        attributed_total += counted;

        // `Errors` is a binary indicator of each invocation's outcome, so the version's share
        // is its SampleCount; `Duration` measures each run, so it holds one sample per run.
        let errors =
            datapoints(&router, case, ERRORS_SERIES, COUNT_UNIT, &dimensions).await;
        assert_eq!(
            total(&errors, "SampleCount"),
            expected,
            "case {case:?}: {ERRORS_SERIES} of {alias_resource:?} at version {version:?} must \
             report {expected} outcomes, got {errors:?}"
        );
        assert_eq!(
            total(&errors, "Sum"),
            0.0,
            "case {case:?}: no run failed, so {ERRORS_SERIES} of {alias_resource:?} at version \
             {version:?} must sum to 0, got {errors:?}"
        );
        let durations = datapoints(
            &router,
            case,
            DURATION_SERIES,
            MILLISECONDS_UNIT,
            &dimensions,
        )
        .await;
        assert_eq!(
            total(&durations, "SampleCount"),
            expected,
            "case {case:?}: {DURATION_SERIES} of {alias_resource:?} at version {version:?} must \
             report {expected} runs, got {durations:?}"
        );
    }
    assert_eq!(
        attributed_total, total_invocations,
        "case {case:?}: the versions' shares must add up to the alias's {total_invocations} \
         invocations, got {attributed_total} over {ran:?}"
    );
}
