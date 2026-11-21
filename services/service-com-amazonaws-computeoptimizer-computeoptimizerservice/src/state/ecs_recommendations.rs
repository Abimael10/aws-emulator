// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECS service RECOMMENDATION this service reports, computed in ONE place.
//!
//! An Amazon ECS service recommendation is not read from the findings catalog: it is computed
//! from the Amazon ECS service inventory (the service's launch type, task definition and task
//! count) and the `AWS/ECS` utilization the account published for it, under the preferences
//! this service holds for the `EcsService` resource type. Every surface that serves those
//! recommendations — `GetECSServiceRecommendations`, the projected-metrics reader and
//! `ExportECSServiceRecommendations` — reports the recommendations THIS module computes, so a
//! reader's response and an exported row of the same moment cannot disagree about which
//! services have a recommendation or about any member of one.
//!
//! Part of [`super`]'s one state module, in its own file so one concern lives in one place;
//! the ARN form, the inventory read and the Fargate sizing it builds on are
//! [`super::ecs_analysis`]'s.

use super::*;
use crate::ops::types::{
    ContainerRecommendation, ECSEffectiveRecommendationPreferences, ECSEstimatedMonthlySavings,
    ECSSavingsEstimationMode, ECSSavingsOpportunityAfterDiscounts, ECSServiceRecommendation,
    ECSServiceRecommendationOption, ECSServiceUtilizationMetric, ServiceConfiguration,
};

/// The public on-demand price of ONE vCPU-hour and ONE GB-hour of AWS Fargate compute, in
/// [`CURRENCY_USD`] — the two figures AWS Fargate's own pricing is published as ("you pay for
/// the amount of vCPU and memory resources that your containerized application requests",
/// charged per vCPU per hour and per GB per hour).
///
/// Every money figure an ECS service recommendation reports is computed from these two prices,
/// exactly as the EBS and Lambda analyses of this service compute theirs from their own price
/// tables, so the recommendation's figures agree with each other and with the
/// [`SAVINGS_ESTIMATION_SOURCE`] it names.
pub const FARGATE_PRICE_PER_VCPU_HOUR: f64 = 0.040_48;
/// See [`FARGATE_PRICE_PER_VCPU_HOUR`].
pub const FARGATE_PRICE_PER_GB_HOUR: f64 = 0.004_445;

/// CPU units in one vCPU, the unit the Fargate vCPU-hour price is charged in: "1024 CPU units
/// is the equivalent of 1 vCPU" (the ECS model's `RegisterTaskDefinitionRequest$cpu`).
const CPU_UNITS_PER_VCPU: f64 = 1024.0;

/// One Amazon ECS service in scope of an analysis, with the state the analysis reads for it.
#[derive(Debug, Clone)]
pub struct EcsServiceCandidate {
    /// Which service this is ([`parse_ecs_service_arn`]).
    pub reference: EcsServiceRef,
    /// What the ECS store holds for it ([`ecs_service_config`]), or `None` when it holds no
    /// ACTIVE record.
    pub config: Option<EcsServiceConfig>,
    /// The `LookBackPeriodPreference` in force for it, resolved from this service's own
    /// preference store ([`ecs_candidate_preferences`]).
    pub look_back_period: String,
    /// Whether the `SavingsEstimationMode` preference in force for it activates reporting
    /// savings net of Savings Plans discounts, from the same resolution.
    pub after_discounts: bool,
}

impl EcsServiceCandidate {
    /// One candidate of `reference` with `config`, before the preferences in force for it are
    /// resolved ([`ecs_candidate_preferences`]).
    fn of(reference: EcsServiceRef, config: Option<EcsServiceConfig>) -> Self {
        Self {
            reference,
            config,
            look_back_period: String::new(),
            after_discounts: false,
        }
    }
}

/// Why one candidate in scope produced no recommendation.
#[derive(Debug, Clone)]
pub enum EcsNoAnalysis {
    /// Compute Optimizer does not support the resource, so the analysis REFUSED it: the
    /// sentence that says why, which the reader reports as the `message` of the request's
    /// `errors` entry for the service the request named.
    Unsupported(String),
    /// The analysis has not reached the resource — this service holds no record of it, or no
    /// observation of it falls in the look-back period — so there is no recommendation yet,
    /// which is the steady state of a newly enrolled account: the resource is absent from
    /// every answer and no request has failed.
    NotAnalysed,
}

