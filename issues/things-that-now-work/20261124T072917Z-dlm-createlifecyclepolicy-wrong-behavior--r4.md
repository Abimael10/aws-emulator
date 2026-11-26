Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: a6beefae630d63d931f6e3dc50dceb328128e26d

# PolicyDetails.Parameters keeps no materialized defaults, so NoReboot and ExcludeBootVolume are absent from every read although their documentation states the value the service fills in

_Filed: 2026-11-24T07:29:17Z_
_Operation: CreateLifecyclePolicy | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `a6beefae630d63d931f6e3dc50dceb328128e26d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`materialized_policy_details` is this crate's one place where a submitted `PolicyDetails` is stamped with the members the service itself fills in, and its own doc comment states the criterion for what is left alone: a member is NOT stamped only when "neither the model, the documentation nor a recorded read states a value the service fills in". Two members of `PolicyDetails$Parameters` fall on the other side of that criterion — the model documents `NoReboot` as "The default is true (instances are not rebooted)" and `ExcludeBootVolume` as "The default is false" — yet the function never touches `details.parameters` at all: it stamps `PolicyLanguage`, `PolicyType`, `ResourceLocations`, per-schedule `CopyTags`, `CreateRule$Location`, the two rule intervals and a cross-Region copy rule's `CopyTags`, and stops there.

The result is that a custom AMI policy (or a custom snapshot policy targeting instances) created with a `Parameters` structure that omits those members is stored and read back with them absent, while every sibling member whose only basis is the same kind of documentation sentence comes back filled in. A client that reads a policy to learn whether its targeted instances are rebooted, or whether boot volumes are excluded, gets nothing, and a read-modify-write round trip cannot see the value the policy actually runs on. The crate already demonstrates that a documentation sentence alone is a sufficient basis for stamping: `DEFAULT_COPY_TAGS`, `DEFAULT_EXTEND_DELETION`, `DEFAULT_CREATE_INTERVAL` and `DEFAULT_RETAIN_INTERVAL` are all justified by nothing more than "If you do not specify a value, the default is …" and are all reported by `effective_policy_details`.

Both write paths are affected (`UpdateLifecyclePolicy` calls the same helper when it replaces the configuration); the create is where the member is first stored and read back.

## Evidence

The crate's own criterion for what is left unstamped — `services/service-com-amazonaws-dlm-dlm-20180112/src/state.rs:532-536`:
```
/// What is NOT stamped: a member for which neither the model, the documentation
/// nor a recorded read states a value the service fills in — a count, an
/// interval unit, a nested deprecate/archive/fast-restore rule's members. They
/// keep exactly what the request stated, because inventing one would be
/// fabrication.
```
The model DOES state a value for two `Parameters` members (`smithy_query(query="shape", shape="Parameters")`):
```
"NoReboot": ... "[Custom AMI policies only] Indicates whether targeted instances are rebooted when the lifecycle policy runs. ... The default is true (instances are not rebooted)."
"ExcludeBootVolume": ... "[Custom snapshot policies that target instances only] Indicates whether to exclude the root volume from multi-volume snapshot sets. The default is false."
```
The stamping body never reaches `details.parameters` — `src/state.rs:592-611`:
```
    details
        .resource_locations
        .get_or_insert_with(|| vec![CLOUD_LOCATION.to_string()]);
    for schedule in details.schedules.iter_mut().flatten() {
        schedule.copy_tags.get_or_insert(DEFAULT_SCHEDULE_COPY_TAGS);
        if let Some(create_rule) = schedule.create_rule.as_mut() {
            ...
            create_rule.interval.get_or_insert(UNSET_RULE_INTERVAL);
        }
        if let Some(retain_rule) = schedule.retain_rule.as_mut() {
            retain_rule.interval.get_or_insert(UNSET_RULE_INTERVAL);
        }
        for copy_rule in schedule.cross_region_copy_rules.iter_mut().flatten() {
            copy_rule.copy_tags.get_or_insert(DEFAULT_COPY_TAGS_ON_COPY);
        }
    }
    details
```
The sibling stamp justified by a documentation sentence alone — `src/state.rs:152-157`:
```
/// Whether a schedule copies the source resource's tags when the request states
/// no `Schedule$CopyTags`: the member targets the model's non-nullable `CopyTags`
/// boolean ... and `CopyTags` is documented "If you do not specify a value, the
/// default is false."
pub const DEFAULT_SCHEDULE_COPY_TAGS: bool = false;
```
and the same basis for the default-policy members, `src/state.rs:187-195` (`DEFAULT_COPY_TAGS`, `DEFAULT_EXTEND_DELETION`), which `effective_policy_details` (`src/state.rs:771-772`) does report.

The member exists in the generated type and is written nowhere: `src/ops/types/shared_types.rs:178` declares `pub no_reboot: Option<bool>,` and a repo-wide grep for `no_reboot` matches only that declaration.

Test coverage: `tests/all/policy_read_projection.rs` is the projection table, and its comparison allows a materialized member the request did not name (`tests/all/policy_read_projection.rs:164`: "member (so a materialized member `sent` does not name is allowed)"). No case in it states `PolicyDetails.Parameters` at all (the only `"Parameters"` key in the table, line 419, is `EventSource$Parameters`), so this path is unpinned and filling the documented defaults leaves the existing assertions unchanged.

## Steps to Reproduce

1. POST /policies, SigV4-signed for dlm in us-east-1, with ExecutionRoleArn naming a role of the emulator account, Description, State "ENABLED", and PolicyDetails = {"PolicyType":"IMAGE_MANAGEMENT","ResourceTypes":["INSTANCE"],"TargetTags":[{"Key":"k","Value":"v"}],"Schedules":[{"Name":"s1","CreateRule":{"Interval":24,"IntervalUnit":"HOURS"},"RetainRule":{"Count":2}}],"Parameters":{}} — note the returned PolicyId
2. GET /policies/{PolicyId}
3. Observe Policy.PolicyDetails.Schedules[0].CopyTags is reported as false and CreateRule.Interval/RetainRule.Interval are reported, i.e. the documented defaults were stamped at create
4. Observe Policy.PolicyDetails.Parameters carries no NoReboot member, although the model documents it as "The default is true (instances are not rebooted)" — the same kind of documentation sentence that justified the stamps in step 3 (src/state.rs:532-536 vs src/state.rs:592-611)
5. Repeat with PolicyType "EBS_SNAPSHOT_MANAGEMENT", ResourceTypes ["INSTANCE"] and Parameters {"ExcludeDataVolumeTags":[]}: the read reports no ExcludeBootVolume, although it is documented "The default is false"
