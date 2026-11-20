Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: ee8860a66886d6dfcfd5e6a9cf8e58fb6efb9023

# The crate's shared identifier-list bound is applied by exactly one reader, so sibling readers accept unbounded ARN lists

_Filed: 2026-11-15T11:55:41Z_
_Operation: GetAutoScalingGroupRecommendations | Class: validation_

> _Note: this issue was identified against the code base at commit `ee8860a66886d6dfcfd5e6a9cf8e58fb6efb9023`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::too_many_identifiers` exists to bound the ARN list a reader accepts, and its doc says the rule is stated once so that no two readers of this service bound their identifier list differently. In fact only `GetEC2InstanceRecommendations` calls it. Every other reader that takes a caller-supplied identifier list — `autoScalingGroupArns` here, and `volumeArns`, `functionArns`, `serviceArns`, `resourceArns` and `jobIds` in the siblings — validates each entry in a loop with no length check, so a request naming 10,000 ARNs is refused by one reader and accepted (with the per-identifier parse and selection work it implies) by all the others. Either the shared bound is the crate's rule for every reader, as its doc claims, or one reader refuses what the rest accept; today both are true at once.

## Evidence

The shared rule and its claim, `src/state.rs:173-194`:

```
/// The most resource identifiers one request may name in a reader's ARN list, whatever the
/// model declares no bound for: a request naming more is REFUSED with that operation's
/// `InvalidParameterValueException` rather than answered.
...
pub const MAX_REQUESTED_IDENTIFIERS: usize = MAX_PAGE_SIZE;

/// Why an ARN list a request carries is too long to answer ([`MAX_REQUESTED_IDENTIFIERS`]),
/// naming the member and the bound, or `None` when it is within it. Stated once so no two
/// readers of this service bound their identifier list differently.
pub fn too_many_identifiers(member: &str, named: usize) -> Option<String> {
```

The one caller, `src/ops/get_ec2_instance_recommendations.rs:441-445`:

```
    if let Some(reason) =
        crate::state::too_many_identifiers("instanceArns", instance_arns.len())
    {
        return Err(invalid_parameter(reason));
    }
```

This handler's unbounded loop over the same kind of list, `src/ops/get_auto_scaling_group_recommendations.rs:229-242`:

```
        let requested_arns: Vec<String> = input.auto_scaling_group_arns.clone().unwrap_or_default();
        for arn in &requested_arns {
            let parsed = crate::state::parse_resource_arn(arn).filter(|parsed| {
                parsed.resource_type == crate::state::RESOURCE_TYPE_AUTO_SCALING_GROUP
            });
            let Some(parsed) = parsed else {
                return Err(invalid_parameter(
                    "Invalid Auto Scaling group ARN.".to_string(),
                ));
            };
```

The same omission in the other sibling readers: `src/ops/get_ebs_volume_recommendations.rs:383-404` (`volumeArns`), `src/ops/get_lambda_function_recommendations.rs:384-414` (`functionArns`), `src/ops/get_license_recommendations.rs:209-220` (`resourceArns`), `src/ops/get_idle_recommendations.rs:209-223` (`resourceArns`), `src/ops/get_rds_database_recommendations.rs:221-230` (`resourceArns`), `src/ops/get_ecs_service_recommendations.rs:318-331` (`serviceArns`) — none of them mentions or calls `too_many_identifiers`, and a grep of the crate shows `too_many_identifiers` has exactly one call site.

## Steps to Reproduce

1. Opt in with `UpdateEnrollmentStatus {status: Active}`.
2. Call `GetEC2InstanceRecommendations` with 1001 well-formed instance ARNs — the request is refused with `InvalidParameterValueException` naming the shared bound.
3. Call `GetAutoScalingGroupRecommendations` with 1001 well-formed Auto Scaling group ARNs (or `GetEBSVolumeRecommendations` with 1001 volume ARNs) — the request is accepted and answered.
4. Observe two readers of the same service disagree about the identifier-list bound the shared helper's doc says is stated once for all of them.