/// Every ACTIVE Amazon ECS service of `region` the inventory holds, in ARN order so a page
/// boundary and a row order are stable across calls.
///
/// Compute Optimizer is regional and an ECS service belongs to the region its own ARN names,
/// so a service created at one endpoint is never analysed at another.
pub fn ecs_service_candidates(
    inventory: &ecs::state::ComAmazonawsEcsAmazonec2containerservicev20141113State,
    region: &str,
) -> Vec<EcsServiceCandidate> {
    let mut candidates: Vec<EcsServiceCandidate> = inventory
        .services
        .values()
        .filter(|record| record.is_active())
        .filter_map(|record| {
            let reference = parse_ecs_service_arn(&record.arn)?;
            if reference.region != region {
                return None;
            }
            let config = ecs_service_config(inventory, &reference);
            Some(EcsServiceCandidate::of(reference, config))
        })
        .collect();
    candidates.sort_by(|left, right| left.reference.arn.cmp(&right.reference.arn));
    candidates.dedup_by(|left, right| left.reference.arn == right.reference.arn);
    candidates
}

/// One candidate for a service a request NAMED, as the endpoint of `region` sees it.
///
/// An ARN that names another region names no service of this regional endpoint, so it is given
/// no configuration — which is what reports it as unanalysed exactly as an ARN naming a service
/// that does not exist is reported.
pub fn ecs_named_candidate(
    inventory: &ecs::state::ComAmazonawsEcsAmazonec2containerservicev20141113State,
    reference: EcsServiceRef,
    region: &str,
) -> EcsServiceCandidate {
    let config = (reference.region == region)
        .then(|| ecs_service_config(inventory, &reference))
        .flatten();
    EcsServiceCandidate::of(reference, config)
}

/// Fill in the preferences in force for each candidate: the value from the most specific scope
/// that sets one for the `EcsService` resource type, and the service's documented defaults for
/// every preference no record sets.
///
/// The resolution is the preference store's OWN
/// ([`ComAmazonawsComputeoptimizerComputeoptimizerserviceState::effective_preferences_for`]) —
/// "the ONE resolution in this service", which `GetEffectiveRecommendationPreferences` and
/// every other recommendation reader state their preferences from — so no surface that serves
/// these recommendations can drift from another on the precedence or on the defaults.
pub fn ecs_candidate_preferences(
    store: &ComAmazonawsComputeoptimizerComputeoptimizerserviceState,
    candidates: &mut [EcsServiceCandidate],
) {
    for candidate in candidates {
        let effective = store.effective_preferences_for(
            &candidate.reference.arn,
            RESOURCE_TYPE_ECS_SERVICE,
            &candidate.reference.account_id,
        );
        candidate.after_discounts = effective.after_discounts();
        candidate.look_back_period = effective.look_back_period;
    }
}

/// The Amazon ECS service recommendations one account holds at the endpoint of `region`, in the
/// order every surface reports them: one per ACTIVE service of that region the analysis has
/// inputs for, computed by [`ecs_service_recommendation`].
///
/// The ONE sweep every surface that serves a SET of these recommendations reads — the reader
/// that reports them and the export that writes them to a file — so the file an export job
/// writes holds exactly the recommendations a read of the same moment reports.
///
/// The gate is the one the reader applies: an account that is not opted in holds no analysis at
/// all. The analysis itself is computed from another service's inventory and the utilization
/// published for it rather than recorded with this account's enrollment, so it is not the stored
/// analysis [`Enrollment::analysis_visible`] governs.
///
/// Reads only: the ECS inventory and the CloudWatch series are another service's state, and this
/// writes to neither, nor to this service's own store.
pub fn analysed_ecs_services<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &ComAmazonawsComputeoptimizerComputeoptimizerserviceState,
    region: &str,
    account_id: &str,
    now: f64,
) -> Vec<ECSServiceRecommendation> {
    if !store.enrollment.is_opted_in() {
        return Vec::new();
    }
    // The inventory holds the resources of the ONE account this emulator presents
    // ([`engine::emulator_account_id`]), so an account the request named that is not it — a
    // member account of the organization this service records — runs no service here and has
    // no inventory swept on its behalf.
    if account_id != ::engine::emulator_account_id() {
        return Vec::new();
    }
    // An emulator that does not hold the Amazon ECS inventory, or the metric store the analysis
    // reads, has no service to analyse — the same answer as an account that runs none — rather
    // than a finding computed from an input nothing provided.
    let (Some(inventory), Some(observations)) = (
        ecs::state::service_state(engine),
        cloudwatch::state::service_state(engine),
    ) else {
        return Vec::new();
    };
    let mut candidates = {
        let inventory = inventory.lock();
        ecs_service_candidates(&inventory, region)
    };
    // "The account that owns a service is the account field of its ARN": a service owned by any
    // other account is one no request to this service can be answered about, so it is left out
    // of the sweep entirely.
    candidates.retain(|candidate| candidate.reference.account_id == account_id);
    ecs_candidate_preferences(store, &mut candidates);
    let observations = observations.lock();
    candidates
        .iter()
        .filter_map(|candidate| ecs_service_recommendation(candidate, &observations, now).ok())
        .collect()
}

