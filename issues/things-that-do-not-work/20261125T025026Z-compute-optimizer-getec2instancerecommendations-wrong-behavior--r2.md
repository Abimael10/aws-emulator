Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: cc91cd412557a223e827e0d947b209de54e44d58

# Analyse the account's real Amazon EC2 instances instead of a hardcoded in-crate instance inventory

_Filed: 2026-11-25T02:50:26Z_
_Operation: GetEC2InstanceRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `cc91cd412557a223e827e0d947b209de54e44d58`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

GetEC2InstanceRecommendations reports three instances baked into this crate's own state module — `i-0a1b2c3d4e5f60001` (m5.2xlarge), `...0002` (m5.large) and `...0003` — together with their utilization, candidate instance types, tags and instance state. The Amazon EC2 emulator's own instance inventory is never consulted, so an instance a client actually launches with RunInstances is reported by nobody (its ARN selects nothing and `errors` is always answered empty, src/ops/get_ec2_instance_recommendations.rs:381), while every response names instance ARNs the EC2 service does not hold.

That contradicts the rule this crate states for itself and already implements for four of its other analysis surfaces. `src/state.rs:3682-3689` says a resource the account owns in another service is projected from the inventory that holds it and the utilization published for it when a read asks for it — and `analysed_volumes`, `analysed_functions`, `analysed_databases` and the Amazon ECS sweep do exactly that, reading `ec2::state` / `lambda` / `rds` / `ecs` plus the CloudWatch metric store. An Amazon EC2 instance is equally "a resource the account owns elsewhere": the same `ec2::state` store this crate already locks for the EBS volume analysis (`src/state/ebs_inventory.rs:116`, `inventory.volumes.values()`) holds `ec2::state::InstanceRecord` (services/service-com-amazonaws-ec2-amazonec2/src/state/store.rs:571), and both crates are already dependencies (Cargo.toml:37, :51). The instance surface is the one analysis path left on an in-crate fixture.

The same seed is the single source of the `Ec2Instance` rows of the findings catalog (src/state.rs:3691-3698), so GetRecommendationSummaries counts the fictitious instances too, and GetEC2RecommendationProjectedMetrics projects their utilization — one cause, several surfaces.

## Evidence

The crate's own rule, broken here — src/state.rs:3682-3689:
```
/// A resource type whose analysis this service computes from ANOTHER service's inventory has
/// no row here at all: an Amazon Aurora or RDS database, an Amazon EBS volume, a Lambda
/// function version and an Amazon ECS service are resources the account owns elsewhere, so
/// those rows are projected from the inventory that holds them and the utilization published
/// for them when a read asks for them ([`rds_inventory::analysed_databases`],
/// [`ebs_inventory::analysed_volumes`], [`lambda_inventory::analysed_functions`] and
/// [`ecs_recommendations::analysed_ecs_service_resources`]) rather than seeded here
```
The instance surface instead reads a fixture — src/state.rs:3649-3652:
```
            instances: observed_instances()
                .into_iter()
                .map(|instance| (instance.instance_id.clone(), instance))
                .collect(),
```
src/state/instances.rs:509-517 and :531, :559-563:
```
/// The Amazon EC2 instance inventory this emulator observes, generated once when the store
/// is created.
///
/// The emulator watches no real workload, so WHICH instances it holds and the utilization
/// observed on them are a fixed observation rather than something derived from traffic.
...
pub(super) fn observed_instances() -> Vec<ObservedInstance> {
...
        ObservedInstance {
            instance_id: "i-0a1b2c3d4e5f60001".to_string(),
            account_id: account_id.clone(),
            current: m5_2xlarge(),
```
and the handler answers from exactly that set — src/ops/get_ec2_instance_recommendations.rs:289-297:
```
        let selected: Vec<&crate::state::ObservedInstance> = guard
            .visible_instances()
            .filter(|instance| instance.account_id == account_id)
```
The sibling surface that does it right, in this same crate — src/state/ebs_inventory.rs:112-136:
```
    let (Some(inventory), Some(observations)) = (
        ec2::state::service_state(engine),
        cloudwatch::state::service_state(engine),
    ) else {
        return Vec::new();
    };
...
    inventory
        .volumes
        .values()
```
The dependencies a fix needs are already declared — Cargo.toml:51 `service-com-amazonaws-ec2-amazonec2 = { workspace = true }` ("The catalog a preferred-resource value is resolved against: the Amazon EC2 instance types that service's model names") and Cargo.toml:37 the CloudWatch crate.

Test coverage: tests/all/ec2_instance_recommendations.rs:216-218 writes its own corpus straight into the store (`guard.instances.clear(); ... guard.instances.insert(instance.instance_id.clone(), instance);`) and tests/all/ec2_recommendation_projected_metrics.rs:244 does the same — no test creates an instance with EC2 RunInstances and asks Compute Optimizer about it, so nothing pins the fixture as a deliberate answer over the wire.

## Steps to Reproduce

1. Boot the emulator with the EC2, CloudWatch and Compute Optimizer services mounted.
2. Launch an instance with EC2 RunInstances (say an m5.2xlarge) and note its instance id and ARN.
3. Call Compute Optimizer UpdateEnrollmentStatus with status=Active and wait out the analysis delay (crate::state::ANALYSIS_DELAY).
4. Call GetEC2InstanceRecommendations with instanceArns=[the ARN of the instance just launched]: observe HTTP 200 with an empty instanceRecommendations list and an empty errors list.
5. Call GetEC2InstanceRecommendations with no instanceArns: observe recommendations for i-0a1b2c3d4e5f60001, i-0a1b2c3d4e5f60002 and i-0a1b2c3d4e5f60003 — instances EC2 does not hold (src/state/instances.rs:554-625).
6. Call GetRecommendationSummaries: the Ec2Instance summary counts those same fixture instances (src/state.rs:3691-3698), not the instance the account actually owns.
