Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: dcd6c0968bb95ff573300c824ad4ea7810662c7a

# accounting.defaultPurgeTimeInDays = 0 is accepted and persisted, although the member documents "0 isn't a valid value"

_Filed: 2026-11-19T22:27:06Z_
_Operation: UpdateCluster | Class: validation_

> _Note: this issue was identified against the code base at commit `dcd6c0968bb95ff573300c824ad4ea7810662c7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateAccountingRequest$defaultPurgeTimeInDays` (and its create-path twin `AccountingRequest$defaultPurgeTimeInDays`) is modelled `@range(min: -1, max: 10000)` and its documentation adds the one value that range cannot exclude: "0 isn't a valid value." A `@range` with min -1 and max 10000 admits 0, so the boundary validator passes it and no handler re-checks it: `UpdateCluster` hands the request straight to `crate::state::apply_slurm_update`, which stores whatever integer arrived, and the cluster then reports `slurmConfiguration.accounting.defaultPurgeTimeInDays: 0` on every later read — a slurmdbd purge setting the service documents as invalid.

This is exactly the shape of rule this crate already answers itself when the model cannot express it: `scaling_range_violation` states the cross-member `minInstanceCount <= maxInstanceCount` rule, and `resource_name_violation` states the name pattern the boundary regex engine cannot compile — both in `crate::state`, both reached from the handlers that write the value. The excluded-value rule has no such statement anywhere in the crate (`grep -i purge` finds only the two plumbing sites below and one test that uses 7).

The same unchecked value reaches the store through `CreateCluster` (`Accounting::from`), so the check belongs beside the other two in `crate::state`, where both the create and the update path can ask it.

## Evidence

Smithy (smithy_query query="shape" shape="UpdateAccountingRequest"): `defaultPurgeTimeInDays` -> "range": {"min": -1, "max": 10000} with doc "<p>The default value for all purge settings for <code>slurmdbd.conf</code>. ... The default value for <code>defaultPurgeTimeInDays</code> is <code>-1</code>.</p> <p>A value of <code>-1</code> means there is no purge time and records persist as long as the cluster exists.</p> <important> <p> <code>0</code> isn't a valid value.</p> </important>" (identical wording on `AccountingRequest$defaultPurgeTimeInDays`, the CreateCluster path).

The update path stores it unchecked — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/update_cluster.rs:266-270:
```
        if let Some(slurm) = input.slurm_configuration {
            let stored = cluster.slurm_configuration.take();
            cluster.slurm_configuration = Some(crate::state::apply_slurm_update(stored, slurm));
            changed = true;
        }
```
and src/state.rs:1361-1373 (`apply_slurm_update`), which copies the integer through with no value check:
```
        if let Some(accounting) = update.accounting {
            let previous = merged.accounting.take();
            merged.accounting = Some(Accounting {
                default_purge_time_in_days: accounting
                    .default_purge_time_in_days
                    .or_else(|| previous.as_ref().and_then(|a| a.default_purge_time_in_days)),
```
The create path does the same — src/ops/create_cluster.rs:348 (`slurm_configuration: input.slurm_configuration.map(Into::into),`) through src/state.rs:1582-1588:
```
impl From<AccountingRequest> for Accounting {
    fn from(request: AccountingRequest) -> Self {
        Self {
            default_purge_time_in_days: request.default_purge_time_in_days,
```
The crate's own home for a rule the model cannot express as a member constraint, which this value has no entry in — src/state.rs:1257-1273:
```
/// The modelled `ValidationException` a scaling configuration the cluster could never
/// satisfy is refused with, or `None` when the service accepts it.
///
/// The model bounds each member of `ScalingConfigurationRequest` on its own; that
/// `minInstanceCount` must not exceed `maxInstanceCount` is a cross-member rule only the
/// service can state, and it is stated once here so every path that stores a scaling
/// configuration refuses the same values in the same words.
pub fn scaling_range_violation(
```

Test coverage: the only test that sends the member uses a valid 7 and asserts the read-back — tests/all/cluster_update_and_endpoints.rs:488 (`"slurmConfiguration": {"accounting": {"mode": "STANDARD", "defaultPurgeTimeInDays": 7}}`) and :499. No test sends 0, so nothing pins the current acceptance.

## Steps to Reproduce

1. CreateCluster(clusterName="purge-host", scheduler={type:SLURM,version:"24.11"}, size=SMALL, networking={subnetIds:[<a subnet EC2 holds>]}, slurmConfiguration={accounting:{mode:"STANDARD"}}) -> 200; one more request settles it ACTIVE.
2. UpdateCluster(clusterIdentifier="purge-host", slurmConfiguration={accounting:{mode:"STANDARD", defaultPurgeTimeInDays:0}}).
3. Expected per the member documentation ("0 isn't a valid value"): refused with the modelled ValidationException, the cluster left as it was.
4. Observed: HTTP 200, status UPDATING, and GetCluster reports cluster.slurmConfiguration.accounting.defaultPurgeTimeInDays = 0 from then on.
5. The same value on the create path is accepted too: CreateCluster(..., slurmConfiguration={accounting:{mode:"STANDARD", defaultPurgeTimeInDays:0}}) -> 200 with the 0 persisted.