/// The recommendation for one Amazon ECS service, or why the analysis produced none for it.
///
/// Compute Optimizer invents nothing about a workload: a finding is "identified by analyzing the
/// `CPUUtilization` metric of the current service during the look-back period"
/// (`ECSServiceRecommendation$findingReasonCodes`), and the current configuration is the
/// service's own — so every member reported here is computed from the candidate's persisted ECS
/// configuration and the utilization published for it.
pub fn ecs_service_recommendation(
    candidate: &EcsServiceCandidate,
    observations: &cloudwatch::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State,
    now: f64,
) -> Result<ECSServiceRecommendation, EcsNoAnalysis> {
    let arn = candidate.reference.arn.as_str();
    let Some(config) = &candidate.config else {
        return Err(EcsNoAnalysis::NotAnalysed);
    };
    // "Compute Optimizer generates recommendations for Amazon ECS services on Fargate."
    if ecs_launch_type_of(config) != Some(ECS_SERVICE_LAUNCH_TYPE_FARGATE) {
        return Err(EcsNoAnalysis::Unsupported(format!(
            "Compute Optimizer generates Amazon ECS service recommendations for services on \
             {ECS_SERVICE_LAUNCH_TYPE_FARGATE} only, and {arn} does not run on \
             {ECS_SERVICE_LAUNCH_TYPE_FARGATE}."
        )));
    }
    // A Fargate task definition carries task-level CPU and memory ("This field is required for
    // Fargate"); without them there is no current size to compare utilization against.
    let (Some(current_cpu), Some(current_memory)) = (config.cpu_units, config.memory_mib) else {
        return Err(EcsNoAnalysis::Unsupported(format!(
            "The task definition of {arn} sets no task-level cpu and memory, so its Amazon ECS \
             service utilization cannot be sized."
        )));
    };
    if current_cpu <= 0 || current_memory <= 0 {
        return Err(EcsNoAnalysis::Unsupported(format!(
            "The task definition of {arn} sets a task-level cpu of {current_cpu} and memory of \
             {current_memory}, which is not a Fargate task size."
        )));
    }

    // The window the analysis reads: the effective look-back period, back from now
    // ([`ecs_observed_utilization`]). The day count is the crate's own
    // ([`look_back_period_in_days`]), the one every recommendation reader reports
    // `lookbackPeriodInDays` from.
    let look_back_days = look_back_period_in_days(&candidate.look_back_period);
    let observed = ecs_observed_utilization(
        observations,
        &candidate.reference,
        &candidate.look_back_period,
        now,
    );
    let Some((cpu, memory)) = observed.both() else {
        return Err(EcsNoAnalysis::NotAnalysed);
    };

    // The size each dimension needs: the observed PEAK carried with the crate's one
    // recommendation headroom ([`RECOMMENDATION_HEADROOM`], the service's documented
    // `PERCENT_20`). The recommendation option is then the smallest Fargate task size that
    // holds both ([`ecs_recommended_task_size`]).
    let required_cpu = ecs_required_capacity(current_cpu, cpu.maximum);
    let required_memory = ecs_required_capacity(current_memory, memory.maximum);
    let (option_cpu, option_memory) =
        ecs_recommended_task_size(current_cpu, current_memory, cpu.maximum, memory.maximum);

    // "The service CPU configuration can be sized up to enhance the performance of your
    // workload" / "can be sized down while still meeting the performance requirements of your
    // workload", per dimension. A reason code describes the DIMENSION, not the joint option, so
    // each one is judged against the smallest size the catalog offers for THAT dimension alone:
    // a dimension whose own requirement fits a smaller size can be sized down even when the
    // other dimension forces the recommended task up, and a dimension already at the catalog's
    // smallest size cannot be sized down however little it uses.
    let cpu_under = required_cpu > current_cpu;
    let memory_under = required_memory > current_memory;
    let cpu_over = ecs_smallest_fargate_cpu(required_cpu) < current_cpu;
    let memory_over = ecs_smallest_fargate_memory(required_memory) < current_memory;
    let mut finding_reason_codes = Vec::new();
    if cpu_under {
        finding_reason_codes.push(ECS_REASON_CPU_UNDERPROVISIONED.to_string());
    }
    if cpu_over {
        finding_reason_codes.push(ECS_REASON_CPU_OVERPROVISIONED.to_string());
    }
    if memory_under {
        finding_reason_codes.push(ECS_REASON_MEMORY_UNDERPROVISIONED.to_string());
    }
    if memory_over {
        finding_reason_codes.push(ECS_REASON_MEMORY_OVERPROVISIONED.to_string());
    }
    // A dimension that is too small is a performance problem and a dimension that is too large
    // is a cost one, so under-provisioning decides the finding when both hold: "When Compute
    // Optimizer detects that there's not enough memory or CPU, an Amazon ECS service is
    // considered under-provisioned."
    let finding = if cpu_under || memory_under {
        FINDING_UNDERPROVISIONED
    } else if cpu_over || memory_over {
        FINDING_OVERPROVISIONED
    } else {
        FINDING_OPTIMIZED
    };
    // "The risk of the current Amazon ECS service not meeting the performance needs of its
    // workloads. The higher the risk, the more likely the current service can't meet the
    // performance requirements of its workload." AWS publishes no numeric rule for the four
    // `CurrentPerformanceRisk` values, so this emulator reports the risk it can actually derive:
    // how many of the two analysed dimensions are already past their target utilization —
    // neither (`VeryLow`), one (`Medium`) or both (`High`). `Low` is DELIBERATELY unreachable
    // here: it would have to grade how far past the target one dimension is, and an ECS service
    // recommendation is analysed over exactly two dimensions with no published gradation between
    // them, so reporting it would claim a finer judgement than the analysis makes.
    let current_performance_risk = match u8::from(cpu_under) + u8::from(memory_under) {
        0 => PERFORMANCE_RISK_VERY_LOW,
        1 => PERFORMANCE_RISK_MEDIUM,
        _ => PERFORMANCE_RISK_HIGH,
    };

    // What moving the service to the recommended task size saves per month, on the public
    // Fargate prices: the difference between what the two sizes cost for the tasks this service
    // keeps running, and that saving "as a percentage of monthly cost"
    // (`SavingsOpportunity$savingsOpportunityPercentage`). A recommendation that costs the same
    // or MORE saves nothing rather than a negative amount — an under-provisioned service is
    // recommended a bigger task — which is the arithmetic every other analysis of this service
    // reports a saving with.
    let current_cost = fargate_monthly_cost(current_cpu, current_memory, config.desired_tasks);
    let option_cost = fargate_monthly_cost(option_cpu, option_memory, config.desired_tasks);
    let saved = round_hundredths((current_cost - option_cost).max(0.0));
    let saved_percentage = percentage_of(saved, current_cost);

    Ok(ECSServiceRecommendation {
        service_arn: Some(arn.to_string()),
        account_id: Some(candidate.reference.account_id.clone()),
        current_service_configuration: Some(ServiceConfiguration {
            memory: Some(current_memory),
            cpu: Some(current_cpu),
            container_configurations: Some(config.containers.clone()),
            // Whether a target-tracking policy scales the service lives in Application Auto
            // Scaling, which this analysis does not read, so the member is omitted rather than
            // guessed.
            auto_scaling_configuration: None,
            task_definition_arn: config.task_definition_arn.clone(),
        }),
        utilization_metrics: Some(vec![
            ecs_utilization_metric(METRIC_CPU, METRIC_STATISTIC_MAXIMUM, cpu.maximum),
            ecs_utilization_metric(METRIC_CPU, METRIC_STATISTIC_AVERAGE, cpu.average),
            ecs_utilization_metric(METRIC_MEMORY, METRIC_STATISTIC_MAXIMUM, memory.maximum),
            ecs_utilization_metric(METRIC_MEMORY, METRIC_STATISTIC_AVERAGE, memory.average),
        ]),
        // "The number of days the Amazon ECS service utilization metrics were analyzed": the
        // window the look-back preference sets, which is the window read above.
        lookback_period_in_days: look_back_days,
        launch_type: Some(ECS_SERVICE_LAUNCH_TYPE_FARGATE.to_string()),
        // The analysis is current to the newest observation it read; it changes when a newer one
        // is published, not when somebody reads it.
        last_refresh_timestamp: Some(cloudwatch::state::to_timestamp(
            cpu.newest_secs.max(memory.newest_secs),
        )),
        finding: Some(finding.to_string()),
        finding_reason_codes: Some(finding_reason_codes),
        service_recommendation_options: Some(vec![ECSServiceRecommendationOption {
            memory: Some(option_memory),
            cpu: Some(option_cpu),
            // The money the recommendation is for, computed from the two task sizes above and
            // the public Fargate price table.
            savings_opportunity: Some(SavingsOpportunity {
                savings_opportunity_percentage: saved_percentage,
                estimated_monthly_savings: Some(EstimatedMonthlySavings {
                    currency: Some(CURRENCY_USD.to_string()),
                    value: saved,
                }),
            }),
            // The saving net of Savings Plans discounts is reported only once the account
            // activates the savings-estimation-mode preference — "specify the `AfterDiscounts`
            // status to activate the preference". This emulator records no Savings Plans
            // commitment against a service's Fargate spend, so the after-discount figures are
            // the on-demand ones, which is exactly what the reported
            // [`SAVINGS_ESTIMATION_SOURCE`] says they came from.
            savings_opportunity_after_discounts: candidate.after_discounts.then(|| {
                ECSSavingsOpportunityAfterDiscounts {
                    savings_opportunity_percentage: saved_percentage,
                    estimated_monthly_savings: Some(ECSEstimatedMonthlySavings {
                        currency: Some(CURRENCY_USD.to_string()),
                        value: saved,
                    }),
                }
            }),
            // The utilization each observed statistic becomes at the recommended task size
            // ([`ecs_projected_utilization_metrics`]), the one projection
            // `GetECSServiceRecommendationProjectedMetrics` reports a series of for this same
            // option.
            projected_utilization_metrics: Some(ecs_projected_utilization_metrics(
                cpu,
                memory,
                (current_cpu, current_memory),
                (option_cpu, option_memory),
            )),
            // "The CPU and memory size recommendations for the containers within the task":
            // each container's OWN reservation, carried to the recommended task size in the
            // proportion of the task it reserves today. A task definition that reserves nothing
            // per container has no container recommendation to report.
            container_recommendations: (!config.containers.is_empty()).then(|| {
                config
                    .containers
                    .iter()
                    .map(|container| {
                        ecs_container_recommendation(
                            container,
                            (current_cpu, option_cpu),
                            (current_memory, option_memory),
                        )
                    })
                    .collect()
            }),
        }]),
        current_performance_risk: Some(current_performance_risk.to_string()),
        effective_recommendation_preferences: Some(ECSEffectiveRecommendationPreferences {
            // The savings estimation mode names the PRICING source the estimate above came from
            // (`ECSSavingsEstimationModeSource`): the public Fargate prices this analysis holds,
            // never an account's own Cost Explorer or Cost Optimization Hub data — the one
            // source constant this service reports every money figure from.
            savings_estimation_mode: Some(ECSSavingsEstimationMode {
                source: Some(SAVINGS_ESTIMATION_SOURCE.to_string()),
            }),
            look_back_period: Some(candidate.look_back_period.clone()),
        }),
        tags: Some(config.tags.clone()),
    })
}

