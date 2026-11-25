// SPDX-License-Identifier: CC0-1.0

//! `GetEC2InstanceRecommendations` over the wire: the recommendation this service derives
//! from each analysed Amazon EC2 instance, and how a request narrows, pages and refuses.
//!
//! The RULE under test: for every instance of the CALLER's own account the endpoint's Region
//! has analysed, the read reports a recommendation computed from the stored analysis — the
//! type it runs, the finding and reason codes the analysis reached, the utilization it
//! observed, and one ranked option per candidate instance type carrying the utilization
//! projected onto that type and the saving adopting it offers — narrowed by `accountIds`,
//! `instanceArns`, `filters` and `recommendationPreferences`, one page at a time; an
//! identifier the analysis holds no recommendation for is absent from the answer, and a
//! request the service cannot honour is refused with the operation's modelled 400.
//!
//! The analysis of an observed workload is what produces a recommendation, and no operation
//! of THIS API creates one, so the fixtures below record instances into the store the way
//! Compute Optimizer's own analysis would and then read them back only over the wire. What is
//! asserted is the rule over a TABLE of instances that differ in what the analysis found (a
//! type to size down to, one to size up to, nothing to move to), in how many candidates they
//! have and of which CPU architecture, and in whether they carry tags — never one recorded
//! call. Every money figure and every projected percentage is recomputed here from the stored
//! prices and capacities, so a response that is not derived from the store fails.
//!
//! The last suites here are about what the account's STORED preferences do to that answer: a
//! `utilizationPreferences` preference states the percentile of the analysed utilization the
//! sizing reads and the headroom it keeps above it, so it decides which candidate instance
//! types carry the workload and are reported as options at all.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::state::{
    service_state, InstanceConfiguration, ObservedInstance, ObservedInstanceMetric,
    ANALYSIS_DELAY, CPU_ARCHITECTURE_AWS_ARM64, CPU_ARCHITECTURE_CURRENT, DEFAULT_PAGE_SIZE,
    FINDING_OPTIMIZED, FINDING_OVERPROVISIONED, FINDING_UNDERPROVISIONED, HOURS_PER_MONTH,
    INSTANCE_STATE_RUNNING, MAX_REQUESTED_IDENTIFIERS, NAME_TAG_KEY, PERFORMANCE_RISK_VERY_LOW,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, call_in_region, emulator_with_engine, error_code, make_analysis_due, opt_in,
    put_preference, ACCOUNT,
};

/// The region the harness signs for, and so the regional endpoint every call below addresses
/// unless it names another.
const REGION: &str = "us-east-1";

/// The one `MetricStatistic` this API reports: "The Compute Optimizer API, Command Line
/// Interface (CLI), and SDKs return utilization metrics using only the `Maximum` statistic"
/// (`UtilizationMetric$statistic`).
const MAXIMUM: &str = "Maximum";

/// The other statistic the analysis records, which no reader of this API reports.
const AVERAGE: &str = "Average";

/// The ids of the analysed instances [`corpus`] records.
const SIZED_DOWN: &str = "i-0c1b2c3d4e5f80001";
const SIZED_UP: &str = "i-0c1b2c3d4e5f80002";
const ALREADY_RIGHT_SIZED: &str = "i-0c1b2c3d4e5f80003";
const TWO_ARCHITECTURES: &str = "i-0c1b2c3d4e5f80004";

/// One instance type as the analysis records it: the type, the vCPUs and memory (GiB) it
/// provides, its on-demand price per hour and its `CpuVendorArchitecture`.
///
/// The vCPU and memory figures are those of the named instance types in the Amazon EC2
/// instance-type documentation; the prices are their public on-demand Linux prices in
/// `us-east-1`.
fn configuration(
    instance_type: &str,
    vcpus: f64,
    memory_gib: f64,
    hourly_price: f64,
    architecture: &str,
) -> InstanceConfiguration {
    InstanceConfiguration {
        instance_type: instance_type.to_string(),
        vcpus,
        memory_gib,
        hourly_price,
        architecture: architecture.to_string(),
    }
}

fn m5_large() -> InstanceConfiguration {
    configuration("m5.large", 2.0, 8.0, 0.096, CPU_ARCHITECTURE_CURRENT)
}

fn m5_xlarge() -> InstanceConfiguration {
    configuration("m5.xlarge", 4.0, 16.0, 0.192, CPU_ARCHITECTURE_CURRENT)
}

fn m5_2xlarge() -> InstanceConfiguration {
    configuration("m5.2xlarge", 8.0, 32.0, 0.384, CPU_ARCHITECTURE_CURRENT)
}

fn m6g_xlarge() -> InstanceConfiguration {
    configuration("m6g.xlarge", 4.0, 16.0, 0.154, CPU_ARCHITECTURE_AWS_ARM64)
}

/// One utilization figure as the analysis observed it: a `MetricName`, the `MetricStatistic`
/// it was observed under, and its value.
fn metric(name: &str, statistic: &str, value: f64) -> ObservedInstanceMetric {
    ObservedInstanceMetric {
        name: name.to_string(),
        statistic: statistic.to_string(),
        value,
    }
}

/// One analysed instance, as Compute Optimizer's analysis of an observed workload records it.
fn instance(
    instance_id: &str,
    current: InstanceConfiguration,
    candidates: Vec<InstanceConfiguration>,
    utilization: Vec<ObservedInstanceMetric>,
    finding_reason_codes: &[&str],
    inferred_workload_types: &[&str],
    tags: &[(&str, &str)],
) -> ObservedInstance {
    ObservedInstance {
        instance_id: instance_id.to_string(),
        account_id: ACCOUNT.to_string(),
        current,
        candidates,
        utilization,
        finding_reason_codes: finding_reason_codes
            .iter()
            .map(|code| (*code).to_string())
            .collect(),
        current_performance_risk: PERFORMANCE_RISK_VERY_LOW.to_string(),
        inferred_workload_types: inferred_workload_types
            .iter()
            .map(|workload| (*workload).to_string())
            .collect(),
        instance_state: INSTANCE_STATE_RUNNING.to_string(),
        tags: tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
    }
}

/// The analysis output every fixture below reads: instances spanning the cases an instance
/// recommendation distinguishes — one the analysis would size DOWN (so adopting its option
/// saves), one it would size UP (so its option saves nothing), one it would leave as it is
/// (no option at all), and one with candidates of two CPU vendor architectures (so a CPU
/// vendor architecture preference has something to select). Two carry tags, so a name is
/// reported for them and none for the others.
fn corpus() -> Vec<ObservedInstance> {
    vec![
        instance(
            SIZED_DOWN,
            m5_2xlarge(),
            vec![m5_xlarge()],
            vec![
                metric("Cpu", MAXIMUM, 22.5),
                metric("Memory", MAXIMUM, 31.0),
                // A metric the instance type does not size: the workload's own traffic, in
                // bytes per second, which no option projects.
                metric("NETWORK_IN_BYTES_PER_SECOND", MAXIMUM, 1_250_000.0),
                // Observed under the other statistic, which this API does not report.
                metric("Cpu", AVERAGE, 12.0),
            ],
            &["MemoryOverprovisioned"],
            &["PostgreSql"],
            &[(NAME_TAG_KEY, "reporting-db"), ("Owner", "TeamA")],
        ),
        instance(
            SIZED_UP,
            m5_large(),
            vec![m5_xlarge()],
            vec![metric("Cpu", MAXIMUM, 88.0), metric("Memory", MAXIMUM, 97.0)],
            &["MemoryUnderprovisioned"],
            &["Redis"],
            &[(NAME_TAG_KEY, "session-cache")],
        ),
        instance(
            ALREADY_RIGHT_SIZED,
            m5_xlarge(),
            Vec::new(),
            vec![metric("Cpu", MAXIMUM, 55.0), metric("Memory", MAXIMUM, 60.0)],
            &[],
            &[],
            &[],
        ),
        instance(
            TWO_ARCHITECTURES,
            m5_2xlarge(),
            vec![m5_xlarge(), m6g_xlarge()],
            vec![metric("Cpu", MAXIMUM, 24.0), metric("Memory", MAXIMUM, 36.0)],
            &["CPUOverprovisioned"],
            &["AmazonEmr"],
            &[],
        ),
    ]
}

