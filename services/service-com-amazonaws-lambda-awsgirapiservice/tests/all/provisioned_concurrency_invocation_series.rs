// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the series a client reads to learn whether the PROVISIONED CONCURRENCY
//! it pays for is being used — `ProvisionedConcurrencyInvocations`,
//! `ProvisionedConcurrencySpilloverInvocations` and `ProvisionedConcurrencyUtilization` — read
//! back where a client reads them, the `AWS/Lambda` series CloudWatch answers for, with
//! CloudWatch mounted beside Lambda on one engine.
//!
//! The RULE asserted over the table below: provisioned concurrency "initializes a requested
//! number of execution environments so that they are prepared to respond immediately to your
//! function's invocations", allocated per alias or version
//! (`PutProvisionedConcurrencyConfig`), so every invocation of a qualifier whose allocation is
//! READY falls on one side of a split and is metered on it —
//! `ProvisionedConcurrencyInvocations` for one of those environments served it,
//! `ProvisionedConcurrencySpilloverInvocations` for one served on demand because they were all
//! busy — beside `ProvisionedConcurrencyUtilization`, "the fraction of the allocated
//! provisioned concurrency that is in use", which is a gauge and carries no unit of its own. A
//! qualifier with no ready allocation has NO series of this family at all, which is what tells
//! "this alias is not provisioned" from "its pool served nothing".
//!
//! The rows differ along what the rule generalises over: how much is allocated (one
//! environment, two, three), how many of those environments are already serving an invocation
//! when the row invokes (none, one, all of them), how many invocations are made, which resource
//! the invocations address (the alias the allocation is on, the version it resolves to, the
//! unqualified function) and whether an allocation exists at all.
//!
//! Every metric, dimension and unit name here is written as a literal, taken from the AWS
//! Lambda metric reference rather than from this crate's constants, so a series published under
//! a name no AWS reader would ask for fails here instead of agreeing with itself. The
//! Lambda-side harness (`send`, `Answer`, `CreateFunction` and the activation window) is
//! `function_lifecycle.rs`'s, and the CloudWatch call is `kinesis_event_source_delivery.rs`'s.
//!
//! Nothing here waits on the clock: the allocation's warm-up and the function's activation are
//! both owed by it, so this suite moves each due instant into the past exactly as
//! `function_lifecycle.rs` moves the activation — the same thing waiting would do, without
//! waiting.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{create_active_function, json_call, package_bytes, send, Answer};
use crate::kinesis_event_source_delivery::cloudwatch;

/// The namespace Lambda's own metrics live in (the AWS Lambda metric reference).
const NAMESPACE: &str = "AWS/Lambda";

/// The names that reference gives the provisioned-concurrency series under test, and the units
/// it reports them in.
const POOL_SERIES: &str = "ProvisionedConcurrencyInvocations";
/// See [`POOL_SERIES`].
const SPILLOVER_SERIES: &str = "ProvisionedConcurrencySpilloverInvocations";
/// See [`POOL_SERIES`].
const UTILIZATION_SERIES: &str = "ProvisionedConcurrencyUtilization";
/// See [`POOL_SERIES`].
const INVOCATIONS_SERIES: &str = "Invocations";
/// See [`POOL_SERIES`].
const COUNT_UNIT: &str = "Count";
/// The unit a FRACTION carries, which CloudWatch's `StandardUnit` spells `None`
/// (`smithy_query(query="shape", shape="StandardUnit")`).
const NO_UNIT: &str = "None";

/// The dimensions that reference reports a function's metrics per.
const FUNCTION_DIMENSION: &str = "FunctionName";
/// See [`FUNCTION_DIMENSION`].
const RESOURCE_DIMENSION: &str = "Resource";

/// The function every row invokes.
const FUNCTION: &str = "provisioned";

/// The alias every allocation is made on, and the published version it names.
const ALIAS: &str = "live";
/// See [`ALIAS`].
const VERSION: &str = "1";

/// Where `CreateFunction`, `PublishVersion`, `CreateAlias` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `PutProvisionedConcurrencyConfig` is bound
/// (`PUT /2019-09-30/functions/{FunctionName}/provisioned-concurrency?Qualifier=`).
const PROVISIONED_CONCURRENCY_URI: &str = "/2019-09-30/functions";

/// The `ProvisionedConcurrencyStatusEnum` value a completed allocation carries
/// (`smithy_query(query="shape", shape="ProvisionedConcurrencyStatusEnum")`).
const READY: &str = "READY";

/// The `Period`, in seconds, every query reads at: the one-minute granularity Lambda's own
/// series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a query's window reaches, in seconds.
const WINDOW_SECS: f64 = 600.0;

/// The status this suite's unreadable package answers a synchronous invocation with: its bytes
/// are not an archive ("Lambda could not unzip the deployment package"), which is past the
/// point an invocation is admitted and metered at, so no handler has to run for the admission's
/// own series to exist.
const RUN_NOT_IMPLEMENTED: u16 = 502;