/// The `ECSServiceRecommendationFindingReasonCode` values
/// (`crate::ops::types::E_C_S_SERVICE_RECOMMENDATION_FINDING_REASON_CODE_VALUES`): one per
/// dimension that can be "sized up to enhance the performance of your workload" or "sized down
/// while still meeting the performance requirements of your workload".
pub const ECS_REASON_CPU_UNDERPROVISIONED: &str = "CPUUnderprovisioned";
/// See [`ECS_REASON_CPU_UNDERPROVISIONED`].
pub const ECS_REASON_CPU_OVERPROVISIONED: &str = "CPUOverprovisioned";
/// See [`ECS_REASON_CPU_UNDERPROVISIONED`].
pub const ECS_REASON_MEMORY_UNDERPROVISIONED: &str = "MemoryUnderprovisioned";
/// See [`ECS_REASON_CPU_UNDERPROVISIONED`].
pub const ECS_REASON_MEMORY_OVERPROVISIONED: &str = "MemoryOverprovisioned";

/// What a month of one Amazon ECS service on Fargate costs: the vCPU-hours and GB-hours `tasks`
/// tasks of a `cpu_units`/`memory_mib` task size bill over a month, at
/// [`FARGATE_PRICE_PER_VCPU_HOUR`] and [`FARGATE_PRICE_PER_GB_HOUR`].
///
/// The month is the crate's one [`HOURS_PER_MONTH`] and the GB the crate's one [`MB_PER_GB`] (a
/// task-level `memory` of `1024` MiB is "`1GB`", per the ECS model's
/// `RegisterTaskDefinitionRequest$memory`), so this cost and every other monthly figure this
/// service reports are months of the same length in the same units. A service that keeps no task
/// running costs nothing, which is what its stored task count says.
fn fargate_monthly_cost(cpu_units: i32, memory_mib: i32, tasks: i32) -> f64 {
    let tasks = f64::from(tasks.max(0));
    let vcpus = f64::from(cpu_units.max(0)) / CPU_UNITS_PER_VCPU;
    let gigabytes = f64::from(memory_mib.max(0)) / MB_PER_GB;
    round_hundredths(
        tasks * (vcpus * FARGATE_PRICE_PER_VCPU_HOUR + gigabytes * FARGATE_PRICE_PER_GB_HOUR)
            * HOURS_PER_MONTH,
    )
}

