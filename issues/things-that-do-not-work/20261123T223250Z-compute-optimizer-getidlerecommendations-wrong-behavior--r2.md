Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 135485142286501fe8192681e8d56b16575ee550

# Idle (and EBS volume / Lambda) recommendations report savingsOpportunityAfterDiscounts without the savings-estimation-mode preference that activates it

_Filed: 2026-11-23T22:32:50Z_
_Operation: GetIdleRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `135485142286501fe8192681e8d56b16575ee550`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate implements ONE rule for the after-discount savings member: it is reported only once the account activates the `savingsEstimationMode` preference. `EffectivePreferences::after_discounts()` is the crate's single test for that (`src/state.rs:1819-1829`), and four surfaces gate the member on it — the EC2 instance reader, the Auto Scaling group reader, the RDS database projection in `state.rs`, and the ECS service recommendation.

Three surfaces skip that rule and report the member unconditionally:

* `GetIdleRecommendations` — `IdleResourceAnalysis::to_recommendation` always builds `savings_opportunity_after_discounts: Some(..)` (`src/state/idle.rs:180`). The idle handler never resolves the account's preferences at all: it only names the default look-back period (`src/ops/get_idle_recommendations.rs:318`), so nothing a client writes for `savingsEstimationMode` on the `Idle` resource type can change a byte of the answer.
* `GetEBSVolumeRecommendations` / `ExportEBSVolumeRecommendations` — `src/state/volumes.rs:238`.
* `GetLambdaFunctionRecommendations` / `ExportLambdaFunctionRecommendations` — `src/state/functions.rs:588`.

`savingsEstimationMode` IS settable (and stored) for every one of those resource types: `PutRecommendationPreferences`'s restriction table deliberately omits it (`src/ops/put_recommendation_preferences.rs:343-346`: "A preference that applies to every resource type is absent from the table — `savingsEstimationMode`"), which the crate's own tests pin (`tests/all/reported_defaults_and_preference_validation.rs:395`: "`savingsEstimationMode` is restricted to no resource type at all").