/// Which resource of the function one invocation addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The alias the allocation is made on.
    Alias,
    /// The published version that alias resolves to, which holds no allocation of its own.
    Version,
    /// The function itself, with no qualifier at all.
    Function,
}

impl Target {
    /// The query string a call to this target carries.
    fn query(self) -> &'static str {
        match self {
            Self::Alias => "?Qualifier=live",
            Self::Version => "?Qualifier=1",
            Self::Function => "",
        }
    }

    /// The `Resource` dimension value the series of this target carry.
    fn resource(self) -> String {
        match self {
            Self::Alias => format!("{FUNCTION}:{ALIAS}"),
            Self::Version => format!("{FUNCTION}:{VERSION}"),
            Self::Function => FUNCTION.to_string(),
        }
    }
}

/// One row: what is allocated, what is already using it, what the row invokes, and what each
/// series must then report. `None` is NO DATAPOINT AT ALL, which is a different reading from a
/// zero.
struct Row {
    case: &'static str,
    /// What `PutProvisionedConcurrencyConfig` allocates on the alias, or `None` for a row that
    /// allocates nothing.
    allocated: Option<i64>,
    /// How many of the allocated environments are already serving an invocation when the row
    /// invokes — what a concurrent caller of the same qualifier leaves behind.
    occupied: i32,
    /// The resource each invocation addresses, one invocation per entry.
    invokes: &'static [Target],
    /// What [`POOL_SERIES`] of the ALIAS must sum to.
    pool_sum: Option<f64>,
    /// What [`SPILLOVER_SERIES`] of the alias must sum to.
    spillover_sum: Option<f64>,
    /// The greatest [`UTILIZATION_SERIES`] reading of the alias.
    utilization_maximum: Option<f64>,
}

/// The rows. Every expectation is written out rather than computed from the allocation, so a
/// row that reports the split and one that reports a bare count cannot both pass.
const ROWS: &[Row] = &[
    Row {
        case: "two sequential invocations inside a one-environment allocation",
        allocated: Some(1),
        occupied: 0,
        invokes: &[Target::Alias, Target::Alias],
        pool_sum: Some(2.0),
        spillover_sum: None,
        utilization_maximum: Some(1.0),
    },
    Row {
        case: "three sequential invocations inside a three-environment allocation",
        allocated: Some(3),
        occupied: 0,
        invokes: &[Target::Alias, Target::Alias, Target::Alias],
        pool_sum: Some(3.0),
        spillover_sum: None,
        // One of the three environments serves each sequential invocation.
        utilization_maximum: Some(1.0 / 3.0),
    },
    Row {
        case: "an allocation whose only environment is already busy, so both invocations spill",
        allocated: Some(1),
        occupied: 1,
        invokes: &[Target::Alias, Target::Alias],
        pool_sum: None,
        spillover_sum: Some(2.0),
        // The allocation is fully in use either way: a spillover uses none of it.
        utilization_maximum: Some(1.0),
    },
    Row {
        case: "a two-environment allocation with one of them busy, so the invocation fits",
        allocated: Some(2),
        occupied: 1,
        invokes: &[Target::Alias],
        pool_sum: Some(1.0),
        spillover_sum: None,
        utilization_maximum: Some(1.0),
    },
    Row {
        case: "a three-environment allocation with two busy, so the invocation still fits",
        allocated: Some(3),
        occupied: 2,
        invokes: &[Target::Alias],
        pool_sum: Some(1.0),
        spillover_sum: None,
        utilization_maximum: Some(1.0),
    },
    Row {
        case: "no allocation at all, so the family has no series",
        allocated: None,
        occupied: 0,
        invokes: &[Target::Alias, Target::Version, Target::Function],
        pool_sum: None,
        spillover_sum: None,
        utilization_maximum: None,
    },
    Row {
        case: "the version the provisioned alias resolves to, which holds no allocation",
        allocated: Some(2),
        occupied: 0,
        invokes: &[Target::Version, Target::Version, Target::Function],
        pool_sum: None,
        spillover_sum: None,
        utilization_maximum: None,
    },
];

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

/// Create the function, publish [`VERSION`] of it and give it the alias [`ALIAS`], so an
/// allocation can be made on a qualifier and the version behind it addressed separately.
async fn create_function_with_alias(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
) {
    create_active_function(router, engine, FUNCTION, &package_bytes(64)).await;
    let published = json_call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/versions"),
        &json!({}),
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "case {case:?}: PublishVersion must succeed, got {} {}",
        published.status,
        String::from_utf8_lossy(&published.body)
    );
    let alias = json_call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/aliases"),
        &json!({ "Name": ALIAS, "FunctionVersion": VERSION }),
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "case {case:?}: CreateAlias must succeed, got {} {}",
        alias.status,
        String::from_utf8_lossy(&alias.body)
    );
}