/// One `ContainerRecommendation`: the container's reservations at the recommended task size.
/// `cpu` is carried on the CPU sizes and both memory members on the memory sizes, each as the
/// pair `(current, recommended)`, so a container keeps the share of the task it reserves today;
/// a member the task definition sets no reservation for is recommended none, because there is no
/// reservation to resize.
fn ecs_container_recommendation(
    container: &ContainerConfiguration,
    cpu: (i32, i32),
    memory: (i32, i32),
) -> ContainerRecommendation {
    ContainerRecommendation {
        container_name: container.container_name.clone(),
        cpu: container
            .cpu
            .map(|reserved| scaled_reservation(reserved, cpu.0, cpu.1)),
        memory_size_configuration: container.memory_size_configuration.as_ref().map(
            |configuration| MemorySizeConfiguration {
                memory: configuration
                    .memory
                    .map(|reserved| scaled_reservation(reserved, memory.0, memory.1)),
                memory_reservation: configuration
                    .memory_reservation
                    .map(|reserved| scaled_reservation(reserved, memory.0, memory.1)),
            },
        ),
    }
}

/// A container reservation of `reserved` out of a task of `current` capacity, at a task of
/// `recommended` capacity: the same share of the task, in whole units.
///
/// A positive reservation stays positive however far the task shrinks — a container that
/// reserves capacity today cannot be recommended a reservation of nothing, which would be a
/// container definition that reserves nothing rather than a smaller one.
fn scaled_reservation(reserved: i32, current: i32, recommended: i32) -> i32 {
    if reserved <= 0 || current <= 0 {
        return reserved;
    }
    ecs_rounded_units(f64::from(reserved) * f64::from(recommended) / f64::from(current)).max(1)
}

/// One `ECSServiceUtilizationMetric`: what a named statistic of a named series was over the
/// look-back period.
fn ecs_utilization_metric(name: &str, statistic: &str, value: f64) -> ECSServiceUtilizationMetric {
    ECSServiceUtilizationMetric {
        name: Some(name.to_string()),
        statistic: Some(statistic.to_string()),
        value,
    }
}