/// The analysed instance the corpus holds under `instance_id`.
fn analysed_instance(instance_id: &str) -> ObservedInstance {
    corpus()
        .into_iter()
        .find(|instance| instance.instance_id == instance_id)
        .unwrap_or_else(|| panic!("the corpus holds no instance {instance_id}"))
}

/// One analysed instance's ARN at the endpoint of `region`.
fn instance_arn_in(region: &str, instance_id: &str) -> String {
    format!("arn:aws:ec2:{region}:{ACCOUNT}:instance/{instance_id}")
}

/// One analysed instance's ARN at the endpoint every fixture addresses.
fn instance_arn(instance_id: &str) -> String {
    instance_arn_in(REGION, instance_id)
}

/// Record `instances` into the analysis this reader reports from, the way Compute Optimizer's
/// own analysis of an observed workload would. The instances this emulator ships with are
/// replaced, so the table below is exactly what the fixtures read.
fn record(engine: &Arc<engine::InMemoryEngine>, instances: Vec<ObservedInstance>) {
    let state = service_state(&**engine).expect("the engine bears this service's state");
    let mut guard = state.lock();
    guard.instances.clear();
    for instance in instances {
        guard.instances.insert(instance.instance_id.clone(), instance);
    }
}

/// The analysis and the preferences as the store holds them, so a test can prove a read moved
/// none of it.
fn snapshot(engine: &Arc<engine::InMemoryEngine>) -> String {
    let state = service_state(&**engine).expect("the engine bears this service's state");
    let guard = state.lock();
    format!("{:?} {:?}", guard.instances, guard.preferences)
}

/// Opt the account in and let the analysis its enrollment starts come due — the gate every
/// recommendation reader of this service applies. The span is the service's own
/// ([`ANALYSIS_DELAY`]); the extra margin covers scheduler jitter.
async fn analysed(router: &MultiServiceRouter) {
    opt_in(router).await;
    tokio::time::sleep(ANALYSIS_DELAY + Duration::from_millis(100)).await;
}

/// An emulator whose account is opted in, whose analysis is due and which holds [`corpus`].
async fn emulator_with_corpus() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_with_engine();
    record(&engine, corpus());
    analysed(&router).await;
    (router, engine)
}

/// Read instance recommendations, asserting the call was served.
async fn recommendations(router: &MultiServiceRouter, case: &str, request: Value) -> Value {
    let (status, body) = call(router, "GetEC2InstanceRecommendations", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {request}, got {status}: {body}"
    );
    body
}

/// Assert a request is refused with the operation's modelled 400, and answer the message.
async fn refused(router: &MultiServiceRouter, case: &str, request: Value) -> String {
    let (status, body) = call(router, "GetEC2InstanceRecommendations", request.clone()).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {request} to be refused, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValueException",
        "case {case:?}: expected InvalidParameterValueException, got {body}"
    );
    assert!(
        body["instanceRecommendations"].is_null(),
        "case {case:?}: a refused read reports no recommendations, got {body}"
    );
    body["message"].as_str().unwrap_or_default().to_string()
}

