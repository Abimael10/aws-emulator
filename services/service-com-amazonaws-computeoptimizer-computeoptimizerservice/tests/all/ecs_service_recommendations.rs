// SPDX-License-Identifier: CC0-1.0

//! `GetECSServiceRecommendations`, driven end to end over HTTP.
//!
//! The rule these suites pin: an Amazon ECS service recommendation is COMPUTED, for every
//! service the request puts in scope, from state the emulator holds —
//!
//! * the current service configuration is the task size and container definitions the ECS
//!   store holds for the service, and its launch type is the service's own;
//! * the utilization metrics are CloudWatch's own statistics of the service's `AWS/ECS`
//!   `CPUUtilization`/`MemoryUtilization` series over the effective look-back period;
//! * the finding, its reason codes, the recommendation option and the performance risk
//!   follow from sizing each dimension so that its observed PEAK leaves the service's
//!   documented default headroom, the option being the smallest Fargate task size that
//!   holds both dimensions;
//! * the option's savings opportunity is the difference between what the current task size
//!   and the recommended one cost per month at the public Fargate per-vCPU-hour and
//!   per-GB-hour prices, for the tasks the service keeps running, and the recommendation
//!   names the pricing source that estimate came from;
//! * the option's container recommendations are the task definition's own container
//!   reservations carried to the recommended task size;
//! * a service the analysis has no input for is never given an invented recommendation: one
//!   Compute Optimizer does not support (it does not run on Fargate, or its task definition
//!   sets no task size) is reported in `errors`, and one the analysis has merely not reached
//!   — unknown, or with no observation inside the window — is absent from the answer.
//!
//! Every row differs along what that rule generalises over: the observed utilization, the
//! current task size and the form it is registered in, how many tasks the service keeps
//! running, the look-back period in force, and which analysis input is missing.
//!
//! The last suites here pin the same rule through the OTHER surface that serves these
//! recommendations, `ExportECSServiceRecommendations`: the file an export job writes holds one
//! row per recommendation this reader reports at that moment, each cell the value the reader
//! reports for the member its `ExportableECSServiceField` column names — so the reader is the
//! oracle for the file as well, and no row of it is of a service the reader reports nothing
//! for. (The export's job record, destination keys, lifecycle and refusals are
//! `ecs_service_recommendations_export.rs`.)

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::ops::types::EXPORTABLE_E_C_S_SERVICE_FIELD_VALUES;
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::state::{
    EXPORT_JOB_STEP, JOB_STATUS_COMPLETE, RESOURCE_TYPE_ECS_SERVICE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, call_ecs, call_in_region, cell, create_bucket, csv_rows,
    emulator_with_analysed_services, emulator_with_analysed_services_and_engine, error_code,
    export_destination, get_object, opt_in, seed_cloudwatch_series, summary_of, summary_set,
    REGION,
};

/// The account id every ARN the emulator mints carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The two `AWS/ECS` series an ECS service recommendation is computed from.
const CPU_METRIC: &str = "CPUUtilization";
/// See [`CPU_METRIC`].
const MEMORY_METRIC: &str = "MemoryUtilization";

/// The public on-demand Fargate prices every money figure an ECS service recommendation
/// reports is computed from: one vCPU-hour and one GB-hour of Fargate compute, which is how
/// AWS Fargate's pricing is published (per vCPU per hour and per GB per hour).
///
/// Stated here so the expectation is computed independently of the handler; they are the
/// handler's own `FARGATE_PRICE_PER_VCPU_HOUR` / `FARGATE_PRICE_PER_GB_HOUR` and must stay
/// in step with them.
const PRICE_PER_VCPU_HOUR: f64 = 0.040_48;
/// See [`PRICE_PER_VCPU_HOUR`].
const PRICE_PER_GB_HOUR: f64 = 0.004_445;
/// Hours in a month, the factor an hourly price becomes a monthly figure by: 365 × 24 / 12.
const HOURS_PER_MONTH: f64 = 730.0;

/// One row of the analysis table: a service's persisted size, what was observed for it, and
/// the recommendation that follows.
#[derive(Debug)]
struct AnalysisCase {
    name: &'static str,
    service: &'static str,
    /// The task definition's task-level `cpu`/`memory`, in the form it is registered with.
    cpu: &'static str,
    memory: &'static str,
    /// How many tasks the service keeps running — the multiplier between what one task of
    /// the size above costs and what the SERVICE costs.
    desired_count: i64,
    /// What those forms mean in CPU units and MiB — the current configuration the
    /// recommendation must report.
    current_cpu: f64,
    current_memory: f64,
    /// The `CPUUtilization` and `MemoryUtilization` observations, as per cent.
    cpu_values: [f64; 2],
    memory_values: [f64; 2],
    /// The finding, its reason codes IN REPORTED ORDER, and the performance risk.
    finding: &'static str,
    reason_codes: &'static [&'static str],
    risk: &'static str,
    /// The recommended Fargate task size.
    option_cpu: f64,
    option_memory: f64,
}