/// `PutProvisionedConcurrencyConfig` on the alias, then the READY status the clock owes it:
/// the warm-up instant is moved into the past, which is what waiting it out would do.
async fn allocate_provisioned_concurrency(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    requested: i64,
) {
    let answer = json_call(
        router,
        "PUT",
        &format!(
            "{PROVISIONED_CONCURRENCY_URI}/{FUNCTION}/provisioned-concurrency?Qualifier={ALIAS}"
        ),
        &json!({ "ProvisionedConcurrentExecutions": requested }),
    )
    .await;
    assert_eq!(
        answer.status,
        202,
        "case {case:?}: PutProvisionedConcurrencyConfig({requested}) must be accepted, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    {
        let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
        let mut store = state.lock();
        let past = SystemTime::now() - Duration::from_secs(1);
        for function in store.functions.values_mut() {
            for config in function.provisioned_concurrency_configs.values_mut() {
                config.ready_due = Some(past);
            }
        }
    }
    // The allocation reports itself ready, which is the state every row's invocations are
    // admitted against.
    let read = json_call(
        router,
        "GET",
        &format!(
            "{PROVISIONED_CONCURRENCY_URI}/{FUNCTION}/provisioned-concurrency?Qualifier={ALIAS}"
        ),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetProvisionedConcurrencyConfig must succeed, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    let document = read.json();
    assert_eq!(
        document["Status"].as_str(),
        Some(READY),
        "case {case:?}: the allocation must be {READY} before the row invokes, got {document}"
    );
    assert_eq!(
        document["AllocatedProvisionedConcurrentExecutions"].as_i64(),
        Some(requested),
        "case {case:?}: the allocation must report {requested} allocated, got {document}"
    );
}

/// Leave `count` of the alias's provisioned environments SERVING an invocation, which is what a
/// concurrent caller of the same qualifier leaves behind: the per-qualifier in-flight count the
/// admission reads is what decides whether the next invocation fits inside the allocation.
fn occupy_provisioned_environments(engine: &Arc<engine::InMemoryEngine>, count: i32) {
    if count <= 0 {
        return;
    }
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    for function in store.functions.values_mut() {
        function
            .in_flight_by_qualifier
            .insert(ALIAS.to_string(), count);
        function.concurrent_executions = function.concurrent_executions.saturating_add(count);
    }
}

/// One `Invoke` of the resource `target` addresses.
async fn invoke(router: &MultiServiceRouter, target: Target) -> Answer {
    send(
        router,
        "POST",
        &format!(
            "{FUNCTIONS_URI}/{FUNCTION}/invocations{}",
            target.query()
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
            "Statistics": ["Sum", "SampleCount", "Maximum", "Minimum"],
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

/// The greatest `Maximum` any datapoint of a series reports, or `None` for a series with no
/// datapoint at all.
fn greatest(points: &[Value]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point["Maximum"].as_f64())
        .max_by(f64::total_cmp)
}

/// Assert what one count series of the family reports: a sum, or no datapoint at all.
fn assert_count(case: &str, metric: &str, points: &[Value], expected: Option<f64>) {
    match expected {
        None => assert!(
            points.is_empty(),
            "case {case:?}: {metric} must have no datapoint at all, got {points:?}"
        ),
        Some(expected) => {
            let sum = total(points, "Sum");
            assert_eq!(
                sum, expected,
                "case {case:?}: {metric} must sum to {expected}, got {sum} over {points:?}"
            );
            assert!(
                points
                    .iter()
                    .all(|point| point["Unit"].as_str() == Some(COUNT_UNIT)),
                "case {case:?}: every datapoint of {metric} must carry the unit {COUNT_UNIT:?}, \
                 got {points:?}"
            );
        }
    }
}

/// Every metric name `ListMetrics` discovers in [`NAMESPACE`] for one dimension set, which is
/// how a client finds what it can alarm or chart on.
async fn listed_metrics_for(
    router: &MultiServiceRouter,
    case: &str,
    dimensions: &[(&str, String)],
) -> Vec<String> {
    let wanted: Vec<(String, String)> = dimensions
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect();
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
        .filter_map(|series| {
            let mut found: Vec<(String, String)> = series["Dimensions"]
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
            found.sort();
            let mut expected = wanted.clone();
            expected.sort();
            (found == expected).then(|| {
                series["MetricName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
        })
        .collect()
}

/// RULE: an invocation of a qualifier with a ready provisioned-concurrency allocation is
/// metered on the side of the split that served it and on the utilisation that admission left,
/// and a qualifier with no allocation has none of those series at all.
#[tokio::test]
async fn an_invocation_of_a_provisioned_qualifier_is_metered_on_the_split_that_served_it() {
    for row in ROWS {
        // One account per row, so the only invocations in the window are this row's.
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);
        let case = row.case;
        create_function_with_alias(&router, &engine, case).await;
        if let Some(requested) = row.allocated {
            allocate_provisioned_concurrency(&router, &engine, case, requested).await;
        }
        occupy_provisioned_environments(&engine, row.occupied);

        for (nth, target) in row.invokes.iter().enumerate() {
            let answer = invoke(&router, *target).await;
            assert_eq!(
                answer.status,
                RUN_NOT_IMPLEMENTED,
                "case {case:?}: invocation {nth} of {target:?} must answer \
                 {RUN_NOT_IMPLEMENTED}, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
        }

        // Every invocation reached the function, whichever side of the split served it: the
        // family reports HOW they were served and never how many there were.
        let invocations = total(
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
        let made = f64::from(u32::try_from(row.invokes.len()).unwrap_or_default());
        assert_eq!(
            invocations, made,
            "case {case:?}: {INVOCATIONS_SERIES} of the function must sum to the {made} \
             invocations made, got {invocations}"
        );

        let alias_resource = Target::Alias.resource();
        let alias_dimensions = resource_dimensions(&alias_resource);
        let pool = datapoints(&router, case, POOL_SERIES, COUNT_UNIT, &alias_dimensions).await;
        assert_count(case, POOL_SERIES, &pool, row.pool_sum);
        let spillover =
            datapoints(&router, case, SPILLOVER_SERIES, COUNT_UNIT, &alias_dimensions).await;
        assert_count(case, SPILLOVER_SERIES, &spillover, row.spillover_sum);

        // The utilisation gauge: the fraction of the allocation in use at each admission, with
        // no unit of its own.
        let utilization =
            datapoints(&router, case, UTILIZATION_SERIES, NO_UNIT, &alias_dimensions).await;
        match row.utilization_maximum {
            None => assert!(
                utilization.is_empty(),
                "case {case:?}: {UTILIZATION_SERIES} must have no datapoint at all, got \
                 {utilization:?}"
            ),
            Some(expected) => {
                let highest = greatest(&utilization);
                assert_eq!(
                    highest,
                    Some(expected),
                    "case {case:?}: the greatest {UTILIZATION_SERIES} reading must be \
                     {expected}, got {highest:?} over {utilization:?}"
                );
                assert!(
                    utilization
                        .iter()
                        .all(|point| point["Unit"].as_str() == Some(NO_UNIT)),
                    "case {case:?}: a fraction carries no unit, so every datapoint of \
                     {UTILIZATION_SERIES} must carry {NO_UNIT:?}, got {utilization:?}"
                );
                assert!(
                    utilization
                        .iter()
                        .filter_map(|point| point["Minimum"].as_f64())
                        .all(|reading| reading > 0.0 && reading <= 1.0),
                    "case {case:?}: {UTILIZATION_SERIES} is a fraction of the allocation, so \
                     every reading must lie in (0, 1], got {utilization:?}"
                );
            }
        }

        // The version the alias resolves to holds no allocation of its own, so none of the
        // family is reported for it however the alias's traffic was served.
        let version_dimensions = resource_dimensions(&Target::Version.resource());
        for metric in [POOL_SERIES, SPILLOVER_SERIES] {
            let points = datapoints(&router, case, metric, COUNT_UNIT, &version_dimensions).await;
            assert!(
                points.is_empty(),
                "case {case:?}: {} holds no allocation, so {metric} must have no datapoint, got \
                 {points:?}",
                Target::Version.resource()
            );
        }

        // What a client enumerating the alias's series finds: the family exactly where an
        // allocation served traffic, and nothing of it anywhere else.
        let discovered = listed_metrics_for(
            &router,
            case,
            &[
                (FUNCTION_DIMENSION, FUNCTION.to_string()),
                (RESOURCE_DIMENSION, alias_resource.clone()),
            ],
        )
        .await;
        for (metric, expected) in [
            (POOL_SERIES, row.pool_sum.is_some()),
            (SPILLOVER_SERIES, row.spillover_sum.is_some()),
            (UTILIZATION_SERIES, row.utilization_maximum.is_some()),
        ] {
            assert_eq!(
                discovered.iter().any(|found| found == metric),
                expected,
                "case {case:?}: ListMetrics must {} discover {metric} for {alias_resource:?}, \
                 got {discovered:?}",
                if expected { "" } else { "not" }
            );
        }

        // Reading the series changes nothing a later reader sees.
        let again = datapoints(&router, case, POOL_SERIES, COUNT_UNIT, &alias_dimensions).await;
        assert_eq!(
            total(&again, "Sum"),
            total(&pool, "Sum"),
            "case {case:?}: a second read of {POOL_SERIES} must report the same sum, got \
             {again:?}"
        );
    }
}