/// The `instanceArn` of every recommendation in a response, in the order reported.
fn reported_arns(body: &Value) -> Vec<String> {
    body["instanceRecommendations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| row["instanceArn"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The identifier of every entry of a response's `errors` list, in the order reported.
fn reported_error_identifiers(body: &Value) -> Vec<String> {
    body["errors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|error| error["identifier"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The recommendation a response carries for one analysed instance.
fn recommendation_for(body: &Value, instance_id: &str) -> Value {
    let wanted = instance_arn(instance_id);
    body["instanceRecommendations"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {instance_id}: instanceRecommendations must be a list: {body}")
        })
        .iter()
        .find(|row| row["instanceArn"] == json!(wanted))
        .cloned()
        .unwrap_or_else(|| panic!("case {instance_id}: no recommendation for {wanted}: {body}"))
}

/// A number the wire carries, or `0` for one the protocol omitted because it equals the
/// member's modelled default.
fn number(value: &Value, member: &str) -> f64 {
    value[member].as_f64().unwrap_or(0.0)
}

/// The recommendation options of a recommendation, in the order reported.
fn options(recommendation: &Value) -> Vec<Value> {
    recommendation["recommendationOptions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// One reported metric list as (name, statistic, value) triples, so a comparison is on the
/// figures rather than on how the protocol rendered them.
fn reported_metrics(value: &Value, member: &str) -> Vec<(String, String, f64)> {
    value[member]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().unwrap_or_default().to_string(),
                entry["statistic"].as_str().unwrap_or_default().to_string(),
                number(entry, "value"),
            )
        })
        .collect()
}

/// The estimated monthly on-demand cost of one instance of `configuration`: the price of one
/// instance-hour × the hours in a month, which is how this service prices an instance.
fn monthly_cost(configuration: &InstanceConfiguration) -> f64 {
    round_hundredths(configuration.hourly_price * HOURS_PER_MONTH)
}

/// Two decimal places, the precision every money figure and percentage of this service is
/// reported to.
fn round_hundredths(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Every analysed instance of the caller's account is reported with the members the stored
/// analysis supports, and every option is a candidate the analysis found — ranked, with the
/// utilization projected onto it and the saving adopting it offers. Reading it twice changes
/// nothing a later caller sees.
#[tokio::test]
async fn every_analysed_instance_is_reported_from_its_stored_analysis() {
    let (router, engine) = emulator_with_corpus().await;
    let before = snapshot(&engine);
    let body = recommendations(&router, "the whole analysis", json!({})).await;

    let mut reported = reported_arns(&body);
    reported.sort();
    let mut expected: Vec<String> = corpus()
        .iter()
        .map(|instance| instance_arn(&instance.instance_id))
        .collect();
    expected.sort();
    assert_eq!(
        reported, expected,
        "one recommendation per analysed instance of this account, got {body}"
    );
    assert_eq!(
        body["errors"],
        json!([]),
        "a request naming no instance reports no per-identifier error, got {body}"
    );

    // (case, the finding the analysis reaches, the candidates it found)
    let rows: [(&str, &str, Vec<InstanceConfiguration>); 4] = [
        // Sizing down means at least one specification can shrink: over-provisioned.
        (SIZED_DOWN, FINDING_OVERPROVISIONED, vec![m5_xlarge()]),
        // Sizing up means a specification did not meet the workload: under-provisioned.
        (SIZED_UP, FINDING_UNDERPROVISIONED, vec![m5_xlarge()]),
        // Nothing to move to: optimized, and no option to report.
        (ALREADY_RIGHT_SIZED, FINDING_OPTIMIZED, Vec::new()),
        (
            TWO_ARCHITECTURES,
            FINDING_OVERPROVISIONED,
            vec![m5_xlarge(), m6g_xlarge()],
        ),
    ];

    for (instance_id, finding, candidates) in rows {
        let analysed = analysed_instance(instance_id);
        let recommendation = recommendation_for(&body, instance_id);
        let case = instance_id;

        assert_eq!(
            recommendation["accountId"],
            json!(ACCOUNT),
            "case {case:?}: every recommendation is of the caller's own account, got \
             {recommendation}"
        );
        assert_eq!(
            recommendation["currentInstanceType"],
            json!(analysed.current.instance_type),
            "case {case:?}: expected the type the instance runs, got {recommendation}"
        );
        assert_eq!(
            recommendation["finding"],
            json!(finding),
            "case {case:?}: expected finding {finding}, got {recommendation}"
        );
        assert_eq!(
            recommendation["findingReasonCodes"],
            json!(analysed.finding_reason_codes),
            "case {case:?}: expected the stored reason codes, got {recommendation}"
        );
        assert_eq!(
            recommendation["currentPerformanceRisk"],
            json!(analysed.current_performance_risk),
            "case {case:?}: expected the stored performance risk, got {recommendation}"
        );
        assert_eq!(
            recommendation["instanceState"],
            json!(INSTANCE_STATE_RUNNING),
            "case {case:?}: expected the state the instance was analysed in, got \
             {recommendation}"
        );
        assert_eq!(
            recommendation["inferredWorkloadTypes"],
            json!(analysed.inferred_workload_types),
            "case {case:?}: expected the inferred workload types, got {recommendation}"
        );
        // "The number of days for which utilization metrics were analyzed": the length of the
        // look-back period in force, which no preference has changed here (`DAYS_14`).
        assert_eq!(
            number(&recommendation, "lookBackPeriodInDays"),
            14.0,
            "case {case:?}: expected the default look-back period's length, got {recommendation}"
        );
        assert_eq!(
            recommendation["idle"],
            json!("False"),
            "case {case:?}: this service holds no idle recommendation for the instance, got \
             {recommendation}"
        );
        assert_eq!(
            recommendation["recommendationSources"],
            json!([{
                "recommendationSourceArn": instance_arn(instance_id),
                "recommendationSourceType": "Ec2Instance",
            }]),
            "case {case:?}: the source of an instance recommendation is that instance, got \
             {recommendation}"
        );
        // An account that prefers no external metrics provider has no integration at all.
        assert_eq!(
            recommendation["externalMetricStatus"]["statusCode"],
            json!("NO_EXTERNAL_METRIC_SET"),
            "case {case:?}: expected no external metrics integration, got {recommendation}"
        );
        // The `Name` tag is what names an instance; one that carries no tag carries no name.
        let name = analysed
            .tags
            .iter()
            .find(|(key, _)| key == NAME_TAG_KEY)
            .map(|(_, value)| value.clone());
        assert_eq!(
            recommendation["instanceName"],
            name.map_or(Value::Null, Value::from),
            "case {case:?}: expected the name the Name tag gives, got {recommendation}"
        );
        let tags: Vec<Value> = analysed
            .tags
            .iter()
            .map(|(key, value)| json!({"key": key, "value": value}))
            .collect();
        assert_eq!(
            recommendation["tags"],
            if tags.is_empty() {
                Value::Null
            } else {
                Value::from(tags)
            },
            "case {case:?}: expected the stored tags, got {recommendation}"
        );

        // Only the observations recorded under `Maximum` are reported.
        let expected_metrics: Vec<(String, String, f64)> = analysed
            .utilization
            .iter()
            .filter(|metric| metric.statistic == MAXIMUM)
            .map(|metric| (metric.name.clone(), MAXIMUM.to_string(), metric.value))
            .collect();
        assert_eq!(
            reported_metrics(&recommendation, "utilizationMetrics"),
            expected_metrics,
            "case {case:?}: expected the observed Maximum utilization, got {recommendation}"
        );

        let reported_options = options(&recommendation);
        assert_eq!(
            reported_options.len(),
            candidates.len(),
            "case {case:?}: expected one option per candidate, got {recommendation}"
        );
        for (index, candidate) in candidates.iter().enumerate() {
            let option = &reported_options[index];
            let case = format!("{case}/{}", candidate.instance_type);
            assert_eq!(
                option["instanceType"],
                json!(candidate.instance_type),
                "case {case:?}: expected the candidate's instance type, got {option}"
            );
            // "The top recommendation option is ranked as 1."
            let rank = (index + 1) as f64;
            assert_eq!(
                number(option, "rank"),
                rank,
                "case {case:?}: expected rank {rank}, got {option}"
            );
            // The saving is what the move takes off the monthly bill, and the percentage is
            // that saving as a share of what the instance costs today; a move that costs no
            // less saves nothing rather than a negative amount.
            let current = monthly_cost(&analysed.current);
            let saved = round_hundredths((current - monthly_cost(candidate)).max(0.0));
            let percentage = round_hundredths(saved / current * 100.0);
            assert_eq!(
                number(&option["savingsOpportunity"], "savingsOpportunityPercentage"),
                percentage,
                "case {case:?}: expected a saving of {percentage}% of {current}, got {option}"
            );
            let savings = &option["savingsOpportunity"]["estimatedMonthlySavings"];
            assert_eq!(
                savings["currency"],
                json!("USD"),
                "case {case:?}: expected the saving in USD, got {option}"
            );
            assert_eq!(
                number(savings, "value"),
                saved,
                "case {case:?}: expected a monthly saving of {saved}, got {option}"
            );
            assert!(
                option["savingsOpportunityAfterDiscounts"].is_null(),
                "case {case:?}: the after-discount member is reported only once the account \
                 activates the preference, got {option}"
            );
            // Only `Cpu` and `Memory` are projected, each scaled by the ratio of the capacity
            // it was observed on to the capacity it is projected onto — and no percentage can
            // pass 100.
            let expected_projected: Vec<(String, String, f64)> = analysed
                .utilization
                .iter()
                .filter(|metric| metric.statistic == MAXIMUM)
                .filter(|metric| metric.name == "Cpu" || metric.name == "Memory")
                .map(|metric| {
                    let (from, to) = if metric.name == "Memory" {
                        (analysed.current.memory_gib, candidate.memory_gib)
                    } else {
                        (analysed.current.vcpus, candidate.vcpus)
                    };
                    let projected = round_hundredths((metric.value * from / to).clamp(0.0, 100.0));
                    (metric.name.clone(), MAXIMUM.to_string(), projected)
                })
                .collect();
            assert_eq!(
                reported_metrics(option, "projectedUtilizationMetrics"),
                expected_projected,
                "case {case:?}: expected the projected Cpu and Memory utilization, got {option}"
            );
            // A candidate of another CPU architecture is a platform difference, and moving to
            // it is more effort than staying on the same architecture.
            let differs = candidate.architecture != analysed.current.architecture;
            assert_eq!(
                option["platformDifferences"],
                if differs {
                    json!(["Architecture"])
                } else {
                    json!([])
                },
                "case {case:?}: expected the architecture difference to be stated, got {option}"
            );
            let effort = if !differs {
                "VeryLow"
            } else if analysed.inferred_workload_types.is_empty() {
                "Medium"
            } else {
                "Low"
            };
            assert_eq!(
                option["migrationEffort"],
                json!(effort),
                "case {case:?}: expected migration effort {effort}, got {option}"
            );
            // The risk an option runs is bounded by the model's own 0..4 scale.
            let risk = number(option, "performanceRisk");
            assert!(
                (0.0..=4.0).contains(&risk),
                "case {case:?}: expected a performance risk in 0..4, got {risk}"
            );
        }
    }

    // A read is a READ: the same call answers the same thing, and the store did not move.
    let again = recommendations(&router, "the same read again", json!({})).await;
    assert_eq!(
        again, body,
        "two identical reads must answer identically, got {again}"
    );
    assert_eq!(
        snapshot(&engine),
        before,
        "reading recommendations must not write to the store"
    );
}

/// What a request SELECTS: `accountIds` reads the caller's own account only, `instanceArns`
/// picks instances out of the analysis, an identifier the analysis holds no recommendation
/// for is simply absent from the answer, and an ARN of another Region names an instance this
/// endpoint does not analyse.
#[tokio::test]
async fn account_ids_and_instance_arns_select_what_is_reported() {
    let (router, _engine) = emulator_with_corpus().await;
    let all = reported_arns(&recommendations(&router, "the whole analysis", json!({})).await);
    let unknown = instance_arn("i-0c1b2c3d4e5f8ffff");
    let other_region = instance_arn_in("eu-west-1", SIZED_UP);
    // The SAME instance of the SAME Region, named in another partition: an ARN form the
    // service parses, which names an instance this endpoint analyses.
    let other_partition = format!("arn:aws-cn:ec2:{REGION}:{ACCOUNT}:instance/{SIZED_UP}");

    // (case, request, the ARNs reported)
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "the caller's own account, named",
            json!({"accountIds": [ACCOUNT]}),
            all.clone(),
        ),
        ("an empty account list", json!({"accountIds": []}), all.clone()),
        (
            "one instance",
            json!({"instanceArns": [instance_arn(SIZED_UP)]}),
            vec![instance_arn(SIZED_UP)],
        ),
        (
            "two instances",
            json!({"instanceArns": [instance_arn(SIZED_DOWN), instance_arn(TWO_ARCHITECTURES)]}),
            vec![instance_arn(SIZED_DOWN), instance_arn(TWO_ARCHITECTURES)],
        ),
        (
            "the same instance twice",
            json!({"instanceArns": [instance_arn(SIZED_UP), instance_arn(SIZED_UP)]}),
            vec![instance_arn(SIZED_UP)],
        ),
        (
            "an instance this analysis does not hold",
            json!({"instanceArns": [unknown.clone()]}),
            Vec::new(),
        ),
        (
            "the same unheld instance twice",
            json!({"instanceArns": [unknown.clone(), unknown.clone()]}),
            Vec::new(),
        ),
        (
            "one held and one unheld instance",
            json!({"instanceArns": [instance_arn(SIZED_UP), unknown.clone()]}),
            vec![instance_arn(SIZED_UP)],
        ),
        (
            "an instance of another Region",
            json!({"instanceArns": [other_region.clone()]}),
            Vec::new(),
        ),
        (
            // The identifier names an analysed instance of this Region, so it is answered
            // with that instance's recommendation — whatever partition the ARN was written
            // in.
            "an instance of this Region named in another partition",
            json!({"instanceArns": [other_partition.clone()]}),
            vec![instance_arn(SIZED_UP)],
        ),
    ];

    for (case, request, expected) in rows {
        let body = recommendations(&router, case, request).await;
        let mut reported = reported_arns(&body);
        reported.sort();
        let mut wanted = expected.clone();
        wanted.sort();
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected exactly {wanted:?}, got {body}"
        );
        // An identifier with no recommendation is absent from the answer and is NOT a
        // per-resource failure of the request: `errors` carries the instances an analysis
        // rejected as unsupported, which this inventory holds none of.
        assert_eq!(
            reported_error_identifiers(&body),
            Vec::<String>::new(),
            "case {case:?}: expected no errors entry, got {body}"
        );
    }

    // The endpoint's Region is what the reported ARNs name: the same analysis read at another
    // Region's endpoint reports that Region's ARNs, which is why an ARN of another Region
    // selects nothing here.
    let (status, body) = call_in_region(
        &router,
        "eu-west-1",
        "GetEC2InstanceRecommendations",
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "another Region's endpoint: got {body}");
    let mut reported = reported_arns(&body);
    reported.sort();
    let mut wanted: Vec<String> = corpus()
        .iter()
        .map(|instance| instance_arn_in("eu-west-1", &instance.instance_id))
        .collect();
    wanted.sort();
    assert_eq!(
        reported, wanted,
        "another Region's endpoint reports its own Region's ARNs, got {body}"
    );
}