So a client that writes `savingsEstimationMode=BeforeDiscounts` (the model's own word for deactivating the preference) — or that never writes the preference at all — still reads `savingsOpportunityAfterDiscounts` on every idle, volume and function recommendation option, while the same account's instance, group, database and ECS recommendations omit it. The preference round-trip is unobservable on exactly the three surfaces that skip the shared gate, which is the same defect the FIXED RDS issue (20261024T061128Z-compute-optimizer-getrdsdatabaserecommendations-wrong-behavior) was closed by adding the gate for.

The comment above the idle site also misstates what the siblings do, so the divergence is invisible to a reader of that code: it claims the stance is "the same stance the volume, function and group readers of this service take", but the GROUP reader both gates the member on the preference AND computes a different figure for it from `group.savings_plans_discount_rate`.

## Evidence

The crate's rule, stated where the EC2 instance reader applies it — `src/ops/get_ec2_instance_recommendations.rs:529-533`:
```
    // Whether savings are reported net of Savings Plans and Reserved Instances discounts: the
    // account activates that by storing the savings-estimation-mode preference — "specify the
    // `AfterDiscounts` status to activate the preference" — and until it does, the member that
    // preference activates is reported on no option.
    let after_discounts = preferences.after_discounts();
```
and applied at `src/ops/get_ec2_instance_recommendations.rs:561`: `savings_opportunity_after_discounts: after_discounts.then(|| { .. })` — note the premise quoted right above it (`:556-560`) is the SAME premise the three outliers give: "records no Reserved Instances or Savings Plans commitment against the account's spend, so the after-discount figures are those before discounts", and the EC2 reader still omits the member until the preference is stored.

The model sentence that rule reads — `smithy_query doc shape=PutRecommendationPreferencesRequest member=savingsEstimationMode`:
> "Specify the `AfterDiscounts` status to activate the preference, or specify `BeforeDiscounts` to deactivate the preference."

The three sites that skip it:
* `src/state/idle.rs:176-183`:
```
            // This emulator prices every resource from public pricing and records no
            // negotiated discount ([`SAVINGS_ESTIMATION_SOURCE`]), so the after-discount
            // figures are the before-discount ones — the same stance the volume, function and
            // group readers of this service take.
            savings_opportunity_after_discounts: Some(IdleSavingsOpportunityAfterDiscounts {
```
* `src/state/volumes.rs:238`: `savings_opportunity_after_discounts: savings.map(|(percentage, value)| { EBSSavingsOpportunityAfterDiscounts { .. } })`
* `src/state/functions.rs:588`: `savings_opportunity_after_discounts: Some(LambdaSavingsOpportunityAfterDiscounts { .. })`

The "group reader" the idle comment appeals to does the opposite — `src/ops/get_auto_scaling_group_recommendations.rs:511-521`:
```
            let after_discounts = preferences.after_discounts().then(|| {
                let (saving, percentage) =
                    group.savings(option, group.savings_plans_discount_rate);
```
The other two gated surfaces: `src/state.rs:2863` (`let after_discounts = effective.after_discounts();`) with `:2896` / `:2914` (`savings_opportunity_after_discounts: after_discounts.then(|| ..)`), and `src/state/ecs_recommendations.rs:147` (`candidate.after_discounts = effective.after_discounts();`) with `:491` (`savings_opportunity_after_discounts: candidate.after_discounts.then(|| ..)`).

The idle handler never resolves preferences — `src/ops/get_idle_recommendations.rs:318`: `let look_back_period = crate::state::DEFAULT_LOOK_BACK_PERIOD;` (no `effective_preferences_for` call anywhere in the file).

Test coverage, which pins BOTH sides of the one rule:
* the rule — `tests/all/ec2_instance_recommendations.rs:549-553`: `assert!(option["savingsOpportunityAfterDiscounts"].is_null(), "case {case:?}: the after-discount member is reported only once the account activates the preference, got {option}")`; `tests/all/ecs_service_recommendations.rs:466-472` ("No savings-estimation-mode preference is in force here, so no after-discount saving is reported (the preference is what activates it)."); `tests/all/auto_scaling_group_recommendations.rs:330`; `tests/all/rds_database_recommendation_options.rs:346` + `:521`.
* the current outlier behaviour — `tests/all/idle_recommendations.rs:339-353` (reason given is only that the two VALUES are equal: "before and after discounts alike, because this service prices from public pricing and records no negotiated discount" — the same premise the EC2 reader states while still gating presence), `tests/all/ebs_volume_recommendations.rs:232-237`, `tests/all/lambda_function_recommendations.rs:482`.
* `tests/all/unrestricted_preference_resource_types.rs:37` + `:99-137` show `savingsEstimationMode` is accepted and stored for EVERY resource type, including `Idle`, `EbsVolume` and `LambdaFunction`.

## Steps to Reproduce

1. Boot the emulator and call UpdateEnrollmentStatus(status=Active); wait for the analysis to become due.
2. Call GetEC2InstanceRecommendations with no preference stored and observe that each option omits savingsOpportunityAfterDiscounts (the crate's rule: the preference activates it).
3. Call GetIdleRecommendations with no preference stored: every idleRecommendation carries savingsOpportunityAfterDiscounts anyway (src/state/idle.rs:180).
4. Call PutRecommendationPreferences(resourceType='Idle', scope={'name':'AccountId','value':<caller account>}, savingsEstimationMode='BeforeDiscounts') — accepted and stored (GetRecommendationPreferences reports it).
5. Call GetIdleRecommendations again: the response is byte-identical, so deactivating the preference deactivates nothing.
6. Repeat steps 3-5 with GetEBSVolumeRecommendations (resourceType='EbsVolume') and GetLambdaFunctionRecommendations (resourceType='LambdaFunction'): same result, while GetEC2InstanceRecommendations / GetAutoScalingGroupRecommendations / GetRDSDatabaseRecommendations / GetECSServiceRecommendations on the same account honour the preference.