#[tokio::test]
async fn a_recommendation_is_computed_from_the_service_and_its_observed_utilization() {
    let (router, engine) = emulator_with_analysed_services_and_engine();
    opt_in(&router).await;
    let cluster = "analysis";
    create_cluster(&router, cluster, "setup").await;
    let published_at = now_secs() - 3_600;

    // The rule generalises over the observed utilization and the current task size, so the
    // rows differ along exactly those — including a size registered in the `vCPU`/`GB`
    // string forms, and one whose recommendation has to leave the 4096-CPU-unit row of the
    // Fargate task size catalog for the 8192 one.
    let cases = [
        AnalysisCase {
            name: "both dimensions can be sized down",
            service: "overprovisioned",
            cpu: "1024",
            memory: "2048",
            desired_count: 1,
            current_cpu: 1024.0,
            current_memory: 2048.0,
            cpu_values: [20.0, 40.0],
            memory_values: [30.0, 40.0],
            finding: "Overprovisioned",
            reason_codes: &["CPUOverprovisioned", "MemoryOverprovisioned"],
            risk: "VeryLow",
            option_cpu: 512.0,
            option_memory: 1024.0,
        },
        AnalysisCase {
            name: "a cpu peak above the target sizes the task up",
            service: "cpubound",
            cpu: "1024",
            memory: "2048",
            desired_count: 4,
            current_cpu: 1024.0,
            current_memory: 2048.0,
            cpu_values: [60.0, 95.0],
            memory_values: [40.0, 50.0],
            finding: "Underprovisioned",
            reason_codes: &["CPUUnderprovisioned"],
            risk: "Medium",
            option_cpu: 2048.0,
            option_memory: 4096.0,
        },
        AnalysisCase {
            name: "a task already at the smallest sufficient size is optimized",
            service: "optimized",
            cpu: "256",
            memory: "512",
            desired_count: 2,
            current_cpu: 256.0,
            current_memory: 512.0,
            cpu_values: [50.0, 70.0],
            memory_values: [60.0, 70.0],
            finding: "Optimized",
            reason_codes: &[],
            risk: "VeryLow",
            option_cpu: 256.0,
            option_memory: 512.0,
        },
        AnalysisCase {
            name: "both dimensions above the target",
            service: "saturated",
            cpu: "512",
            memory: "1024",
            desired_count: 2,
            current_cpu: 512.0,
            current_memory: 1024.0,
            cpu_values: [90.0, 99.0],
            memory_values: [95.0, 99.0],
            finding: "Underprovisioned",
            reason_codes: &["CPUUnderprovisioned", "MemoryUnderprovisioned"],
            risk: "High",
            option_cpu: 1024.0,
            option_memory: 2048.0,
        },
        AnalysisCase {
            name: "only memory is excessive, size given as vCPU and GB",
            service: "memoryheavy",
            cpu: "1 vCPU",
            memory: "8GB",
            desired_count: 3,
            current_cpu: 1024.0,
            current_memory: 8192.0,
            cpu_values: [70.0, 80.0],
            memory_values: [5.0, 10.0],
            finding: "Overprovisioned",
            reason_codes: &["MemoryOverprovisioned"],
            risk: "VeryLow",
            option_cpu: 1024.0,
            option_memory: 2048.0,
        },
        AnalysisCase {
            name: "a memory peak past the 4 vCPU row's largest task memory",
            service: "largest",
            cpu: "4096",
            memory: "30GB",
            desired_count: 1,
            current_cpu: 4096.0,
            current_memory: 30_720.0,
            cpu_values: [40.0, 50.0],
            memory_values: [90.0, 95.0],
            finding: "Underprovisioned",
            reason_codes: &["MemoryUnderprovisioned"],
            risk: "Medium",
            option_cpu: 8192.0,
            option_memory: 36_864.0,
        },
        AnalysisCase {
            // One dimension can be sized DOWN while the other must be sized UP: the
            // reason codes describe each dimension on its own, so the CPU code is
            // reported even though the memory requirement forces the recommended task to
            // a LARGER CPU row than the CPU requirement alone needs.
            name: "cpu can shrink while memory must grow",
            service: "mixed",
            cpu: "4096",
            memory: "8GB",
            desired_count: 7,
            current_cpu: 4096.0,
            current_memory: 8192.0,
            cpu_values: [2.0, 5.0],
            memory_values: [90.0, 95.0],
            finding: "Underprovisioned",
            reason_codes: &["CPUOverprovisioned", "MemoryUnderprovisioned"],
            risk: "Medium",
            option_cpu: 2048.0,
            option_memory: 10_240.0,
        },
        AnalysisCase {
            // A service that keeps NO task running costs nothing, so there is nothing to
            // save however oversized its task size is: the saving is zero, and so is the
            // percentage of a monthly cost of zero.
            name: "an oversized service that keeps no task running",
            service: "notasks",
            cpu: "2048",
            memory: "4096",
            desired_count: 0,
            current_cpu: 2048.0,
            current_memory: 4096.0,
            cpu_values: [10.0, 20.0],
            memory_values: [10.0, 20.0],
            finding: "Overprovisioned",
            reason_codes: &["CPUOverprovisioned", "MemoryOverprovisioned"],
            risk: "VeryLow",
            option_cpu: 512.0,
            option_memory: 1024.0,
        },
    ];

    for test in &cases {
        let case = test.name;
        let definition = register_task_definition(
            &router,
            &format!("{}family", test.service),
            test.cpu,
            test.memory,
            case,
        )
        .await;
        let arn = create_service(
            &router,
            cluster,
            test.service,
            &format!("{}family", test.service),
            "FARGATE",
            test.desired_count,
            case,
        )
        .await;
        publish(
            &engine,
            cluster,
            test.service,
            CPU_METRIC,
            &test.cpu_values,
            published_at,
        );
        publish(
            &engine,
            cluster,
            test.service,
            MEMORY_METRIC,
            &test.memory_values,
            published_at,
        );

        let found = only_recommendation(&router, &arn, case).await;

        assert_eq!(
            found["serviceArn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: expected the recommendation of {arn}, got {found}"
        );
        assert_eq!(
            found["accountId"].as_str(),
            Some(ACCOUNT),
            "case {case:?}: expected the owning account {ACCOUNT}, got {found}"
        );
        assert_eq!(
            found["launchType"].as_str(),
            Some("Fargate"),
            "case {case:?}: expected the service's own launch type, got {found}"
        );
        // The current configuration is the task definition's, not a restatement of the
        // request: the task size, the task definition ARN and the container definitions.
        let configuration = &found["currentServiceConfiguration"];
        assert_eq!(
            configuration["cpu"].as_f64(),
            Some(test.current_cpu),
            "case {case:?}: expected the persisted cpu {}, got {configuration}",
            test.current_cpu
        );
        assert_eq!(
            configuration["memory"].as_f64(),
            Some(test.current_memory),
            "case {case:?}: expected the persisted memory {}, got {configuration}",
            test.current_memory
        );
        assert_eq!(
            configuration["taskDefinitionArn"].as_str(),
            Some(definition.as_str()),
            "case {case:?}: expected the task definition {definition}, got {configuration}"
        );
        assert_eq!(
            configuration["containerConfigurations"],
            json!([{
                "containerName": "web",
                "cpu": 128,
                "memorySizeConfiguration": { "memory": 256, "memoryReservation": 192 },
            }]),
            "case {case:?}: expected the stored container definitions, got {configuration}"
        );
        assert_eq!(
            found["tags"],
            json!([{ "key": "env", "value": "test" }]),
            "case {case:?}: expected the service's own tags, got {found}"
        );

        // The utilization metrics are the observations' statistics, nothing else.
        let metrics = list(&found, "utilizationMetrics", case);
        for (name, values) in [
            ("Cpu", test.cpu_values),
            ("Memory", test.memory_values),
        ] {
            let maximum = values[0].max(values[1]);
            let average = (values[0] + values[1]) / 2.0;
            assert_close(
                metric_value(&metrics, name, "Maximum", case),
                maximum,
                case,
                &format!("the {name} Maximum over the look-back period"),
            );
            assert_close(
                metric_value(&metrics, name, "Average", case),
                average,
                case,
                &format!("the {name} Average over the look-back period"),
            );
        }
        assert_eq!(
            found["lastRefreshTimestamp"].as_f64(),
            Some(published_at as f64),
            "case {case:?}: the analysis is current to its newest observation, got {found}"
        );
        // "The number of days the Amazon ECS service utilization metrics were analyzed":
        // the default look-back period, which no preference overrides here.
        assert_close(
            found["lookbackPeriodInDays"].as_f64().unwrap_or_default(),
            14.0,
            case,
            "the analysed look-back period in days",
        );
        assert_eq!(
            found["effectiveRecommendationPreferences"]["lookBackPeriod"].as_str(),
            Some("DAYS_14"),
            "case {case:?}: expected the effective look-back preference, got {found}"
        );

        assert_eq!(
            found["finding"].as_str(),
            Some(test.finding),
            "case {case:?}: expected the finding {}, got {found}",
            test.finding
        );
        assert_eq!(
            found["findingReasonCodes"],
            json!(test.reason_codes),
            "case {case:?}: expected the reason codes {:?}, got {found}",
            test.reason_codes
        );
        assert_eq!(
            found["currentPerformanceRisk"].as_str(),
            Some(test.risk),
            "case {case:?}: expected the performance risk {}, got {found}",
            test.risk
        );

        // One recommendation option: the smallest Fargate task size that holds both
        // dimensions, with the utilization each observation becomes at that size.
        let options = list(&found, "serviceRecommendationOptions", case);
        assert_eq!(
            options.len(),
            1,
            "case {case:?}: expected exactly one recommendation option, got {found}"
        );
        let option = &options[0];
        assert_eq!(
            option["cpu"].as_f64(),
            Some(test.option_cpu),
            "case {case:?}: expected the recommended cpu {}, got {option}",
            test.option_cpu
        );
        assert_eq!(
            option["memory"].as_f64(),
            Some(test.option_memory),
            "case {case:?}: expected the recommended memory {}, got {option}",
            test.option_memory
        );
        // The money the recommendation is for: what the two task sizes cost per month at
        // the public Fargate prices, for the tasks this service keeps running. A
        // recommendation that costs the same or more saves nothing rather than a negative
        // amount.
        let current_cost = monthly_cost(test.current_cpu, test.current_memory, test.desired_count);
        let option_cost = monthly_cost(test.option_cpu, test.option_memory, test.desired_count);
        let expected_saving = round2((current_cost - option_cost).max(0.0));
        let expected_percentage = if current_cost == 0.0 {
            0.0
        } else {
            round2(expected_saving / current_cost * 100.0)
        };
        let savings = &option["savingsOpportunity"];
        assert!(
            (savings["estimatedMonthlySavings"]["value"]
                .as_f64()
                .unwrap_or(f64::NAN)
                - expected_saving)
                .abs()
                < 0.01,
            "case {case:?}: expected a monthly saving of {expected_saving}, got {savings}"
        );
        assert_eq!(
            savings["estimatedMonthlySavings"]["currency"].as_str(),
            Some("USD"),
            "case {case:?}: expected the saving in USD, got {savings}"
        );
        assert!(
            (savings["savingsOpportunityPercentage"]
                .as_f64()
                .unwrap_or(f64::NAN)
                - expected_percentage)
                .abs()
                < 0.01,
            "case {case:?}: expected a saving of {expected_percentage}% of the current monthly \
             cost, got {savings}"
        );
        // The pricing source those figures came from.
        assert_eq!(
            found["effectiveRecommendationPreferences"]["savingsEstimationMode"]["source"].as_str(),
            Some("PublicPricing"),
            "case {case:?}: expected the pricing source of the estimate, got {found}"
        );
        // No savings-estimation-mode preference is in force here, so no after-discount
        // saving is reported (the preference is what activates it).
        assert!(
            option["savingsOpportunityAfterDiscounts"].is_null(),
            "case {case:?}: no savings-estimation-mode preference is set, so no after-discount \
             saving is reported, got {option}"
        );

        // The container recommendations are the task definition's own reservations carried
        // to the recommended task size, in the share of the task they reserve today.
        assert_eq!(
            option["containerRecommendations"],
            json!([{
                "containerName": "web",
                "cpu": scaled(128.0, test.current_cpu, test.option_cpu),
                "memorySizeConfiguration": {
                    "memory": scaled(256.0, test.current_memory, test.option_memory),
                    "memoryReservation": scaled(192.0, test.current_memory, test.option_memory),
                },
            }]),
            "case {case:?}: expected the container reservations at the recommended task size, \
             got {option}"
        );
        let projected = list(option, "projectedUtilizationMetrics", case);
        for (name, values, current, recommended) in [
            ("Cpu", test.cpu_values, test.current_cpu, test.option_cpu),
            (
                "Memory",
                test.memory_values,
                test.current_memory,
                test.option_memory,
            ),
        ] {
            let maximum = values[0].max(values[1]);
            let entry = metric_entry(&projected, name, "Maximum", case);
            let expected = maximum * current / recommended;
            for bound in ["lowerBoundValue", "upperBoundValue"] {
                assert_close(
                    entry[bound].as_f64().unwrap_or_default(),
                    expected,
                    case,
                    &format!("the projected {name} Maximum {bound}"),
                );
            }
        }
    }

    // A request that names no ARN at all reports every analysed service, in ARN order.
    let (status, body) = call(&router, "GetECSServiceRecommendations", json!({})).await;
    assert_eq!(
        status, 200,
        "the inventory sweep: expected 200, got {status}: {body}"
    );
    let mut arns: Vec<String> = list(&body, "ecsServiceRecommendations", "sweep")
        .iter()
        .map(|found| found["serviceArn"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut expected: Vec<String> = cases
        .iter()
        .map(|test| service_arn(cluster, test.service))
        .collect();
    arns.sort();
    expected.sort();
    assert_eq!(
        arns, expected,
        "the inventory sweep: expected every analysed service, got {body}"
    );
}

/// The after-discount savings opportunity is reported only once the account activates the
/// savings-estimation-mode preference for this resource type; the figures are the on-demand
/// ones, because no Savings Plans commitment is recorded against a service's Fargate spend.
#[tokio::test]
async fn after_discount_savings_are_reported_only_when_the_preference_activates_them() {
    let (router, engine) = emulator_with_analysed_services_and_engine();
    opt_in(&router).await;
    let cluster = "discounts";
    create_cluster(&router, cluster, "setup").await;
    let published_at = now_secs() - 3_600;

    // Two oversized services, so every row is observed on more than one recommendation.
    let services = ["alpha", "bravo"];
    for service in services {
        let family = format!("{service}family");
        register_task_definition(&router, &family, "2048", "4096", service).await;
        create_service(&router, cluster, service, &family, "FARGATE", 2, service).await;
        for metric in [CPU_METRIC, MEMORY_METRIC] {
            publish(&engine, cluster, service, metric, &[10.0, 20.0], published_at);
        }
    }

    // Before the preference is set, neither service carries an after-discount saving.
    for service in services {
        let arn = service_arn(cluster, service);
        let found = only_recommendation(&router, &arn, "the preference unset").await;
        let option = list(&found, "serviceRecommendationOptions", service)[0].clone();
        assert!(
            option["savingsOpportunityAfterDiscounts"].is_null(),
            "case {service:?}: the preference is unset, so no after-discount saving is \
             reported, got {option}"
        );
    }

    // The rows are the states the preference can be put in, applied in order: one set for
    // ANOTHER resource type (which this resource type's recommendation does not read), the
    // deactivating value, and the activating one.
    let rows: [(&str, Value, bool); 3] = [
        (
            "the preference of another resource type",
            json!({
                "resourceType": "LambdaFunction",
                "savingsEstimationMode": "AfterDiscounts",
            }),
            false,
        ),
        (
            "BeforeDiscounts deactivates the preference",
            json!({
                "resourceType": "EcsService",
                "savingsEstimationMode": "BeforeDiscounts",
            }),
            false,
        ),
        (
            "AfterDiscounts activates the preference",
            json!({
                "resourceType": "EcsService",
                "savingsEstimationMode": "AfterDiscounts",
            }),
            true,
        ),
    ];
    for (case, request, activated) in rows {
        let (status, body) = call(&router, "PutRecommendationPreferences", request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {request} to answer 200, got {status}: {body}"
        );

        for service in services {
            let arn = service_arn(cluster, service);
            let found = only_recommendation(&router, &arn, case).await;
            let options = list(&found, "serviceRecommendationOptions", case);
            let option = &options[0];
            let on_demand = &option["savingsOpportunity"];
            let after = &option["savingsOpportunityAfterDiscounts"];
            assert!(
                on_demand["estimatedMonthlySavings"]["value"]
                    .as_f64()
                    .is_some_and(|value| value > 0.0),
                "case {case:?} service {service:?}: an oversized service saves money, got {option}"
            );
            if activated {
                // No discount is recorded against Fargate spend, so the after-discount
                // saving is the on-demand one, in the same currency.
                assert_eq!(
                    after, on_demand,
                    "case {case:?} service {service:?}: with no recorded discount the \
                     after-discount saving is the on-demand one, got {option}"
                );
            } else {
                assert!(
                    after.is_null(),
                    "case {case:?} service {service:?}: the preference does not activate an \
                     after-discount saving here, got {option}"
                );
            }
        }
    }
}

/// One row of the no-analysis table: which analysis input is missing, and how the request
/// is answered for the service that misses it.
#[derive(Debug)]
struct ErrorCase {
    name: &'static str,
    service: &'static str,
    /// The ECS launch type the service is created with, or `None` to create no service.
    launch_type: Option<&'static str>,
    /// Which series to publish, and how long ago.
    publish: &'static [&'static str],
    age_secs: i64,
    /// The `GetRecommendationError$code` the request is answered with, or `None` when the
    /// service is simply ABSENT from the answer: `errors` carries the resources the
    /// analysis REJECTED as unsupported, not the ones it has not analysed yet.
    code: Option<&'static str>,
}

#[tokio::test]
async fn a_service_with_no_analysis_input_is_absent_or_reported_as_unsupported() {
    let (router, engine) = emulator_with_analysed_services_and_engine();
    opt_in(&router).await;
    let cluster = "errors";
    create_cluster(&router, cluster, "setup").await;

    // A recommendation needs an ACTIVE Fargate service and both of its series inside the
    // look-back window; the rows differ along which of those is missing.
    let cases = [
        ErrorCase {
            name: "no such service",
            service: "missing",
            launch_type: None,
            publish: &[],
            age_secs: 3_600,
            code: None,
        },
        ErrorCase {
            name: "the service does not run on Fargate",
            service: "oncontainerinstances",
            launch_type: Some("EC2"),
            publish: &[CPU_METRIC, MEMORY_METRIC],
            age_secs: 3_600,
            code: Some("UnsupportedResource"),
        },
        ErrorCase {
            name: "nothing was ever published for the service",
            service: "unobserved",
            launch_type: Some("FARGATE"),
            publish: &[],
            age_secs: 3_600,
            code: None,
        },
        ErrorCase {
            name: "only one of the two series was published",
            service: "cpuonly",
            launch_type: Some("FARGATE"),
            publish: &[CPU_METRIC],
            age_secs: 3_600,
            code: None,
        },
        ErrorCase {
            name: "every observation is older than the look-back period",
            service: "stale",
            launch_type: Some("FARGATE"),
            publish: &[CPU_METRIC, MEMORY_METRIC],
            // 20 days back, which the default DAYS_14 window does not reach.
            age_secs: 20 * 86_400,
            code: None,
        },
    ];

    for test in &cases {
        let case = test.name;
        let family = format!("{}family", test.service);
        if let Some(launch_type) = test.launch_type {
            register_task_definition(&router, &family, "512", "1024", case).await;
            create_service(&router, cluster, test.service, &family, launch_type, 1, case).await;
        }
        for metric in test.publish {
            publish(
                &engine,
                cluster,
                test.service,
                metric,
                &[50.0, 60.0],
                now_secs() - test.age_secs,
            );
        }

        let arn = service_arn(cluster, test.service);
        let (status, body) = call(
            &router,
            "GetECSServiceRecommendations",
            json!({ "serviceArns": [arn] }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status}: {body}");
        assert!(
            list(&body, "ecsServiceRecommendations", case).is_empty(),
            "case {case:?}: expected no recommendation to be invented, got {body}"
        );
        let errors = list(&body, "errors", case);
        match test.code {
            Some(code) => {
                assert_eq!(
                    errors.len(),
                    1,
                    "case {case:?}: expected exactly one error entry, got {body}"
                );
                assert_eq!(
                    errors[0]["identifier"].as_str(),
                    Some(arn.as_str()),
                    "case {case:?}: expected the error to name {arn}, got {body}"
                );
                assert_eq!(
                    errors[0]["code"].as_str(),
                    Some(code),
                    "case {case:?}: expected the code {code}, got {body}"
                );
                assert!(
                    errors[0]["message"]
                        .as_str()
                        .is_some_and(|message| message.contains(&arn)),
                    "case {case:?}: expected a message naming {arn}, got {body}"
                );
            }
            None => assert!(
                errors.is_empty(),
                "case {case:?}: a service with no recommendation yet is absent from the \
                 answer rather than an error of the request, got {body}"
            ),
        }
    }

    // `maxResults` bounds the whole response: a page of services Compute Optimizer does not
    // support carries at most that many error entries, plus a token for the rest.
    let unsupported = ["notfargateone", "notfargatetwo", "notfargatethree"];
    for service in unsupported {
        let family = format!("{service}family");
        register_task_definition(&router, &family, "512", "1024", service).await;
        create_service(&router, cluster, service, &family, "EC2", 1, service).await;
    }
    let requested: Vec<String> = unsupported
        .iter()
        .map(|service| service_arn(cluster, service))
        .collect();
    let (status, body) = call(
        &router,
        "GetECSServiceRecommendations",
        json!({ "serviceArns": requested, "maxResults": 1 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a bounded page of error entries: expected 200, got {status}: {body}"
    );
    assert_eq!(
        list(&body, "errors", "a bounded page of error entries").len(),
        1,
        "a bounded page of error entries: maxResults bounds the error channel too, got {body}"
    );
    assert!(
        body["nextToken"].is_string(),
        "a bounded page of error entries: expected a token for the remaining services, got {body}"
    );

    // The window is the EFFECTIVE look-back period, so widening the preference to DAYS_32
    // brings the 20-day-old observations of the `stale` service into the analysis — the
    // same request, answered from the same observations, under a wider window.
    let (status, body) = call(
        &router,
        "PutRecommendationPreferences",
        json!({ "resourceType": "EcsService", "lookBackPeriod": "DAYS_32" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "widening the look-back preference: expected 200, got {status}: {body}"
    );
    let arn = service_arn(cluster, "stale");
    let found = only_recommendation(&router, &arn, "a wider look-back period").await;
    assert_close(
        found["lookbackPeriodInDays"].as_f64().unwrap_or_default(),
        32.0,
        "a wider look-back period",
        "the analysed look-back period in days",
    );
    assert_eq!(
        found["effectiveRecommendationPreferences"]["lookBackPeriod"].as_str(),
        Some("DAYS_32"),
        "a wider look-back period: expected the stored preference to be reported, got {found}"
    );
}

#[tokio::test]
async fn a_malformed_request_is_refused_and_an_unenrolled_account_has_no_analysis() {
    let router = emulator_with_analysed_services();
    let arn = service_arn("any", "service");

    // Until the account is opted in there is no analysis at all, whatever the request asks
    // for — the gate every read of this service shares.
    let (status, body) = call(&router, "GetECSServiceRecommendations", json!({})).await;
    assert_eq!(
        status, 403,
        "an account that is not opted in: expected 403, got {status}: {body}"
    );
    assert!(
        error_code(&body).contains("OptInRequiredException"),
        "an account that is not opted in: expected OptInRequiredException, got {body}"
    );

    opt_in(&router).await;

    // Every row is a request the service defines no answer for; each must be refused with
    // the modelled 400, never with an empty page — and with the ONE bad-input error class
    // this service models, `InvalidParameterValueException`, whether the member is one the
    // MODEL constrains (a filter name outside `ECSServiceRecommendationFilterName`, refused
    // by the constraint walk before the handler runs) or one the model leaves open (refused
    // by the handler itself).
    let cases: [(&str, Value, &str); 7] = [
        (
            "two account ids",
            json!({ "accountIds": [ACCOUNT, "111122223333"] }),
            "InvalidParameterValueException",
        ),
        (
            "an account id that is not 12 digits",
            json!({ "accountIds": ["12345"] }),
            "InvalidParameterValueException",
        ),
        (
            "a service ARN that is not an ECS service ARN",
            json!({ "serviceArns": [format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/i-0123456789abcdef0")] }),
            "InvalidParameterValueException",
        ),
        (
            "a service ARN without its cluster",
            json!({ "serviceArns": [format!("arn:aws:ecs:{REGION}:{ACCOUNT}:service/onlyaname")] }),
            "InvalidParameterValueException",
        ),
        (
            "a filter name the service does not define",
            json!({ "filters": [{ "name": "LaunchType", "values": ["Fargate"] }] }),
            "InvalidParameterValueException",
        ),
        (
            "a filter value outside the finding enum",
            json!({ "filters": [{ "name": "Finding", "values": ["NotOptimized"] }] }),
            "InvalidParameterValueException",
        ),
        (
            "a nextToken this service never issued",
            json!({ "nextToken": "not-a-token" }),
            "InvalidParameterValueException",
        ),
    ];
    for (case, request, expected) in cases {
        let (status, body) = call(&router, "GetECSServiceRecommendations", request.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {request} to be refused with 400, got {status}: {body}"
        );
        assert!(
            error_code(&body).contains(expected),
            "case {case:?}: expected {expected}, got {body}"
        );
    }

    // A well-formed request for a service nothing holds is NOT a refusal, and not an error
    // entry either: the service has no recommendation yet, so it is absent from a successful
    // empty answer.
    let (status, body) = call(
        &router,
        "GetECSServiceRecommendations",
        json!({ "serviceArns": [arn] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a well-formed ARN for an unknown service: expected 200, got {status}: {body}"
    );
    assert!(
        list(&body, "ecsServiceRecommendations", "unknown service").is_empty(),
        "a well-formed ARN for an unknown service: expected no recommendation, got {body}"
    );
    assert!(
        list(&body, "errors", "unknown service").is_empty(),
        "a well-formed ARN for an unknown service: expected no error entry, got {body}"
    );
}

#[tokio::test]
async fn recommendations_are_paged_in_arn_order_and_filtered_by_finding() {
    let (router, engine) = emulator_with_analysed_services_and_engine();
    opt_in(&router).await;
    let cluster = "paging";
    create_cluster(&router, cluster, "setup").await;
    let published_at = now_secs() - 3_600;

    // Three services: two are oversized for what they run, one is short of CPU.
    let services: [(&str, [f64; 2]); 3] = [
        ("alpha", [20.0, 30.0]),
        ("bravo", [20.0, 30.0]),
        ("charlie", [90.0, 99.0]),
    ];
    for (service, cpu_values) in services {
        let family = format!("{service}family");
        register_task_definition(&router, &family, "1024", "2048", service).await;
        create_service(&router, cluster, service, &family, "FARGATE", 1, service).await;
        publish(
            &engine,
            cluster,
            service,
            CPU_METRIC,
            &cpu_values,
            published_at,
        );
        publish(
            &engine,
            cluster,
            service,
            MEMORY_METRIC,
            &[20.0, 30.0],
            published_at,
        );
    }

    // Every page size walks the same sequence, in ARN order, and the last page carries no
    // token.
    for page_size in [1_usize, 2, 3] {
        let case = format!("maxResults {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = json!({ "maxResults": page_size });
            if let Some(token) = &token {
                request["nextToken"] = json!(token);
            }
            let (status, body) = call(&router, "GetECSServiceRecommendations", request).await;
            assert_eq!(status, 200, "case {case:?}: expected 200, got {status}: {body}");
            let page = list(&body, "ecsServiceRecommendations", &case);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page may hold at most {page_size} recommendations, got {body}"
            );
            seen.extend(
                page.iter()
                    .map(|found| found["serviceArn"].as_str().unwrap_or_default().to_string()),
            );
            token = body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        let expected: Vec<String> = services
            .iter()
            .map(|(service, _)| service_arn(cluster, service))
            .collect();
        assert_eq!(
            seen, expected,
            "case {case:?}: expected every recommendation once, in ARN order"
        );
    }

    // A filter selects by the computed finding and by a computed reason code — not by
    // anything the request supplied.
    for (case, filters, expected) in [
        (
            "Finding=Overprovisioned",
            json!([{ "name": "Finding", "values": ["Overprovisioned"] }]),
            vec![
                service_arn(cluster, "alpha"),
                service_arn(cluster, "bravo"),
            ],
        ),
        (
            "FindingReasonCode=CPUUnderprovisioned",
            json!([{ "name": "FindingReasonCode", "values": ["CPUUnderprovisioned"] }]),
            vec![service_arn(cluster, "charlie")],
        ),
        (
            "Finding=Optimized matches nothing here",
            json!([{ "name": "Finding", "values": ["Optimized"] }]),
            Vec::new(),
        ),
    ] {
        let (status, body) = call(
            &router,
            "GetECSServiceRecommendations",
            json!({ "filters": filters }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status}: {body}");
        let selected: Vec<String> = list(&body, "ecsServiceRecommendations", case)
            .iter()
            .map(|found| found["serviceArn"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            selected, expected,
            "case {case:?}: expected exactly {expected:?}, got {body}"
        );
    }
}

/// Compute Optimizer is a REGIONAL service: the region a request names (its SigV4
/// credential scope) decides which Amazon ECS services are in scope, so a service is
/// analysed only at the endpoint its own ARN names. The rows are other regions the same
/// account can address — the rule holds for each of them, not just one.
#[tokio::test]
async fn only_the_services_of_the_requested_region_are_analysed() {
    let (router, engine) = emulator_with_analysed_services_and_engine();
    opt_in(&router).await;
    let cluster = "regional";
    create_cluster(&router, cluster, "setup").await;
    let family = "regionalfamily";
    register_task_definition(&router, family, "512", "1024", "setup").await;
    let arn = create_service(&router, cluster, "checkout", family, "FARGATE", 1, "setup").await;
    let published_at = now_secs() - 3_600;
    for metric in [CPU_METRIC, MEMORY_METRIC] {
        publish(&engine, cluster, "checkout", metric, &[40.0, 50.0], published_at);
    }

    // At the endpoint the service's ARN names, the sweep reports exactly it.
    let case = format!("the sweep at {REGION}");
    let (status, body) = call(&router, "GetECSServiceRecommendations", json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status}: {body}"
    );
    let reported = list(&body, "ecsServiceRecommendations", &case);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected the one service of this region, got {body}"
    );
    assert_eq!(
        reported[0]["serviceArn"].as_str(),
        Some(arn.as_str()),
        "case {case:?}: expected {arn}, got {body}"
    );

    for region in ["eu-west-1", "ap-southeast-2", "us-west-2"] {
        // The same sweep at another endpoint reaches none of this region's services.
        let case = format!("the sweep at {region}");
        let (status, body) = call_in_region(
            &router,
            region,
            "GetECSServiceRecommendations",
            json!({}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status}: {body}"
        );
        assert!(
            list(&body, "ecsServiceRecommendations", &case).is_empty(),
            "case {case:?}: a service of another region is not analysed here, got {body}"
        );

        // And an ARN naming another region names no service of THIS endpoint, so it is
        // absent from the answer exactly as an ARN naming nothing is.
        let foreign = format!("arn:aws:ecs:{region}:{ACCOUNT}:service/{cluster}/checkout");
        let case = format!("an ARN naming {region}");
        let (status, body) = call(
            &router,
            "GetECSServiceRecommendations",
            json!({ "serviceArns": [&foreign] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status}: {body}"
        );
        assert!(
            list(&body, "ecsServiceRecommendations", &case).is_empty(),
            "case {case:?}: expected no recommendation, got {body}"
        );
        assert!(
            list(&body, "errors", &case).is_empty(),
            "case {case:?}: expected no error entry for {foreign}, got {body}"
        );
    }
}

/// Well-formed 12-digit account ids that are NOT the account this emulator answers for: the
/// id the report cites, the documentation's own example account, and the extremes of the
/// 12-digit range ("a 12-digit Amazon Web Services account ID", `Scope$value`).
const OTHER_ACCOUNTS: [&str; 4] = ["210987654321", "111122223333", "000000000001", "999999999999"];

/// An Amazon ECS service recommendation is reported only for the account the request asks
/// about: the one `accountIds` names, or the caller's own when it names none. The account
/// that OWNS a service is the account field of the service's own ARN, so a service record
/// held under another account's ARN is analysed for no caller of this account — and a
/// well-formed id naming another account is REFUSED rather than used to narrow the answer,
/// because this emulator presents one standalone account and records no organization.
///
/// The rows are well-formed ids of other accounts ([`OTHER_ACCOUNTS`]); each moves one
/// service of this region's inventory under that account and asks both ways.
#[tokio::test]
async fn only_the_services_of_the_requested_account_are_analysed() {
    assert!(
        !OTHER_ACCOUNTS.contains(&ACCOUNT),
        "the other-account rows must not name the account this service answers for \
         ({ACCOUNT}), got {OTHER_ACCOUNTS:?}"
    );
    let (router, engine) = emulator_with_analysed_services_and_engine();
    opt_in(&router).await;
    let cluster = "ownership";
    create_cluster(&router, cluster, "setup").await;
    let family = "ownershipfamily";
    register_task_definition(&router, family, "1024", "2048", "setup").await;
    let published_at = now_secs() - 3_600;
    let mut both = Vec::new();
    for service in ["kept", "handedover"] {
        both.push(create_service(&router, cluster, service, family, "FARGATE", 1, "setup").await);
        for metric in [CPU_METRIC, MEMORY_METRIC] {
            publish(&engine, cluster, service, metric, &[20.0, 30.0], published_at);
        }
    }
    both.sort();
    let kept = vec![service_arn(cluster, "kept")];

    // Both services belong to the account this endpoint answers for, so the sweep analyses
    // both: every row below changes exactly one thing, which account owns one of them.
    assert_eq!(
        swept(&router, "both services of this account").await,
        both,
        "case \"both services of this account\": expected every service of this account to be \
         analysed, got a different set"
    );

    for account_id in OTHER_ACCOUNTS {
        own_service_in_account(&engine, cluster, "handedover", account_id);

        let case = format!("a service owned by {account_id}");
        assert_eq!(
            swept(&router, &case).await,
            kept,
            "case {case:?}: a service another account owns is analysed for none of this \
             account's requests"
        );

        // Asking for that account by name is refused, so the service is not reachable by
        // naming its owner either.
        let (status, body) = call(
            &router,
            "GetECSServiceRecommendations",
            json!({ "accountIds": [account_id] }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected naming another account to be refused with 400, got \
             {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {body}"
        );

        // Naming this account explicitly reads exactly what the sweep read.
        let (status, body) = call(
            &router,
            "GetECSServiceRecommendations",
            json!({ "accountIds": [ACCOUNT] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 for this account, got {status}: {body}"
        );
        let reported: Vec<String> = list(&body, "ecsServiceRecommendations", &case)
            .iter()
            .map(|found| found["serviceArn"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            reported, kept,
            "case {case:?}: naming this account reads this account's services only, got {body}"
        );
    }

    // Handed back, the service is analysed again — so the rows above left it out for the
    // account that owned it, not because moving it damaged the record.
    own_service_in_account(&engine, cluster, "handedover", ACCOUNT);
    assert_eq!(
        swept(&router, "handed back to this account").await,
        both,
        "case \"handed back to this account\": expected every service of this account to be \
         analysed again, got a different set"
    );
}

/// The `serviceArn`s one sweep (a request naming no ARN and no account) reports, in ARN
/// order.
async fn swept(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = call(router, "GetECSServiceRecommendations", json!({})).await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status}: {body}");
    let mut reported: Vec<String> = list(&body, "ecsServiceRecommendations", case)
        .iter()
        .map(|found| found["serviceArn"].as_str().unwrap_or_default().to_string())
        .collect();
    reported.sort();
    reported
}

/// Record one Amazon ECS service of the inventory as owned by `account_id`, by the ARN its
/// record carries.
///
/// No call of this endpoint can do it: an ARN names the account that owns the resource, and
/// the ECS emulator mints every service ARN for the one account it serves — so the state a
/// cross-account inventory would have is seeded here, the way the AWS-reserved utilization
/// series is ([`publish`]), and read back over the wire like any client.
fn own_service_in_account(
    engine: &Arc<engine::InMemoryEngine>,
    cluster: &str,
    service: &str,
    account_id: &str,
) {
    use service_com_amazonaws_ecs_amazonec2containerservicev20141113::state as ecs;

    let inventory = engine.state::<ecs::ComAmazonawsEcsAmazonec2containerservicev20141113State>();
    let mut inventory = inventory.lock();
    let record = inventory
        .services
        .get_mut(&(cluster.to_string(), service.to_string()))
        .unwrap_or_else(|| panic!("the ECS store holds the service {service} of {cluster}"));
    record.arn = format!("arn:aws:ecs:{REGION}:{account_id}:service/{cluster}/{service}");
}

/// The ARN the ECS emulator mints for one service.
fn service_arn(cluster: &str, service: &str) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:service/{cluster}/{service}")
}

/// The current instant, as whole epoch seconds.
fn now_secs() -> i64 {
    i64::try_from(
        ::std::time::SystemTime::now()
            .duration_since(::std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_secs(),
    )
    .expect("the epoch seconds fit an i64")
}

/// One ECS call that must succeed, returning its body.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = call_ecs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status}: {parsed}"
    );
    parsed
}

/// Create one ECS cluster.
async fn create_cluster(router: &MultiServiceRouter, cluster: &str, case: &str) {
    ecs_ok(
        router,
        "CreateCluster",
        json!({ "clusterName": cluster }),
        case,
    )
    .await;
}

/// Register one Fargate-compatible task definition with the given task-level size,
/// returning its ARN.
async fn register_task_definition(
    router: &MultiServiceRouter,
    family: &str,
    cpu: &str,
    memory: &str,
    case: &str,
) -> String {
    let body = ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": cpu,
            "memory": memory,
            "containerDefinitions": [{
                "name": "web",
                "image": "public.ecr.aws/nginx/nginx:latest",
                "cpu": 128,
                "memory": 256,
                "memoryReservation": 192,
            }],
        }),
        case,
    )
    .await;
    body["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a taskDefinitionArn, got {body}"))
        .to_string()
}

/// Create one ECS service running `family` under `launch_type`, returning its ARN.
async fn create_service(
    router: &MultiServiceRouter,
    cluster: &str,
    service: &str,
    family: &str,
    launch_type: &str,
    desired_count: i64,
    case: &str,
) -> String {
    let body = ecs_ok(
        router,
        "CreateService",
        json!({
            "cluster": cluster,
            "serviceName": service,
            "taskDefinition": family,
            "desiredCount": desired_count,
            "launchType": launch_type,
            // The revision above is in the `awsvpc` network mode, whose tasks each get
            // their own elastic network interface, so the launch says which subnets and
            // security groups that interface takes.
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": ["subnet-0123456789abcdef0"],
                    "securityGroups": ["sg-0123456789abcdef0"],
                }
            },
            "tags": [{ "key": "env", "value": "test" }],
        }),
        case,
    )
    .await;
    body["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a serviceArn, got {body}"))
        .to_string()
}

/// Record `values` of one `AWS/ECS` series of one service, at `at`.
///
/// `AWS/ECS` is a namespace AWS reserves for the service that owns the metric, and no client
/// may publish into it (`PutMetricData` refuses a `Namespace` beginning with `AWS/`), so the
/// utilization these rows analyse is seeded into the metric store the way ECS writes it.
fn publish(
    engine: &Arc<engine::InMemoryEngine>,
    cluster: &str,
    service: &str,
    metric_name: &str,
    values: &[f64],
    at: i64,
) {
    #[allow(clippy::cast_precision_loss)]
    seed_cloudwatch_series(
        engine,
        "AWS/ECS",
        metric_name,
        &[("ClusterName", cluster), ("ServiceName", service)],
        Some("Percent"),
        at as f64,
        values,
    );
}

/// The one recommendation a request for `arn` must answer with, asserting it reports no
/// error for it.
async fn only_recommendation(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let (status, body) = call(
        router,
        "GetECSServiceRecommendations",
        json!({ "serviceArns": [arn] }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status}: {body}");
    assert!(
        list(&body, "errors", case).is_empty(),
        "case {case:?}: expected no error entry, got {body}"
    );
    let found = list(&body, "ecsServiceRecommendations", case);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one recommendation, got {body}"
    );
    found[0].clone()
}

/// A list member of a response body.
fn list(body: &Value, member: &str, case: &str) -> Vec<Value> {
    body[member]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a list {member}, got {body}"))
        .clone()
}

/// The entry of a utilization-metric list for one metric name and statistic.
fn metric_entry(metrics: &[Value], name: &str, statistic: &str, case: &str) -> Value {
    metrics
        .iter()
        .find(|entry| {
            entry["name"].as_str() == Some(name) && entry["statistic"].as_str() == Some(statistic)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a {name} {statistic} entry, got {metrics:?}")
        })
        .clone()
}

/// The `value` of one utilization metric.
fn metric_value(metrics: &[Value], name: &str, statistic: &str, case: &str) -> f64 {
    metric_entry(metrics, name, statistic, case)["value"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} {statistic} value"))
}

/// Compare two computed utilization values, which are quotients of observations and
/// capacities rather than exact decimals.
fn assert_close(actual: f64, expected: f64, case: &str, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "case {case:?}: expected {what} to be {expected}, got {actual}"
    );
}

/// What a month of `tasks` tasks of a `cpu`/`memory` Fargate task size costs, at the public
/// per-vCPU-hour and per-GB-hour prices: 1024 CPU units is one vCPU and 1024 MiB is one GB.
fn monthly_cost(cpu: f64, memory: f64, tasks: i64) -> f64 {
    let per_task_hour =
        cpu / 1024.0 * PRICE_PER_VCPU_HOUR + memory / 1024.0 * PRICE_PER_GB_HOUR;
    round2(tasks as f64 * per_task_hour * HOURS_PER_MONTH)
}

/// A money figure or a percentage, to whole cents.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// A container reservation of `reserved` out of a task of `current` capacity, at a task of
/// `recommended` capacity: the same share of the task in whole units, and never nothing.
fn scaled(reserved: f64, current: f64, recommended: f64) -> i64 {
    ((reserved * recommended / current).round() as i64).max(1)
}

// ---------------------------------------------------------------------------
// The SUMMARY of these recommendations: `GetRecommendationSummaries` counts a resource type
// whose recommendations this service COMPUTES from another service's inventory from that same
// computation, so the `EcsService` row counts exactly the services this reader reports — the
// reader is the oracle for every count, reason code, risk rating and saving of that row, in
// both directions.
// ---------------------------------------------------------------------------

/// The resource type the account's Amazon ECS services are summarised under: the
/// `RecommendationSourceType` `RecommendationSummary$recommendationResourceType` names for them.
const SUMMARY_RESOURCE_TYPE: &str = "EcsService";

/// One Amazon ECS service of the summary table: what the account creates, what is published for
/// it, and what the analysis then has to say about it.
#[derive(Debug)]
struct SummaryCase {
    name: &'static str,
    service: &'static str,
    /// Its Amazon ECS launch type — only a service on Fargate is one Compute Optimizer analyses.
    launch_type: &'static str,
    /// The task definition's task-level `cpu`/`memory`, in the form it is registered with.
    cpu: &'static str,
    memory: &'static str,
    /// What those forms mean in CPU units and MiB, which is what the task's monthly cost is
    /// computed from.
    current_cpu: f64,
    current_memory: f64,
    desired_count: i64,
    /// The `CPUUtilization` and `MemoryUtilization` observations published for it, as per cent;
    /// empty for a service nothing was published for.
    cpu_values: &'static [f64],
    memory_values: &'static [f64],
    /// The finding the analysis reaches for it, or `None` for a service the analysis has no
    /// input for — which neither the reader nor the summary counts.
    finding: Option<&'static str>,
}

/// The `EcsService` summary counts the services this reader reports and nothing else: an account
/// whose Amazon ECS store holds no service is summarised with zero at every finding, and each
/// service the account then creates moves the counts, the reason codes, the risk ratings and the
/// saving to exactly what the reader reports for the services the store holds.
///
/// The rows differ along what that rule generalises over: how many services the store holds (none,
/// then one more per row), the finding each one reaches, the task size and the form it is
/// registered in, how many tasks a service keeps running, and whether the analysis has an input
/// for the service at all — one row runs on EC2, which Compute Optimizer does not analyse, and
/// one has no published utilization, so neither reader counts either of them.
#[tokio::test]
async fn the_ecs_service_summary_counts_the_services_this_reader_reports() {
    let (router, engine) = emulator_with_analysed_services_and_engine();
    opt_in(&router).await;
    let cluster = "summaries";
    create_cluster(&router, cluster, "setup").await;
    let published_at = now_secs() - 3_600;

    // Row: the account's Amazon ECS store holds no service at all. The reader reports none, so
    // the summary counts none — the baseline a resident catalog row would break.
    let empty = assert_summary_counts_the_reader(&router, "no Amazon ECS service at all").await;
    assert!(
        empty.is_empty(),
        "case \"no Amazon ECS service at all\": the ECS store holds no service, so the reader \
         reports none, got {empty:?}"
    );

    let cases = [
        SummaryCase {
            name: "a service both of whose dimensions can be sized down",
            service: "summaryoverprovisioned",
            launch_type: "FARGATE",
            cpu: "1024",
            memory: "2048",
            current_cpu: 1024.0,
            current_memory: 2048.0,
            desired_count: 1,
            cpu_values: &[20.0, 40.0],
            memory_values: &[30.0, 40.0],
            finding: Some("Overprovisioned"),
        },
        SummaryCase {
            name: "a cpu peak above the target, on four tasks",
            service: "summarycpubound",
            launch_type: "FARGATE",
            cpu: "1024",
            memory: "2048",
            current_cpu: 1024.0,
            current_memory: 2048.0,
            desired_count: 4,
            cpu_values: &[60.0, 95.0],
            memory_values: &[40.0, 50.0],
            finding: Some("Underprovisioned"),
        },
        SummaryCase {
            name: "a service already at the smallest sufficient task size",
            service: "summaryoptimized",
            launch_type: "FARGATE",
            cpu: "256",
            memory: "512",
            current_cpu: 256.0,
            current_memory: 512.0,
            desired_count: 2,
            cpu_values: &[50.0, 70.0],
            memory_values: &[60.0, 70.0],
            finding: Some("Optimized"),
        },
        SummaryCase {
            // A second service at the same finding, whose excess is MEMORY — so the memory
            // reason code behind that finding is counted for two of the account's services and
            // no count below is one row restated.
            name: "only memory is excessive, the size given as vCPU and GB",
            service: "summarymemoryheavy",
            launch_type: "FARGATE",
            cpu: "1 vCPU",
            memory: "8GB",
            current_cpu: 1024.0,
            current_memory: 8192.0,
            desired_count: 3,
            cpu_values: &[70.0, 80.0],
            memory_values: &[5.0, 10.0],
            finding: Some("Overprovisioned"),
        },
        SummaryCase {
            // "Compute Optimizer generates recommendations for Amazon ECS services on Fargate":
            // a service on EC2 is one the analysis does not support, so neither reader counts it.
            name: "a service that does not run on Fargate",
            service: "summaryoncontainerinstances",
            launch_type: "EC2",
            cpu: "1024",
            memory: "2048",
            current_cpu: 1024.0,
            current_memory: 2048.0,
            desired_count: 2,
            cpu_values: &[30.0, 40.0],
            memory_values: &[30.0, 40.0],
            finding: None,
        },
        SummaryCase {
            // A service the analysis has merely not reached: nothing was published for it, so
            // there is no utilization to size it against and neither reader counts it.
            name: "a service nothing was published for",
            service: "summaryunobserved",
            launch_type: "FARGATE",
            cpu: "2048",
            memory: "4096",
            current_cpu: 2048.0,
            current_memory: 4096.0,
            desired_count: 5,
            cpu_values: &[],
            memory_values: &[],
            finding: None,
        },
    ];

    // What the services the analysis DOES reach cost their account per month, at the public
    // Fargate prices: the figure the summary states its saving as a percentage of.
    let mut analysed_monthly_cost = 0.0;
    let mut counted = 0;
    for test in &cases {
        let case = test.name;
        let family = format!("{}family", test.service);
        register_task_definition(&router, &family, test.cpu, test.memory, case).await;
        let arn = create_service(
            &router,
            cluster,
            test.service,
            &family,
            test.launch_type,
            test.desired_count,
            case,
        )
        .await;
        for (metric, values) in [
            (CPU_METRIC, test.cpu_values),
            (MEMORY_METRIC, test.memory_values),
        ] {
            if !values.is_empty() {
                publish(&engine, cluster, test.service, metric, values, published_at);
            }
        }
        if test.finding.is_some() {
            analysed_monthly_cost +=
                monthly_cost(test.current_cpu, test.current_memory, test.desired_count);
            counted += 1;
        }

        // The summary agrees with the reader after EVERY service the account creates, so neither
        // a service the reader reports nor one it does not can drift into the counts.
        let reported = assert_summary_counts_the_reader(&router, case).await;
        assert_eq!(
            reported.len(),
            counted,
            "case {case:?}: the reader must report the {counted} analysable service(s) created so \
             far, got {reported:?}"
        );
        let found = reported
            .iter()
            .find(|recommendation| recommendation["serviceArn"] == json!(arn));
        match test.finding {
            Some(finding) => {
                let found = found.unwrap_or_else(|| {
                    panic!("case {case:?}: {arn} must be reported, got {reported:?}")
                });
                assert_eq!(
                    found["finding"],
                    json!(finding),
                    "case {case:?}: expected the finding {finding} for {arn}, got {found}"
                );
            }
            None => assert!(
                found.is_none(),
                "case {case:?}: the analysis has no input for {arn}, so no recommendation is \
                 reported for it, got {reported:?}"
            ),
        }
    }

    // The table spans the whole finding vocabulary of this resource type, so each per-finding
    // count above is observed non-zero rather than all of them zero together.
    let answer = summary_set(&router, "the whole table", json!({})).await;
    let summary = summary_of("the whole table", &answer, SUMMARY_RESOURCE_TYPE);
    for finding in ["Underprovisioned", "Overprovisioned", "Optimized"] {
        assert!(
            summary_count_at(summary, finding, "the whole table") > 0.0,
            "the table must hold a service at {finding} for its count to be observable, got \
             {summary}"
        );
    }

    // And the saving the summary states is that saving "as a percentage of monthly cost", the
    // monthly cost being what the analysed services bill at the public Fargate prices.
    let opportunity = &summary["savingsOpportunity"];
    let saved = summary_number(
        &opportunity["estimatedMonthlySavings"]["value"],
        "the whole table",
    );
    let expected_percentage = round2(saved / analysed_monthly_cost * 100.0);
    let percentage = summary_number(
        &opportunity["savingsOpportunityPercentage"],
        "the whole table",
    );
    assert!(
        (percentage - expected_percentage).abs() < 0.02,
        "the {saved} saved is {expected_percentage}% of the {analysed_monthly_cost} the analysed \
         services cost, got {percentage}"
    );
}

/// Read the `EcsService` summary and the Amazon ECS service recommendations of the same moment,
/// asserting every count the summary states is what the reader reports, and answer those
/// recommendations.
///
/// Every expectation is computed from the reader's own answer, so the reader is the oracle: how
/// many services carry each finding, how many of those name each `FindingReasonCode` the summary
/// counts at, how many run each `CurrentPerformanceRisk`, and what adopting their recommendation
/// options would save.
async fn assert_summary_counts_the_reader(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let reported = swept_recommendations(router, case).await;
    let answer = summary_set(router, case, json!({})).await;
    let summary = summary_of(case, &answer, SUMMARY_RESOURCE_TYPE);

    let counts = summary["summaries"].as_array().cloned().unwrap_or_else(|| {
        panic!("case {case:?}: an Amazon ECS service is counted at each finding, got {summary}")
    });
    let mut total = 0.0;
    for entry in &counts {
        let finding = entry["name"].as_str().unwrap_or_default().to_string();
        let count = summary_number(&entry["value"], case);
        let at_finding: Vec<&Value> = reported
            .iter()
            .filter(|recommendation| recommendation["finding"] == json!(finding))
            .collect();
        assert_eq!(
            count,
            at_finding.len() as f64,
            "case {case:?}: expected the {} service(s) the reader reports at {finding}, got {count}",
            at_finding.len()
        );
        total += count;

        // A reason code states what the recommended task size would change, counted over the
        // services at the finding it explains.
        let reasons = entry["reasonCodeSummaries"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a group the service recommends a configuration for states why \
                     a finding was reached, got {entry}"
                )
            });
        for reason in &reasons {
            let name = reason["name"].as_str().unwrap_or_default().to_string();
            let counted = summary_number(&reason["value"], case);
            let expected = at_finding
                .iter()
                .filter(|recommendation| {
                    recommendation["findingReasonCodes"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                        .iter()
                        .any(|code| *code == json!(name))
                })
                .count();
            assert_eq!(
                counted, expected as f64,
                "case {case:?}: expected the {expected} service(s) the reader reports {name} for \
                 at {finding}, got {counted}"
            );
        }
    }
    assert_eq!(
        total,
        reported.len() as f64,
        "case {case:?}: the summary counts exactly the {} service(s) the reader reports, got \
         {summary}",
        reported.len()
    );

    // Every analysed service runs exactly one performance risk: the one the reader reports for it.
    let ratings = &summary["currentPerformanceRiskRatings"];
    for (member, rating) in [
        ("high", "High"),
        ("medium", "Medium"),
        ("low", "Low"),
        ("veryLow", "VeryLow"),
    ] {
        let expected = reported
            .iter()
            .filter(|recommendation| recommendation["currentPerformanceRisk"] == json!(rating))
            .count();
        assert_eq!(
            summary_number(&ratings[member], case),
            expected as f64,
            "case {case:?}: expected the {expected} service(s) the reader reports at {rating}, got \
             {ratings}"
        );
    }

    // What adopting the recommendations would save: the saving of the options the reader reports.
    let expected_savings = round2(reported.iter().map(option_savings).sum());
    let saved = summary_number(
        &summary["savingsOpportunity"]["estimatedMonthlySavings"]["value"],
        case,
    );
    assert!(
        (saved - expected_savings).abs() < 0.01,
        "case {case:?}: expected the {expected_savings} the reader's options save, got {saved}"
    );
    assert_eq!(
        summary_number(&summary["savingsOpportunity"]["savingsOpportunityPercentage"], case) == 0.0,
        saved == 0.0,
        "case {case:?}: a percentage is zero exactly when the saving is, got {summary}"
    );

    reported
}

/// What one reported recommendation's option would save per month, or `0` for a recommendation
/// whose option saves nothing.
fn option_savings(recommendation: &Value) -> f64 {
    recommendation["serviceRecommendationOptions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .first()
        .and_then(|option| {
            option["savingsOpportunity"]["estimatedMonthlySavings"]["value"].as_f64()
        })
        .unwrap_or_default()
}

/// Every Amazon ECS service recommendation the account holds, as the reader reports them —
/// including none at all, which is what a store holding no service is answered with.
async fn swept_recommendations(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, body) = call(router, "GetECSServiceRecommendations", json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the Amazon ECS service recommendations must be served, got {status}: \
         {body}"
    );
    list(&body, "ecsServiceRecommendations", case)
}

/// One finding count of a summary.
fn summary_count_at(summary: &Value, finding: &str, case: &str) -> f64 {
    summary["summaries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|entry| entry["name"] == json!(finding))
        .map(|entry| summary_number(&entry["value"], case))
        .unwrap_or_else(|| panic!("case {case:?}: {finding} must be counted, got {summary}"))
}

/// A number a summary member carries.
fn summary_number(value: &Value, case: &str) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a number, got {value}"))
}

// ---------------------------------------------------------------------------
// The EXPORT of these recommendations: `ExportECSServiceRecommendations` writes a file of the
// recommendations this reader reports, so the reader is the oracle for every row and every
// cell of it.
// ---------------------------------------------------------------------------

/// The export operation these suites drive.
const EXPORT: &str = "ExportECSServiceRecommendations";

/// The destination bucket the export fixtures write to. Compute Optimizer does not create it,
/// so each fixture creates it through Amazon S3 first.
const EXPORT_BUCKET: &str = "ecs-service-recommendation-export-files";

/// The cluster the exported services run in.
const EXPORT_CLUSTER: &str = "exports";

/// How a cell holding several values separates them in the export file, so one exportable
/// field is still one column. AWS publishes no layout for the file, so this is the emulator's
/// own, asserted here as what a client reading the file sees.
const LIST_SEPARATOR: &str = "|";

/// The column whose cell states an instant rather than text.
const TIMESTAMP_COLUMN: &str = "LastRefreshTimestamp";

/// The one `ExportableECSServiceField` that names a member of an `ECSServiceRecommendation`
/// this analysis records no value for: whether a target-tracking policy scales the service
/// lives in Application Auto Scaling, which the analysis does not read, so the reader reports
/// no `autoScalingConfiguration` and the column is empty in every row.
const UNRECORDED_COLUMN: &str = "CurrentServiceConfigurationAutoScalingConfiguration";

/// An Amazon ECS service ARN no ECS store holds a record of, which this reader therefore reports
/// no recommendation for — so no row of an export file is of it either.
fn unheld_service_arn() -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:service/payments/checkout")
}

/// One Amazon ECS service of the export corpus: what it runs and what was observed on it.
struct ExportedService {
    service: &'static str,
    /// Its ECS launch type — only a Fargate service is one Compute Optimizer analyses.
    launch_type: &'static str,
    /// The task definition's task-level `cpu`/`memory`, in the form it is registered with.
    cpu: &'static str,
    memory: &'static str,
    desired_count: i64,
    /// The `CPUUtilization` and `MemoryUtilization` observations published for it, as per
    /// cent; empty for a service nothing was published for.
    cpu_values: &'static [f64],
    memory_values: &'static [f64],
}

/// The Amazon ECS services the export fixtures hold, spanning what the rule generalises over:
/// a service the analysis would size DOWN (so its option saves money), one already at the
/// smallest sufficient task size, one it would size UP (so its option saves nothing), one
/// Compute Optimizer does not support, and one the analysis has no observation for.
fn exported_services() -> Vec<ExportedService> {
    vec![
        ExportedService {
            service: "overprovisioned",
            launch_type: "FARGATE",
            cpu: "1024",
            memory: "2048",
            desired_count: 2,
            cpu_values: &[20.0, 40.0],
            memory_values: &[30.0, 40.0],
        },
        ExportedService {
            service: "optimized",
            launch_type: "FARGATE",
            cpu: "256",
            memory: "512",
            desired_count: 1,
            cpu_values: &[50.0, 70.0],
            memory_values: &[60.0, 70.0],
        },
        ExportedService {
            service: "underprovisioned",
            launch_type: "FARGATE",
            // Registered in the `vCPU`/`GB` string forms, which the analysis reads as 512 CPU
            // units and 1024 MiB.
            cpu: "0.5 vCPU",
            memory: "1GB",
            desired_count: 3,
            cpu_values: &[90.0, 99.0],
            memory_values: &[95.0, 99.0],
        },
        ExportedService {
            service: "ec2based",
            launch_type: "EC2",
            cpu: "512",
            memory: "1024",
            desired_count: 1,
            cpu_values: &[40.0, 50.0],
            memory_values: &[40.0, 50.0],
        },
        ExportedService {
            service: "unobserved",
            launch_type: "FARGATE",
            cpu: "512",
            memory: "1024",
            desired_count: 1,
            cpu_values: &[],
            memory_values: &[],
        },
    ]
}

/// An emulator holding [`exported_services`] in the Amazon ECS store with their utilization in
/// the CloudWatch store, the destination bucket created through Amazon S3 and the account
/// opted in — everything an export of these recommendations needs, created the way a client
/// creates it.
async fn export_emulator(case: &str) -> MultiServiceRouter {
    let (router, engine) = emulator_with_analysed_services_and_engine();
    opt_in(&router).await;
    create_bucket(&router, EXPORT_BUCKET).await;
    create_cluster(&router, EXPORT_CLUSTER, case).await;
    let published_at = now_secs() - 3_600;
    for service in exported_services() {
        let family = format!("{}family", service.service);
        register_task_definition(&router, &family, service.cpu, service.memory, case).await;
        create_service(
            &router,
            EXPORT_CLUSTER,
            service.service,
            &family,
            service.launch_type,
            service.desired_count,
            case,
        )
        .await;
        for (metric, values) in [
            (CPU_METRIC, service.cpu_values),
            (MEMORY_METRIC, service.memory_values),
        ] {
            if !values.is_empty() {
                publish(
                    &engine,
                    EXPORT_CLUSTER,
                    service.service,
                    metric,
                    values,
                    published_at,
                );
            }
        }
    }
    router
}

/// The recommendations the reader reports, in the order it reports them: the oracle every
/// expected cell below is derived from.
async fn reader_recommendations(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, body) = call(router, "GetECSServiceRecommendations", json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the recommendations: got {status}: {body}"
    );
    assert!(
        body["nextToken"].is_null(),
        "case {case:?}: the analysis fits one page, got {body}"
    );
    let reported = list(&body, "ecsServiceRecommendations", case);
    assert!(
        reported.len() > 1,
        "case {case:?}: the corpus holds several analysed services, got {body}"
    );
    reported
}

/// Export the account's Amazon ECS service recommendations, asserting the call was accepted,
/// and answer the whole response body.
async fn export_ok(router: &MultiServiceRouter, case: &str, request: Value) -> Value {
    let (status, body) = call(router, EXPORT, request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {request} to be accepted, got {status}: {body}"
    );
    body
}

/// Poll the job to `Complete`, the documented export-then-download workflow: the objects
/// appear when the job REACHES its terminal status, and nothing here waits a fixed time for
/// it.
async fn polled_to_complete(router: &MultiServiceRouter, case: &str, job_id: &str) {
    let mut status = String::new();
    for _ in 0..200 {
        let (code, body) = call(
            router,
            "DescribeRecommendationExportJobs",
            json!({"jobIds": [job_id]}),
        )
        .await;
        assert_eq!(code, 200, "case {case:?}: reading job {job_id}: got {body}");
        let job = &body["recommendationExportJobs"][0];
        status = job["status"].as_str().unwrap_or_default().to_string();
        if status == JOB_STATUS_COMPLETE {
            assert!(
                job["failureReason"].is_null(),
                "case {case:?}: a completed export carries no failure reason, got {job}"
            );
            return;
        }
        tokio::time::sleep(EXPORT_JOB_STEP).await;
    }
    panic!("case {case:?}: the job never completed, last status {status:?}");
}

/// Download one object, asserting it was readable, and answer it as text.
async fn downloaded(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) -> String {
    let (status, body) = get_object(router, bucket, key).await;
    assert_eq!(
        status, 200,
        "case {case:?}: downloading {key}: got {status}: {}",
        String::from_utf8_lossy(&body)
    );
    String::from_utf8(body)
        .unwrap_or_else(|error| panic!("case {case:?}: the object is text: {error}"))
}

/// Export with `fields`, wait for the job and answer the file's rows: the header row and one
/// row per recommendation.
async fn exported_rows(
    router: &MultiServiceRouter,
    case: &str,
    fields: Option<&[&str]>,
) -> Vec<Vec<String>> {
    let mut request = json!({"s3DestinationConfig": {"bucket": EXPORT_BUCKET, "keyPrefix": "ecs"}});
    if let Some(fields) = fields {
        request["fieldsToExport"] = json!(fields);
    }
    let body = export_ok(router, case, request).await;
    let job_id = body["jobId"].as_str().unwrap_or_default().to_string();
    let (bucket, key, _) = export_destination(case, &body);
    polled_to_complete(router, case, &job_id).await;
    csv_rows(&downloaded(router, case, &bucket, &key).await)
}

/// A string member of a reported recommendation, or the empty cell where it reports none.
fn text(reported: &Value, member: &str) -> String {
    reported[member].as_str().unwrap_or_default().to_string()
}

/// A figure as the export file states it: at the two decimal places every figure this service
/// reports is rounded to; the empty cell for a member the recommendation does not report.
fn number_cell(value: Option<f64>) -> String {
    value.map_or_else(String::new, |value| round2(value).to_string())
}

/// A list-valued member as one cell ([`LIST_SEPARATOR`]).
fn list_cell(reported: &Value) -> String {
    reported
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect::<Vec<String>>()
                .join(LIST_SEPARATOR)
        })
        .unwrap_or_default()
}

/// The tags a recommendation reports as one cell, each as `key=value`.
fn tags_cell(reported: &Value) -> String {
    reported
        .as_array()
        .map(|tags| {
            tags.iter()
                .map(|tag| {
                    format!(
                        "{}={}",
                        tag["key"].as_str().unwrap_or_default(),
                        tag["value"].as_str().unwrap_or_default()
                    )
                })
                .collect::<Vec<String>>()
                .join(LIST_SEPARATOR)
        })
        .unwrap_or_default()
}

/// A list of containers — the task definition's own, or the ones the recommendation option
/// would run — as one cell: each container's name and the reservations it carries, under the
/// member names the model gives them.
fn containers_cell(reported: &Value) -> String {
    reported
        .as_array()
        .map(|containers| {
            containers
                .iter()
                .map(container_cell)
                .collect::<Vec<String>>()
                .join(LIST_SEPARATOR)
        })
        .unwrap_or_default()
}

/// One container of such a list.
fn container_cell(container: &Value) -> String {
    let mut reservations: Vec<String> = Vec::new();
    if let Some(cpu) = container["cpu"].as_i64() {
        reservations.push(format!("cpu={cpu}"));
    }
    let memory = &container["memorySizeConfiguration"];
    if let Some(value) = memory["memory"].as_i64() {
        reservations.push(format!("memory={value}"));
    }
    if let Some(value) = memory["memoryReservation"].as_i64() {
        reservations.push(format!("memoryReservation={value}"));
    }
    let name = container["containerName"].as_str().unwrap_or_default();
    if reservations.is_empty() {
        return name.to_string();
    }
    format!("{name}:{}", reservations.join(";"))
}

/// The value the reader reports for one named statistic of one named series of a metric list.
fn reported_metric(metrics: &Value, name: &str, statistic: &str, member: &str) -> Option<f64> {
    metrics
        .as_array()?
        .iter()
        .find(|metric| {
            metric["name"].as_str() == Some(name) && metric["statistic"].as_str() == Some(statistic)
        })
        .and_then(|metric| metric[member].as_f64())
}

/// The cell the export file must carry under `column` for the recommendation the READER
/// reports — the whole expectation for a row, derived from the reader's own answer.
///
/// Every value of `ExportableECSServiceField` is covered (`smithy_query query="shape"
/// shape="ExportableECSServiceField"`); the timestamp column is compared as an instant by the
/// caller.
fn expected_cell(column: &str, reported: &Value) -> String {
    let configuration = &reported["currentServiceConfiguration"];
    let option = &reported["serviceRecommendationOptions"][0];
    let preferences = &reported["effectiveRecommendationPreferences"];
    let savings = &option["savingsOpportunity"];
    let after = &option["savingsOpportunityAfterDiscounts"];
    match column {
        "AccountId" => text(reported, "accountId"),
        "ServiceArn" => text(reported, "serviceArn"),
        "LaunchType" => text(reported, "launchType"),
        "Finding" => text(reported, "finding"),
        "FindingReasonCodes" => list_cell(&reported["findingReasonCodes"]),
        "CurrentPerformanceRisk" => text(reported, "currentPerformanceRisk"),
        "LookbackPeriodInDays" => number_cell(reported["lookbackPeriodInDays"].as_f64()),
        "Tags" => tags_cell(&reported["tags"]),
        // The configuration of the current service.
        "CurrentServiceConfigurationCpu" => number_cell(configuration["cpu"].as_f64()),
        "CurrentServiceConfigurationMemory" => number_cell(configuration["memory"].as_f64()),
        "CurrentServiceConfigurationTaskDefinitionArn" => {
            text(configuration, "taskDefinitionArn")
        }
        UNRECORDED_COLUMN => text(configuration, "autoScalingConfiguration"),
        "CurrentServiceContainerConfigurations" => {
            containers_cell(&configuration["containerConfigurations"])
        }
        // The utilization observed on it, under the statistic the column names.
        "UtilizationMetricsCpuMaximum" => number_cell(reported_metric(
            &reported["utilizationMetrics"],
            "Cpu",
            "Maximum",
            "value",
        )),
        "UtilizationMetricsMemoryMaximum" => number_cell(reported_metric(
            &reported["utilizationMetrics"],
            "Memory",
            "Maximum",
            "value",
        )),
        // The recommendation option: the task size, its projection, its containers and what
        // adopting it saves.
        "RecommendationOptionsCpu" => number_cell(option["cpu"].as_f64()),
        "RecommendationOptionsMemory" => number_cell(option["memory"].as_f64()),
        "RecommendationOptionsProjectedUtilizationMetricsCpuMaximum" => {
            number_cell(reported_metric(
                &option["projectedUtilizationMetrics"],
                "Cpu",
                "Maximum",
                "upperBoundValue",
            ))
        }
        "RecommendationOptionsProjectedUtilizationMetricsMemoryMaximum" => {
            number_cell(reported_metric(
                &option["projectedUtilizationMetrics"],
                "Memory",
                "Maximum",
                "upperBoundValue",
            ))
        }
        "RecommendationOptionsContainerRecommendations" => {
            containers_cell(&option["containerRecommendations"])
        }
        "RecommendationOptionsSavingsOpportunityPercentage" => {
            number_cell(savings["savingsOpportunityPercentage"].as_f64())
        }
        "RecommendationOptionsEstimatedMonthlySavingsCurrency" => {
            text(&savings["estimatedMonthlySavings"], "currency")
        }
        "RecommendationOptionsEstimatedMonthlySavingsValue" => {
            number_cell(savings["estimatedMonthlySavings"]["value"].as_f64())
        }
        "RecommendationOptionsSavingsOpportunityAfterDiscountsPercentage" => {
            number_cell(after["savingsOpportunityPercentage"].as_f64())
        }
        "RecommendationOptionsEstimatedMonthlySavingsCurrencyAfterDiscounts" => {
            text(&after["estimatedMonthlySavings"], "currency")
        }
        "RecommendationOptionsEstimatedMonthlySavingsValueAfterDiscounts" => {
            number_cell(after["estimatedMonthlySavings"]["value"].as_f64())
        }
        // The preferences the recommendation says it was generated under.
        "EffectiveRecommendationPreferencesLookBackPeriod" => text(preferences, "lookBackPeriod"),
        "EffectiveRecommendationPreferencesSavingsEstimationMode" => {
            text(&preferences["savingsEstimationMode"], "source")
        }
        column => panic!("column {column:?} is not covered by this expectation"),
    }
}

/// The instant one cell of the timestamp column states, as a count of whole milliseconds —
/// `None` for a cell that states no instant at all.
///
/// The file states the instant in RFC 3339 while a modelled timestamp crosses the wire as a
/// JSON number, so both sides are compared at the millisecond resolution an AWS JSON timestamp
/// states.
fn stated_millis(cell: &str) -> Option<i64> {
    let at = ::aws_smithy_types::DateTime::from_str(
        cell,
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .ok()?;
    Some(
        at.secs()
            .saturating_mul(1_000)
            .saturating_add(i64::from(at.subsec_nanos() / 1_000_000)),
    )
}

/// Every field the model declares has an expectation above — a field the model adds fails here
/// rather than going unasserted — and a recommendation reporting nothing has an empty cell
/// under each of them.
#[test]
fn the_export_expectation_covers_every_modelled_field() {
    for field in EXPORTABLE_E_C_S_SERVICE_FIELD_VALUES {
        if *field == TIMESTAMP_COLUMN {
            continue;
        }
        let expected = expected_cell(field, &json!({}));
        assert!(
            expected.is_empty(),
            "field {field:?}: a recommendation reporting nothing has an empty cell, got \
             {expected:?}"
        );
    }
}

/// The export file holds the recommendations `GetECSServiceRecommendations` reports for the
/// same account at the same moment: one row per recommendation, in the order the reader
/// reports them, every cell the value the reader reports for the `ExportableECSServiceField`
/// naming that column — and the header is exactly the fields the request asked for, or the
/// whole modelled field set when it asked for none. One row per field selection.
#[tokio::test]
async fn the_export_file_holds_what_the_ecs_service_reader_reports() {
    let every_field: Vec<&str> = EXPORTABLE_E_C_S_SERVICE_FIELD_VALUES.to_vec();

    // (case, the `fieldsToExport` the request carries, the columns the file must then carry)
    type Selection = (&'static str, Option<Vec<&'static str>>, Vec<&'static str>);
    let selections: Vec<Selection> = vec![
        (
            "the service and its finding",
            Some(vec![
                "AccountId",
                "ServiceArn",
                "LaunchType",
                "Finding",
                "FindingReasonCodes",
                "CurrentPerformanceRisk",
            ]),
            vec![
                "AccountId",
                "ServiceArn",
                "LaunchType",
                "Finding",
                "FindingReasonCodes",
                "CurrentPerformanceRisk",
            ],
        ),
        (
            "the current task size, its containers and its tags, in the request's own order",
            Some(vec![
                "Tags",
                "CurrentServiceContainerConfigurations",
                "CurrentServiceConfigurationTaskDefinitionArn",
                "CurrentServiceConfigurationMemory",
                "CurrentServiceConfigurationCpu",
                UNRECORDED_COLUMN,
                "ServiceArn",
            ]),
            vec![
                "Tags",
                "CurrentServiceContainerConfigurations",
                "CurrentServiceConfigurationTaskDefinitionArn",
                "CurrentServiceConfigurationMemory",
                "CurrentServiceConfigurationCpu",
                UNRECORDED_COLUMN,
                "ServiceArn",
            ],
        ),
        (
            // A field asked for twice is one column, so neither the file nor the work of
            // building it grows with a list that repeats.
            "the option, its savings and its containers, one field asked for twice",
            Some(vec![
                "ServiceArn",
                "RecommendationOptionsCpu",
                "RecommendationOptionsMemory",
                "RecommendationOptionsSavingsOpportunityPercentage",
                "RecommendationOptionsEstimatedMonthlySavingsCurrency",
                "RecommendationOptionsEstimatedMonthlySavingsValue",
                "RecommendationOptionsContainerRecommendations",
                "ServiceArn",
            ]),
            vec![
                "ServiceArn",
                "RecommendationOptionsCpu",
                "RecommendationOptionsMemory",
                "RecommendationOptionsSavingsOpportunityPercentage",
                "RecommendationOptionsEstimatedMonthlySavingsCurrency",
                "RecommendationOptionsEstimatedMonthlySavingsValue",
                "RecommendationOptionsContainerRecommendations",
            ],
        ),
        (
            "the utilization columns, the projection, the preferences and the instant",
            Some(vec![
                "ServiceArn",
                "UtilizationMetricsCpuMaximum",
                "UtilizationMetricsMemoryMaximum",
                "RecommendationOptionsProjectedUtilizationMetricsCpuMaximum",
                "RecommendationOptionsProjectedUtilizationMetricsMemoryMaximum",
                "LookbackPeriodInDays",
                "EffectiveRecommendationPreferencesLookBackPeriod",
                "EffectiveRecommendationPreferencesSavingsEstimationMode",
                TIMESTAMP_COLUMN,
            ]),
            vec![
                "ServiceArn",
                "UtilizationMetricsCpuMaximum",
                "UtilizationMetricsMemoryMaximum",
                "RecommendationOptionsProjectedUtilizationMetricsCpuMaximum",
                "RecommendationOptionsProjectedUtilizationMetricsMemoryMaximum",
                "LookbackPeriodInDays",
                "EffectiveRecommendationPreferencesLookBackPeriod",
                "EffectiveRecommendationPreferencesSavingsEstimationMode",
                TIMESTAMP_COLUMN,
            ],
        ),
        (
            // A request that selects no field gets every field the model declares for the
            // resource type (`ExportableECSServiceField`).
            "no field selection at all",
            None,
            every_field,
        ),
    ];

    for (case, fields, columns) in selections {
        // A fresh emulator per row: one Amazon ECS service export job is in progress per
        // Region at a time, so each row gets its own store rather than waiting on the previous
        // row's job.
        let router = export_emulator(case).await;
        let reported = reader_recommendations(&router, case).await;

        let rows = exported_rows(&router, case, fields.as_deref()).await;
        let header = rows.first().cloned().unwrap_or_default();
        assert_eq!(
            header, columns,
            "case {case:?}: the header row is the fields the request asked for, got {rows:?}"
        );
        let data = rows.get(1..).unwrap_or_default();
        assert_eq!(
            data.len(),
            reported.len(),
            "case {case:?}: expected one row per reported recommendation ({}), got {data:?}",
            reported.len()
        );

        for (row, recommendation) in data.iter().zip(&reported) {
            let service = text(recommendation, "serviceArn");
            for column in &header {
                let held = cell(&header, row, column);
                if column == TIMESTAMP_COLUMN {
                    // "The timestamp of when the Amazon ECS service recommendation was last
                    // generated": the instant the reader reports for the same recommendation.
                    let expected = recommendation["lastRefreshTimestamp"]
                        .as_f64()
                        .unwrap_or_else(|| {
                            panic!(
                                "case {case:?}: the reader states when the recommendation was \
                                 generated, got {recommendation}"
                            )
                        });
                    assert_eq!(
                        stated_millis(held),
                        stated_millis(
                            &::aws_smithy_types::DateTime::from_secs_f64(expected)
                                .fmt(::aws_smithy_types::date_time::Format::DateTime)
                                .unwrap_or_default()
                        ),
                        "case {case:?}: column {column} of {service} must state the instant the \
                         reader reports ({expected}), got {held:?} in {row:?}"
                    );
                    continue;
                }
                let expected = expected_cell(column, recommendation);
                assert_eq!(
                    held, expected,
                    "case {case:?}: column {column} of {service} must hold what the reader \
                     reports ({expected:?}), got {row:?}"
                );
            }
        }

        // The file is of the services the READER reports, so a service no ECS store holds —
        // which the reader reports no recommendation for — is in no row of it.
        if header.iter().any(|column| column == "ServiceArn") {
            let exported: Vec<&str> = data
                .iter()
                .map(|row| cell(&header, row, "ServiceArn"))
                .collect();
            let expected: Vec<String> = reported
                .iter()
                .map(|recommendation| text(recommendation, "serviceArn"))
                .collect();
            assert_eq!(
                exported, expected,
                "case {case:?}: the exported services must be the ones the reader reports"
            );
            let unheld = unheld_service_arn();
            assert!(
                !exported.contains(&unheld.as_str()),
                "case {case:?}: {unheld} is a service no ECS store holds and the reader \
                 reports nothing for, so no row is of it, got {exported:?}"
            );
        }
    }
}

/// The file is not a sheet of empty cells: every member the Amazon ECS service reader reports
/// is filled for at least one exported row, under the column the field set names for it —
/// including the after-discount savings the account's `savingsEstimationMode` preference
/// activates — and the one column that names a member this analysis records nothing for is
/// empty in every row. The metadata file beside it states what the export was of.
#[tokio::test]
async fn the_exported_rows_carry_the_ecs_service_members_the_reader_reports() {
    let case = "the filled columns";
    let router = export_emulator(case).await;
    // "Specify the `AfterDiscounts` status to activate the preference": with it active the
    // reader reports an after-discount saving for each option, so the file's after-discount
    // columns carry one too.
    let (status, body) = call(
        &router,
        "PutRecommendationPreferences",
        json!({"resourceType": RESOURCE_TYPE_ECS_SERVICE, "savingsEstimationMode": "AfterDiscounts"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "activating the savings estimation mode preference: got {status}: {body}"
    );
    let reported = reader_recommendations(&router, case).await;

    let rows = exported_rows(&router, case, None).await;
    let header = rows.first().cloned().unwrap_or_default();
    let data = rows.get(1..).unwrap_or_default();
    assert_eq!(
        data.len(),
        reported.len(),
        "expected one row per reported recommendation ({}), got {data:?}",
        reported.len()
    );

    // Every column the field set declares, except the one no member of this analysis fills,
    // carries what the reader reports for at least one of the analysed services.
    for column in EXPORTABLE_E_C_S_SERVICE_FIELD_VALUES {
        if *column == UNRECORDED_COLUMN {
            for row in data {
                assert!(
                    cell(&header, row, column).is_empty(),
                    "column {column} names a member this analysis records nothing for, so it is \
                     empty, got {row:?}"
                );
            }
            continue;
        }
        let filled = data
            .iter()
            .filter(|row| !cell(&header, row, column).is_empty())
            .count();
        assert!(
            filled > 0,
            "column {column} must carry what the reader reports for at least one of the \
             analysed services, got {data:?}"
        );
    }

    // And the file's rows still agree with the reader cell by cell under the activated
    // preference, which is where the after-discount columns come from.
    for (row, recommendation) in data.iter().zip(&reported) {
        for column in ["RecommendationOptionsSavingsOpportunityAfterDiscountsPercentage",
                       "RecommendationOptionsEstimatedMonthlySavingsCurrencyAfterDiscounts",
                       "RecommendationOptionsEstimatedMonthlySavingsValueAfterDiscounts"] {
            let expected = expected_cell(column, recommendation);
            assert_eq!(
                cell(&header, row, column),
                expected,
                "column {column} must hold what the reader reports ({expected:?}), got {row:?}"
            );
        }
    }

    let body = export_ok(
        &router,
        "the metadata file",
        json!({"s3DestinationConfig": {"bucket": EXPORT_BUCKET}}),
    )
    .await;
    let job_id = body["jobId"].as_str().unwrap_or_default().to_string();
    let (bucket, _, metadata_key) = export_destination("the metadata file", &body);
    polled_to_complete(&router, "the metadata file", &job_id).await;
    let metadata = downloaded(&router, "the metadata file", &bucket, &metadata_key).await;
    let metadata: Value = serde_json::from_slice(metadata.as_bytes())
        .unwrap_or_else(|error| panic!("the metadata file is JSON: {error}"));
    assert_eq!(
        metadata["resourceType"].as_str(),
        Some(RESOURCE_TYPE_ECS_SERVICE),
        "the metadata names the exported resource type, got {metadata}"
    );
    assert_eq!(
        metadata["exportFile"]["rowCount"].as_u64(),
        Some(reported.len() as u64),
        "the metadata counts the rows the file holds ({}), got {metadata}",
        reported.len()
    );
}

/// The filters an export carries select exactly what the same filters select for the READER —
/// "an array of objects to specify a filter that exports a more specific set of ...
/// recommendations" — so a client can reconcile the file with the reader's answer. One row per
/// filter of the surface's vocabulary, spanning filters that select recommendations and
/// filters that select none.
#[tokio::test]
async fn the_export_filters_select_what_the_ecs_service_reader_selects() {
    // (case, the filters the reader and the export both carry)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a finding the analysis reached",
            json!([{"name": "Finding", "values": ["Overprovisioned"]}]),
        ),
        (
            "the other two findings at once",
            json!([{"name": "Finding", "values": ["Optimized", "Underprovisioned"]}]),
        ),
        (
            "a finding reason code the analysis reached",
            json!([{"name": "FindingReasonCode", "values": ["MemoryUnderprovisioned"]}]),
        ),
        (
            "a finding reason code of the other side",
            json!([{"name": "FindingReasonCode", "values": ["CPUOverprovisioned"]}]),
        ),
        (
            // Both names at once, selecting a combination no analysed service carries: the
            // under-provisioned service's CPU cannot also be sized down.
            "a finding and a reason code no service carries together",
            json!([{"name": "Finding", "values": ["Underprovisioned"]},
                   {"name": "FindingReasonCode", "values": ["CPUOverprovisioned"]}]),
        ),
        (
            "a filter carrying no value, which constrains nothing",
            json!([{"name": "Finding", "values": []}]),
        ),
    ];

    let mut saw_selected = false;
    let mut saw_nothing = false;
    for (case, filters) in rows {
        let router = export_emulator(case).await;
        let (status, read) = call(
            &router,
            "GetECSServiceRecommendations",
            json!({"filters": filters.clone()}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading with {filters}: got {status}: {read}"
        );
        let selected: Vec<String> = list(&read, "ecsServiceRecommendations", case)
            .iter()
            .map(|recommendation| text(recommendation, "serviceArn"))
            .collect();

        let body = export_ok(
            &router,
            case,
            json!({
                "s3DestinationConfig": {"bucket": EXPORT_BUCKET},
                "fieldsToExport": ["ServiceArn", "Finding", "FindingReasonCodes"],
                "filters": filters.clone(),
            }),
        )
        .await;
        let job_id = body["jobId"].as_str().unwrap_or_default().to_string();
        let (bucket, key, _) = export_destination(case, &body);
        polled_to_complete(&router, case, &job_id).await;
        let rows = csv_rows(&downloaded(&router, case, &bucket, &key).await);
        let header = rows.first().cloned().unwrap_or_default();
        let exported: Vec<String> = rows
            .get(1..)
            .unwrap_or_default()
            .iter()
            .map(|row| cell(&header, row, "ServiceArn").to_string())
            .collect();
        assert_eq!(
            exported, selected,
            "case {case:?}: the filter {filters} selected {selected:?} for the reader, so the \
             export file holds those rows"
        );
        saw_selected |= !selected.is_empty();
        saw_nothing |= selected.is_empty();
    }
    assert!(
        saw_selected && saw_nothing,
        "the table must span a filter that selects recommendations and one that selects none, \
         or it proves nothing (selected some: {saw_selected}, selected none: {saw_nothing})"
    );
}