/// The `Filters` of this surface select on the ANALYSIS: a filter matches on the member it
/// names, several values are ORed, several filters are ANDed, and a filter carrying no values
/// constrains nothing.
#[tokio::test]
async fn filters_select_by_the_analysis_they_name() {
    let (router, _engine) = emulator_with_corpus().await;
    let all = reported_arns(&recommendations(&router, "the whole analysis", json!({})).await);

    // (case, filters, the instances reported)
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "one Finding value",
            json!([{"name": "Finding", "values": ["Underprovisioned"]}]),
            vec![instance_arn(SIZED_UP)],
        ),
        (
            "two Finding values",
            json!([{"name": "Finding", "values": ["Underprovisioned", "Optimized"]}]),
            vec![instance_arn(SIZED_UP), instance_arn(ALREADY_RIGHT_SIZED)],
        ),
        (
            "a Finding nothing in the analysis reaches",
            json!([{"name": "Finding", "values": ["NotOptimized"]}]),
            Vec::new(),
        ),
        (
            "one FindingReasonCodes value",
            json!([{"name": "FindingReasonCodes", "values": ["MemoryOverprovisioned"]}]),
            vec![instance_arn(SIZED_DOWN)],
        ),
        (
            "an InferredWorkloadTypes value",
            json!([{"name": "InferredWorkloadTypes", "values": ["AmazonEmr"]}]),
            vec![instance_arn(TWO_ARCHITECTURES)],
        ),
        (
            "the RecommendationSourceType this reader reports",
            json!([{"name": "RecommendationSourceType", "values": ["Ec2Instance"]}]),
            all.clone(),
        ),
        (
            "another RecommendationSourceType",
            json!([{"name": "RecommendationSourceType", "values": ["EbsVolume"]}]),
            Vec::new(),
        ),
        (
            "two filters, ANDed",
            json!([
                {"name": "Finding", "values": ["Overprovisioned"]},
                {"name": "FindingReasonCodes", "values": ["CPUOverprovisioned"]},
            ]),
            vec![instance_arn(TWO_ARCHITECTURES)],
        ),
        (
            "two filters that select nothing together",
            json!([
                {"name": "Finding", "values": ["Optimized"]},
                {"name": "FindingReasonCodes", "values": ["CPUOverprovisioned"]},
            ]),
            Vec::new(),
        ),
        (
            "a filter carrying no values",
            json!([{"name": "Finding", "values": []}]),
            all.clone(),
        ),
    ];

    for (case, filters, expected) in rows {
        let body = recommendations(&router, case, json!({"filters": filters})).await;
        let mut reported = reported_arns(&body);
        reported.sort();
        let mut wanted = expected.clone();
        wanted.sort();
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected exactly {wanted:?}, got {body}"
        );
    }
}

/// One row of [`a_cpu_vendor_architecture_preference_selects_the_options`]: the CPU vendor
/// architecture preference a request states, the option instance types it then reports for
/// the two-architecture instance, and the architectures those options consist of.
struct ArchitectureCase {
    case: &'static str,
    preference: Option<Value>,
    instance_types: Vec<&'static str>,
    architectures: Vec<&'static str>,
}

