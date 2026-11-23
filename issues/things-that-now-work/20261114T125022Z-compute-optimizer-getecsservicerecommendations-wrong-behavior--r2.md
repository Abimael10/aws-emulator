Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 9a1cab38181c81438a46747e76c041451acccd46

# The two ECS service readers skip the analysis-due visibility gate every other recommendation reader of the crate applies

_Filed: 2026-11-14T12:50:22Z_
_Operation: GetECSServiceRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `9a1cab38181c81438a46747e76c041451acccd46`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other recommendation reader of this crate reports nothing until the analysis the account's enrollment started is DUE (`Enrollment::analysis_visible()`, i.e. `ANALYSIS_DELAY` after the enrollment record was written) — including the other surface that is computed live from another service's inventory, the Amazon Aurora/RDS surface, whose `analysed_databases` applies the gate before reading the RDS inventory at all. The two Amazon ECS readers check only `enrollment.is_opted_in()` and then sweep the ECS inventory, so an account that has just opted in reads full ECS service recommendations (and their projected metrics) while every other surface of the same account still reports "nothing analysed yet". The crate states the rule as universal in two places, so one of the two statements is false as written.

## Evidence

The ECS reader's only gate — `src/ops/get_ecs_service_recommendations.rs:355-368`:
```
        // The enrollment gate, ahead of every read of another service's store: an account
        // that is not opted in has no analysis at all.
        if !state.lock().enrollment.is_opted_in() {
```
(and the candidate sweep at `:382-415` reads `inventory.services` with no visibility filter). The same in `src/ops/get_ecs_service_recommendation_projected_metrics.rs:294-307` (`if !preferences.enrollment.is_opted_in()`).

The sibling live-analysis surface DOES gate — `src/state/rds_inventory.rs:151-156`:
```
    // The gate every findings reader applies, ahead of any inventory read: an account that
    // never opted in, one that opted out and one that has just opted in hold no analysis at
    // all, whatever their inventory holds.
    if !store.enrollment.analysis_visible() {
        return Vec::new();
    }
```
with its doc at `src/state/rds_inventory.rs:132-134`: "the account is enrolled and the analysis its enrollment started is due ([`Enrollment::analysis_visible`], the gate every reader of this service applies)".

The crate's universal claim — `src/ops/get_rds_database_recommendation_projected_metrics.rs:285-287`: "and only once the analysis its enrollment started is due, the gate every recommendation reader of this service applies"; and `tests/all/harness.rs:544-546`: "`ANALYSIS_DELAY` after `Enrollment::last_updated_timestamp`, which is what every reader gates its records on (`Enrollment::analysis_visible`)".

Test coverage: `tests/all/analysis_due_gate.rs:73-120` drives the gate over EC2, ASG, EBS, Lambda, License, Idle, RDS, the summary reader and both EC2/RDS projected-metrics readers — the two ECS readers are the only recommendation readers absent from that table.

## Steps to Reproduce

1. Boot the emulator with an ACTIVE Fargate ECS service and CloudWatch utilization published for it.
2. Call UpdateEnrollmentStatus {status: Active, includeMemberAccounts: false}.
3. Immediately (within ANALYSIS_DELAY) call GetEC2InstanceRecommendations / GetRDSDatabaseRecommendations: both answer empty lists because the analysis is not due.
4. In the same window call GetECSServiceRecommendations (and GetECSServiceRecommendationProjectedMetrics for that service): a full recommendation is returned, so one surface reports an analysis the account is not yet supposed to have.