/// `cpuVendorArchitectures` selects which candidate instance types the options consist of:
/// "when you specify AWS_ARM64 … Compute Optimizer returns recommendations that consist of
/// Graviton instance types only", and the preferences a recommendation says it was generated
/// under name exactly the architectures its options carry.
#[tokio::test]
async fn a_cpu_vendor_architecture_preference_selects_the_options() {
    let (router, _engine) = emulator_with_corpus().await;

    let rows = vec![
        ArchitectureCase {
            case: "no preference",
            preference: None,
            instance_types: vec!["m5.xlarge", "m6g.xlarge"],
            architectures: vec![CPU_ARCHITECTURE_CURRENT, CPU_ARCHITECTURE_AWS_ARM64],
        },
        ArchitectureCase {
            case: "Graviton only",
            preference: Some(json!({"cpuVendorArchitectures": ["AWS_ARM64"]})),
            instance_types: vec!["m6g.xlarge"],
            architectures: vec![CPU_ARCHITECTURE_AWS_ARM64],
        },
        ArchitectureCase {
            case: "the current architecture only",
            preference: Some(json!({"cpuVendorArchitectures": ["CURRENT"]})),
            instance_types: vec!["m5.xlarge"],
            architectures: vec![CPU_ARCHITECTURE_CURRENT],
        },
        ArchitectureCase {
            case: "both",
            preference: Some(json!({"cpuVendorArchitectures": ["CURRENT", "AWS_ARM64"]})),
            instance_types: vec!["m5.xlarge", "m6g.xlarge"],
            architectures: vec![CPU_ARCHITECTURE_CURRENT, CPU_ARCHITECTURE_AWS_ARM64],
        },
    ];

    for row in rows {
        let ArchitectureCase {
            case,
            preference,
            instance_types: expected,
            architectures,
        } = row;
        let mut request = json!({"instanceArns": [instance_arn(TWO_ARCHITECTURES)]});
        if let Some(preference) = preference {
            request["recommendationPreferences"] = preference;
        }
        let body = recommendations(&router, case, request).await;
        let recommendation = recommendation_for(&body, TWO_ARCHITECTURES);
        let reported: Vec<String> = options(&recommendation)
            .iter()
            .map(|option| {
                option["instanceType"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected options {expected:?}, got {recommendation}"
        );
        // Every option is ranked from 1 among the options REPORTED.
        let ranks: Vec<f64> = options(&recommendation)
            .iter()
            .map(|option| number(option, "rank"))
            .collect();
        let wanted: Vec<f64> = (1..=expected.len()).map(|rank| rank as f64).collect();
        assert_eq!(
            ranks, wanted,
            "case {case:?}: expected ranks {wanted:?}, got {recommendation}"
        );
        // "Describes the CPU vendor and architecture for an instance … recommendation": the
        // architectures the reported options actually consist of.
        assert_eq!(
            recommendation["effectiveRecommendationPreferences"]["cpuVendorArchitectures"],
            json!(architectures),
            "case {case:?}: expected the architectures the options consist of, got \
             {recommendation}"
        );
    }
}

/// `maxResults` caps a page and the `nextToken` resumes exactly where the previous page
/// stopped: whatever page size walks the analysis, the sequence it reports is the same one an
/// unpaged read reports, each instance exactly once.
#[tokio::test]
async fn pages_walk_every_recommendation_exactly_once() {
    let (router, _engine) = emulator_with_corpus().await;
    let all = reported_arns(&recommendations(&router, "the unpaged read", json!({})).await);
    assert_eq!(
        all.len(),
        corpus().len(),
        "the whole analysis: expected {} rows, got {all:?}",
        corpus().len()
    );

    // Rows: every page size from one up to one past the sequence, plus `0` — the modelled
    // range's lower bound, which carries no documented meaning and so asks for no size at
    // all, which is the service's own default page.
    for page_size in [0, 1, 2, 3, 4, 5] {
        let case = format!("maxResults {page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut finished = false;
        // One page per row plus one, so a token that never terminates fails the test instead
        // of hanging it.
        for page in 0..=corpus().len() {
            let mut request = json!({"maxResults": page_size});
            if let Some(token) = &token {
                request["nextToken"] = json!(token);
            }
            let body = recommendations(&router, &case, request).await;
            let reported = reported_arns(&body);
            let remaining = all.len() - walked.len();
            let expected_len = if page_size == 0 {
                remaining.min(DEFAULT_PAGE_SIZE)
            } else {
                page_size.min(remaining)
            };
            assert_eq!(
                reported.len(),
                expected_len,
                "case {case:?}: page {page} must carry {expected_len} rows, got {body}"
            );
            walked.extend(reported);
            match body["nextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => {
                    finished = true;
                    break;
                }
            }
        }
        assert!(
            finished,
            "case {case:?}: the walk did not finish within {} pages: {walked:?}",
            corpus().len() + 1
        );
        assert_eq!(
            walked, all,
            "case {case:?}: a paged walk reports the unpaged sequence, got {walked:?}"
        );
    }
}

/// An input the service cannot honour is REFUSED, never answered with a selection of
/// something else: an account it cannot report on, an ARN that is not an instance's, a filter
/// outside this surface's vocabulary, and a page token it did not issue.
#[tokio::test]
async fn an_unhonourable_request_is_refused() {
    let (router, engine) = emulator_with_corpus().await;
    let before = snapshot(&engine);
    let cross_account = "Invalid permission to access another account. If this is an \
                         organization account, request the management account to enable \
                         service access.";

    // (case, request, the message the refusal carries)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "another account's id",
            json!({"accountIds": ["210987654321"]}),
            cross_account,
        ),
        (
            "an instance ARN of another account",
            json!({"instanceArns": [
                "arn:aws:ec2:us-east-1:210987654321:instance/i-0123456789abcdef0"
            ]}),
            cross_account,
        ),
        (
            "an ARN that is not an instance's",
            json!({"instanceArns": [
                format!("arn:aws:ec2:{REGION}:{ACCOUNT}:volume/vol-0123456789abcdef0")
            ]}),
            "Invalid instance arn.",
        ),
        (
            "a value that is not an ARN at all",
            json!({"instanceArns": ["i-0123456789abcdef0"]}),
            "Invalid instance arn.",
        ),
        (
            "a page token this service did not issue",
            json!({"nextToken": "not-a-token"}),
            "The value supplied for parameter nextToken is not valid.",
        ),
    ];

    for (case, request, message) in rows {
        let reported = refused(&router, case, request).await;
        assert_eq!(
            reported, message,
            "case {case:?}: expected the refusal {message:?}, got {reported:?}"
        );
    }

    // No request may name more instances than this service answers about in one call: the
    // bound is the crate's ([`MAX_REQUESTED_IDENTIFIERS`]), so a list at it is answered and the
    // first list past it is refused, naming the member and the bound.
    let at_the_bound: Vec<String> = (0..MAX_REQUESTED_IDENTIFIERS)
        .map(|index| instance_arn(&format!("i-0c1b2c3d4e5f9{index:05}")))
        .collect();
    let body = recommendations(
        &router,
        "as many instances as the bound allows",
        json!({"instanceArns": at_the_bound}),
    )
    .await;
    assert_eq!(
        reported_error_identifiers(&body),
        Vec::<String>::new(),
        "a list at the bound is answered, and an identifier with no recommendation is no \
         error of the request: got {body}"
    );
    let past_the_bound: Vec<String> = (0..=MAX_REQUESTED_IDENTIFIERS)
        .map(|index| instance_arn(&format!("i-0c1b2c3d4e5f9{index:05}")))
        .collect();
    let message = refused(
        &router,
        "one instance more than the bound allows",
        json!({"instanceArns": past_the_bound}),
    )
    .await;
    assert!(
        message.contains("instanceArns")
            && message.contains(&MAX_REQUESTED_IDENTIFIERS.to_string()),
        "the refusal must name the member and the bound, got {message:?}"
    );

    // A filter value outside the vocabulary the name selects on, and a filter with no name at
    // all: refused naming the member, rather than silently matching nothing.
    for (case, filters) in [
        (
            "a value outside the Finding vocabulary",
            json!([{"name": "Finding", "values": ["Enormous"]}]),
        ),
        ("a filter with no name", json!([{"values": ["Optimized"]}])),
    ] {
        let message = refused(&router, case, json!({"filters": filters})).await;
        assert!(
            message.contains("filters"),
            "case {case:?}: the refusal must name the member, got {message:?}"
        );
    }

    assert_eq!(
        snapshot(&engine),
        before,
        "a refused read must not write to the store"
    );
}

/// The enrollment gate: Compute Optimizer analyses an account's resources only once it has
/// opted in, and only once the analysis that enrollment starts is due — so an account that
/// never opted in is refused with the modelled 403, and one that has just opted in reads the
/// empty sequence rather than an analysis nothing has produced yet.
#[tokio::test]
async fn the_analysis_is_readable_only_once_the_enrollment_makes_it_due() {
    let (router, engine) = emulator_with_engine();
    record(&engine, corpus());

    let (status, body) = call(&router, "GetEC2InstanceRecommendations", json!({})).await;
    assert_eq!(
        status, 403,
        "an account that never opted in: expected 403, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "OptInRequiredException",
        "an account that never opted in: expected OptInRequiredException, got {body}"
    );

    opt_in(&router).await;
    let body = recommendations(&router, "just opted in", json!({})).await;
    assert_eq!(
        body["instanceRecommendations"],
        json!([]),
        "the analysis an enrollment starts is not due yet, got {body}"
    );

    tokio::time::sleep(ANALYSIS_DELAY + Duration::from_millis(100)).await;
    let body = recommendations(&router, "the analysis is due", json!({})).await;
    assert_eq!(
        reported_arns(&body).len(),
        corpus().len(),
        "once the analysis is due every analysed instance is reported, got {body}"
    );
}

/// The preferences a recommendation says it was generated under are the account's STORED
/// ones: a longer look-back period is reported as the days analysed, and activating the
/// savings-estimation-mode preference is what makes an option carry its after-discount
/// savings.
#[tokio::test]
async fn the_reported_preferences_are_the_account_s_stored_preferences() {
    let (router, _engine) = emulator_with_corpus().await;

    // (case, the preference written, the days reported, whether an option carries the
    // after-discount member)
    let rows: Vec<(&str, Value, f64, bool)> = vec![
        (
            "a longer look-back period",
            json!({
                "resourceType": "Ec2Instance",
                "scope": {"name": "AccountId", "value": ACCOUNT},
                "lookBackPeriod": "DAYS_93",
            }),
            93.0,
            false,
        ),
        (
            "savings net of discounts",
            json!({
                "resourceType": "Ec2Instance",
                "scope": {"name": "AccountId", "value": ACCOUNT},
                "lookBackPeriod": "DAYS_93",
                "savingsEstimationMode": "AfterDiscounts",
            }),
            93.0,
            true,
        ),
    ];

    for (case, preference, days, after_discounts) in rows {
        let (status, body) =
            call(&router, "PutRecommendationPreferences", preference.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: writing {preference} expected 200, got {status}: {body}"
        );
        let body = recommendations(
            &router,
            case,
            json!({"instanceArns": [instance_arn(SIZED_DOWN)]}),
        )
        .await;
        let recommendation = recommendation_for(&body, SIZED_DOWN);
        assert_eq!(
            number(&recommendation, "lookBackPeriodInDays"),
            days,
            "case {case:?}: expected {days} days analysed, got {recommendation}"
        );
        assert_eq!(
            recommendation["effectiveRecommendationPreferences"]["lookBackPeriod"],
            json!("DAYS_93"),
            "case {case:?}: expected the stored look-back preference, got {recommendation}"
        );
        let option = options(&recommendation)
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected an option: {recommendation}"));
        assert_eq!(
            !option["savingsOpportunityAfterDiscounts"].is_null(),
            after_discounts,
            "case {case:?}: expected the after-discount member reported={after_discounts}, got \
             {option}"
        );
        if after_discounts {
            // This emulator records no Savings Plans or Reserved Instances commitment against
            // the account's spend, so the saving after discounts is the saving before them.
            assert_eq!(
                number(
                    &option["savingsOpportunityAfterDiscounts"]["estimatedMonthlySavings"],
                    "value"
                ),
                number(
                    &option["savingsOpportunity"]["estimatedMonthlySavings"],
                    "value"
                ),
                "case {case:?}: expected the same saving before and after discounts, got {option}"
            );
        }
    }
}

// What the account's UTILIZATION preference does to the options this operation reports.
//
// The rule under test: `UtilizationPreference` is "the preference to control the resource's CPU
// utilization threshold, CPU utilization headroom, and memory utilization headroom", and its
// `metricParameters` are "the parameters to set when customizing the resource utilization
// thresholds" — a `threshold` naming the percentile of the analysed utilization the sizing
// reads ("you can only specify the threshold value for CPU utilization") and a `headroom`
// giving "the headroom value in percentage used for the specified metric parameter". So the
// capacity a rightsizing candidate has to provide is the analysed utilization at that
// percentile plus that headroom, of EVERY resource the instance type sizes: a candidate that
// provides less is an option of no answer, and a preference asking for more headroom — or
// reading a higher percentile — therefore leaves fewer options. With nothing stored the
// service's documented defaults are in force ("CPU utilization: `P99_5` for threshold,
// `PERCENT_20` for headroom. Memory utilization: `PERCENT_20` for headroom").
//
// The rows below span both metrics' headroom values (`PERCENT_0` | `PERCENT_10` |
// `PERCENT_20` | `PERCENT_30`), all three thresholds (`P90` | `P95` | `P99_5`), a preference
// that writes the defaults out and no preference at all, over instances whose candidates sit
// on either side of the capacity each case asks for; every expected option set is the
// capacity arithmetic of the stored analysis, stated in the comment beside it.

/// The ids of the analysed instances [`sizing_corpus`] records.
const CPU_HEADROOM_BOUND: &str = "i-0c1b2c3d4e5f80010";
const MEMORY_HEADROOM_BOUND: &str = "i-0c1b2c3d4e5f80011";
const THRESHOLD_BOUND: &str = "i-0c1b2c3d4e5f80012";
const OVERSHOT_PEAK: &str = "i-0c1b2c3d4e5f80013";

/// The first data point the projections below ask for, in epoch seconds.
const SIZING_WINDOW_START: i64 = 1_761_000_000;

/// The analysis output the utilization-preference fixtures read. Every instance runs an
/// `m5.2xlarge` (8 vCPUs, 32 GiB), so what each case varies is the utilization observed on it
/// and the capacity the candidates offer:
///
/// * [`CPU_HEADROOM_BOUND`] peaks at 40% of its vCPUs (3.2 of them) and 30% of its memory
///   (9.6 GiB), with no spread between the statistics, so every threshold reads the same
///   figure and only the CPU headroom decides: `m5.xlarge` has the 4 vCPUs that carries 3.2 of
///   them up to a headroom of 25%, while `m5.large`'s 2 vCPUs carry the workload at no
///   headroom at all.
/// * [`MEMORY_HEADROOM_BOUND`] peaks at 40% of its memory (12.8 GiB) and 20% of its vCPUs, so
///   only the MEMORY headroom decides: `m5.xlarge`'s 16 GiB carry 12.8 up to a headroom of
///   25%.
/// * [`THRESHOLD_BOUND`] averages 20% of its vCPUs and peaks at 45%, so the percentile the
///   threshold names decides what the sizing reads at all.
/// * [`OVERSHOT_PEAK`] averages 4% of its vCPUs and peaks at 60%, so the only preference that
///   leaves `m5.xlarge` an option is the lowest threshold with no headroom — and the PEAK
///   projected onto that option passes the percentage ceiling.
fn sizing_corpus() -> Vec<ObservedInstance> {
    vec![
        instance(
            CPU_HEADROOM_BOUND,
            m5_2xlarge(),
            vec![m5_xlarge(), m5_large()],
            vec![
                metric("Cpu", MAXIMUM, 40.0),
                metric("Cpu", AVERAGE, 40.0),
                metric("Memory", MAXIMUM, 30.0),
                metric("Memory", AVERAGE, 30.0),
            ],
            &[],
            &[],
            &[],
        ),
        instance(
            MEMORY_HEADROOM_BOUND,
            m5_2xlarge(),
            vec![m5_xlarge()],
            vec![
                metric("Cpu", MAXIMUM, 20.0),
                metric("Cpu", AVERAGE, 20.0),
                metric("Memory", MAXIMUM, 40.0),
                metric("Memory", AVERAGE, 40.0),
            ],
            &[],
            &[],
            &[],
        ),
        instance(
            THRESHOLD_BOUND,
            m5_2xlarge(),
            vec![m5_xlarge()],
            vec![
                metric("Cpu", MAXIMUM, 45.0),
                metric("Cpu", AVERAGE, 20.0),
                metric("Memory", MAXIMUM, 20.0),
                metric("Memory", AVERAGE, 20.0),
            ],
            &[],
            &[],
            &[],
        ),
        instance(
            OVERSHOT_PEAK,
            m5_2xlarge(),
            vec![m5_xlarge()],
            vec![
                metric("Cpu", MAXIMUM, 60.0),
                metric("Cpu", AVERAGE, 4.0),
                metric("Memory", MAXIMUM, 20.0),
                metric("Memory", AVERAGE, 20.0),
            ],
            &[],
            &[],
            &[],
        ),
    ]
}

/// An emulator whose account is opted in, whose analysis is DUE and which holds
/// [`sizing_corpus`]. The stored enrollment instant is moved back a span rather than waited
/// out, so nothing here depends on how fast the host runs.
async fn emulator_with_sizing_corpus() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_with_engine();
    record(&engine, sizing_corpus());
    opt_in(&router).await;
    make_analysis_due(&engine);
    (router, engine)
}

/// A `PutRecommendationPreferences` request storing `utilization_preferences` for the
/// account's Amazon EC2 instances — the one resource type this preference may be set for
/// ("you can only set CPU and memory utilization preferences for the Amazon EC2 instance
/// resource type").
fn utilization_preference(utilization_preferences: Value) -> Value {
    json!({
        "resourceType": "Ec2Instance",
        "scope": {"name": "AccountId", "value": ACCOUNT},
        "utilizationPreferences": utilization_preferences,
    })
}

/// One stored utilization preference entry: the `CustomizableMetricName` and the
/// `metricParameters` set for it.
fn utilization_entry(metric_name: &str, threshold: Option<&str>, headroom: &str) -> Value {
    let mut parameters = json!({"headroom": headroom});
    if let Some(threshold) = threshold {
        parameters["threshold"] = json!(threshold);
    }
    json!({"metricName": metric_name, "metricParameters": parameters})
}

/// The instance types the read reports as options of one instance's recommendation, in the
/// order reported.
fn reported_option_types(case: &str, body: &Value, instance_id: &str) -> Vec<String> {
    let recommendation = recommendation_for(body, instance_id);
    assert!(
        !recommendation["recommendationOptions"].is_null(),
        "case {case:?}: {instance_id} must carry an options list, got {recommendation}"
    );
    options(&recommendation)
        .iter()
        .map(|option| {
            option["instanceType"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The instance types `GetEC2RecommendationProjectedMetrics` projects for one instance, in the
/// order reported, with the series each option carries.
async fn projected_options(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Vec<Value> {
    let request = json!({
        "instanceArn": instance_arn(instance_id),
        "stat": MAXIMUM,
        "period": 3_600,
        "startTime": SIZING_WINDOW_START,
        "endTime": SIZING_WINDOW_START + 3_600,
    });
    let (status, body) = call(router, "GetEC2RecommendationProjectedMetrics", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {request}, got {status}: {body}"
    );
    body["recommendedOptionProjectedMetrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// One row of the sizing table: the case, the `utilizationPreferences` the account stores
/// (`None` where it stores none, so the service's documented defaults are in force), and the
/// option types expected per analysed instance.
type SizingRow<'a> = (&'a str, Option<Value>, Vec<(&'a str, Vec<&'a str>)>);

/// The utilization preference in force decides which candidates are options: a candidate is
/// one only where it provides the analysed utilization the preference's threshold reads, plus
/// the headroom it states, of every resource the instance type sizes.
#[tokio::test]
async fn the_utilization_preference_in_force_decides_which_candidates_are_options() {
    // The three instance types the candidates are drawn from: 8 vCPUs and 32 GiB for the type
    // every analysed instance runs, 4 and 16 for `m5.xlarge`, 2 and 8 for `m5.large`.
    let rows: Vec<SizingRow> = vec![
        (
            "nothing stored, so the documented defaults are in force",
            None,
            vec![
                // 3.2 vCPUs with 20% headroom asks for 3.84 of them, which only m5.xlarge has.
                (CPU_HEADROOM_BOUND, vec!["m5.xlarge"]),
                // 12.8 GiB with 20% headroom asks for 15.36, which m5.xlarge's 16 carry.
                (MEMORY_HEADROOM_BOUND, vec!["m5.xlarge"]),
                // P99_5 reads 44.75% of 8 vCPUs; 3.58 of them with 20% headroom asks for
                // 4.296, which m5.xlarge's 4 do not carry.
                (THRESHOLD_BOUND, Vec::new()),
                // P99_5 reads 59.44%: 4.76 vCPUs with headroom asks for 5.71.
                (OVERSHOT_PEAK, Vec::new()),
            ],
        ),
        (
            "the documented defaults written out",
            Some(json!([
                utilization_entry("CpuUtilization", Some("P99_5"), "PERCENT_20"),
                utilization_entry("MemoryUtilization", None, "PERCENT_20"),
            ])),
            vec![
                (CPU_HEADROOM_BOUND, vec!["m5.xlarge"]),
                (MEMORY_HEADROOM_BOUND, vec!["m5.xlarge"]),
                (THRESHOLD_BOUND, Vec::new()),
                (OVERSHOT_PEAK, Vec::new()),
            ],
        ),
        (
            "no cpu headroom at all",
            Some(json!([
                utilization_entry("CpuUtilization", Some("P99_5"), "PERCENT_0"),
                utilization_entry("MemoryUtilization", None, "PERCENT_20"),
            ])),
            vec![
                (CPU_HEADROOM_BOUND, vec!["m5.xlarge"]),
                (MEMORY_HEADROOM_BOUND, vec!["m5.xlarge"]),
                // 3.58 vCPUs with no headroom is inside m5.xlarge's 4.
                (THRESHOLD_BOUND, vec!["m5.xlarge"]),
                // 4.76 vCPUs is not, whatever the headroom.
                (OVERSHOT_PEAK, Vec::new()),
            ],
        ),
        (
            "the widest cpu headroom",
            Some(json!([
                utilization_entry("CpuUtilization", Some("P99_5"), "PERCENT_30"),
                utilization_entry("MemoryUtilization", None, "PERCENT_20"),
            ])),
            vec![
                // 3.2 vCPUs with 30% headroom asks for 4.16, which m5.xlarge's 4 do not carry.
                (CPU_HEADROOM_BOUND, Vec::new()),
                // 1.6 vCPUs with 30% headroom is 2.08, and the memory headroom is unchanged.
                (MEMORY_HEADROOM_BOUND, vec!["m5.xlarge"]),
                (THRESHOLD_BOUND, Vec::new()),
                (OVERSHOT_PEAK, Vec::new()),
            ],
        ),
        (
            "the widest memory headroom, leaving the cpu parameters at their defaults",
            Some(json!([utilization_entry(
                "MemoryUtilization",
                None,
                "PERCENT_30"
            )])),
            vec![
                // 9.6 GiB with 30% headroom is 12.48, inside m5.xlarge's 16.
                (CPU_HEADROOM_BOUND, vec!["m5.xlarge"]),
                // 12.8 GiB with 30% headroom asks for 16.64, which those 16 do not carry.
                (MEMORY_HEADROOM_BOUND, Vec::new()),
                (THRESHOLD_BOUND, Vec::new()),
                (OVERSHOT_PEAK, Vec::new()),
            ],
        ),
        (
            "the lowest cpu threshold",
            Some(json!([
                utilization_entry("CpuUtilization", Some("P90"), "PERCENT_20"),
                utilization_entry("MemoryUtilization", None, "PERCENT_20"),
            ])),
            vec![
                (CPU_HEADROOM_BOUND, vec!["m5.xlarge"]),
                (MEMORY_HEADROOM_BOUND, vec!["m5.xlarge"]),
                // P90 reads 40% of 8 vCPUs: 3.2 of them with 20% headroom is 3.84.
                (THRESHOLD_BOUND, vec!["m5.xlarge"]),
                // P90 reads 48.8%: 3.9 vCPUs with 20% headroom asks for 4.68.
                (OVERSHOT_PEAK, Vec::new()),
            ],
        ),
        (
            "the middle cpu threshold",
            Some(json!([
                utilization_entry("CpuUtilization", Some("P95"), "PERCENT_20"),
                utilization_entry("MemoryUtilization", None, "PERCENT_20"),
            ])),
            vec![
                (CPU_HEADROOM_BOUND, vec!["m5.xlarge"]),
                (MEMORY_HEADROOM_BOUND, vec!["m5.xlarge"]),
                // P95 reads 42.5% of 8 vCPUs: 3.4 of them with headroom asks for 4.08.
                (THRESHOLD_BOUND, Vec::new()),
                (OVERSHOT_PEAK, Vec::new()),
            ],
        ),
        (
            "the lowest cpu threshold with no headroom and the narrowest memory headroom",
            Some(json!([
                utilization_entry("CpuUtilization", Some("P90"), "PERCENT_0"),
                utilization_entry("MemoryUtilization", None, "PERCENT_10"),
            ])),
            vec![
                (CPU_HEADROOM_BOUND, vec!["m5.xlarge"]),
                // 12.8 GiB with 10% headroom is 14.08, inside m5.xlarge's 16.
                (MEMORY_HEADROOM_BOUND, vec!["m5.xlarge"]),
                (THRESHOLD_BOUND, vec!["m5.xlarge"]),
                // P90 reads 48.8% of 8 vCPUs: 3.9 of them with no headroom is inside 4.
                (OVERSHOT_PEAK, vec!["m5.xlarge"]),
            ],
        ),
    ];

    for (case, preference, expected) in rows {
        // A fresh emulator per row, so each row is read under exactly the preference it
        // states.
        let (router, _engine) = emulator_with_sizing_corpus().await;
        if let Some(stored) = &preference {
            put_preference(&router, &utilization_preference(stored.clone())).await;
        }
        let body = recommendations(&router, case, json!({})).await;
        for (instance_id, wanted) in expected {
            let reported = reported_option_types(case, &body, instance_id);
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected {instance_id} to report the options {wanted:?}, got \
                 {reported:?}"
            );
            // The preference a recommendation says it was generated under is the one the
            // options above were selected with, so a read cannot report a parameter it did not
            // apply.
            if let Some(stored) = &preference {
                assert_eq!(
                    recommendation_for(&body, instance_id)["effectiveRecommendationPreferences"]
                        ["utilizationPreferences"],
                    *stored,
                    "case {case:?}: {instance_id} must report the stored utilization preference"
                );
            }
        }
    }
}

/// Every surface of ONE analysis reports the options the utilization preference leaves: the
/// reader of the recommendation and the projection of an option's metrics apply the same
/// sizing, so neither can report an option the other leaves out.
#[tokio::test]
async fn every_surface_reports_the_options_the_utilization_preference_leaves() {
    // (case, the preference stored, the options expected per instance)
    let rows: Vec<SizingRow> = vec![
        (
            "the lowest threshold with no headroom leaves every candidate the workload fits",
            Some(json!([
                utilization_entry("CpuUtilization", Some("P90"), "PERCENT_0"),
                utilization_entry("MemoryUtilization", None, "PERCENT_10"),
            ])),
            vec![
                (CPU_HEADROOM_BOUND, vec!["m5.xlarge"]),
                (THRESHOLD_BOUND, vec!["m5.xlarge"]),
                (OVERSHOT_PEAK, vec!["m5.xlarge"]),
            ],
        ),
        (
            "the widest headrooms leave some of them out",
            Some(json!([
                utilization_entry("CpuUtilization", Some("P99_5"), "PERCENT_30"),
                utilization_entry("MemoryUtilization", None, "PERCENT_30"),
            ])),
            vec![
                (CPU_HEADROOM_BOUND, Vec::new()),
                (MEMORY_HEADROOM_BOUND, Vec::new()),
                (THRESHOLD_BOUND, Vec::new()),
            ],
        ),
    ];

    for (case, preference, expected) in rows {
        let (router, _engine) = emulator_with_sizing_corpus().await;
        if let Some(stored) = &preference {
            put_preference(&router, &utilization_preference(stored.clone())).await;
        }
        let body = recommendations(&router, case, json!({})).await;
        for (instance_id, wanted) in expected {
            let reported = reported_option_types(case, &body, instance_id);
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected the read of {instance_id} to report {wanted:?}, got \
                 {reported:?}"
            );
            let projected: Vec<String> = projected_options(&router, case, instance_id)
                .await
                .iter()
                .map(|option| {
                    option["recommendedInstanceType"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            assert_eq!(
                projected, wanted,
                "case {case:?}: expected the projection of {instance_id} to report {wanted:?}, \
                 got {projected:?}"
            );
        }
    }
}

/// A projection onto an option the analysed PEAK overshoots stops at the percentage ceiling:
/// the sizing reads the percentile the threshold names, so a lower threshold can leave an
/// option the peak itself would not fit on — and a utilization percentage cannot pass 100.
#[tokio::test]
async fn a_projection_the_analysed_peak_overshoots_stops_at_the_ceiling() {
    let (router, _engine) = emulator_with_sizing_corpus().await;
    let case = "the peak overshoots the option the threshold admitted";
    put_preference(
        &router,
        &utilization_preference(json!([
            utilization_entry("CpuUtilization", Some("P90"), "PERCENT_0"),
            utilization_entry("MemoryUtilization", None, "PERCENT_10"),
        ])),
    )
    .await;

    let options = projected_options(&router, case, OVERSHOT_PEAK).await;
    let option = options
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected one projected option, got {options:?}"));
    // (the metric, the percentage projected onto 4 of the 8 vCPUs and 16 of the 32 GiB)
    for (metric_name, wanted) in [
        // A 60% peak of 8 vCPUs is 120% of 4 of them, which stops at the ceiling.
        ("Cpu", 100.0),
        // A 20% peak of 32 GiB is 40% of 16.
        ("Memory", 40.0),
    ] {
        let series = option["projectedMetrics"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .find(|metric| metric["name"] == json!(metric_name))
            .unwrap_or_else(|| panic!("case {case:?}: a {metric_name} series, got {option}"));
        let values: Vec<f64> = series["values"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|value| value.as_f64().unwrap_or_default())
            .collect();
        assert!(
            !values.is_empty() && values.iter().all(|value| (value - wanted).abs() < 1e-6),
            "case {case:?}: expected {metric_name} projected at {wanted} everywhere, got \
             {values:?}"
        );
    }
}
