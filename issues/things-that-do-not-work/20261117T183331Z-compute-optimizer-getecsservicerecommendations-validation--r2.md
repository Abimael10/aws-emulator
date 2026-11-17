Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 5f72b15ceea68fb83a63d5bc988a846400d3d8ef

# The crate's identifier-list bound is applied by only one reader, not by the siblings it was stated once for

_Filed: 2026-11-17T18:33:31Z_
_Operation: GetECSServiceRecommendations | Class: validation_

> _Note: this issue was identified against the code base at commit `5f72b15ceea68fb83a63d5bc988a846400d3d8ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::too_many_identifiers` exists so that "no two readers of this service bound their identifier list differently", but it is called from exactly one handler (`GetEC2InstanceRecommendations`, for `instanceArns`). Every other reader that takes a caller-supplied identifier list accepts a list of any length and does per-identifier work before paging: `GetECSServiceRecommendations` parses each `serviceArns` entry and builds one `EcsServiceCandidate` per entry, `GetEBSVolumeRecommendations` collects one volume id per `volumeArns` entry, `GetLambdaFunctionRecommendations` one selector per `functionArns` entry, and `GetAutoScalingGroupRecommendations`, `GetIdleRecommendations`, `GetLicenseRecommendations`, `GetRDSDatabaseRecommendations` walk their `autoScalingGroupArns`/`resourceArns` lists the same way. So the same request shape — 2000 ARNs — is refused with `InvalidParameterValueException` on the instance surface and accepted on every other surface, and on those surfaces the work and allocation a request drives is sized by the list the caller sent, which is exactly what the shared helper exists to prevent.

## Evidence

The shared rule and its stated scope — `src/state.rs:186-196`:
```
/// Why an ARN list a request carries is too long to answer ([`MAX_REQUESTED_IDENTIFIERS`]),
/// naming the member and the bound, or `None` when it is within it. Stated once so no two
/// readers of this service bound their identifier list differently.
pub fn too_many_identifiers(member: &str, named: usize) -> Option<String> {
```
The single call site — `src/ops/get_ec2_instance_recommendations.rs:450-459`: "The model bounds this list's length nowhere, so the service does: ... a request may name at most the crate's ceiling ([`crate::state::MAX_REQUESTED_IDENTIFIERS`]) so neither the response nor the work that builds it can be sized by the length of the list a caller sent", followed by `if let Some(reason) = crate::state::too_many_identifiers("instanceArns", instance_arns.len())`.

A grep for `too_many_identifiers|MAX_REQUESTED_IDENTIFIERS` over `src/` returns only `src/state.rs` and `src/ops/get_ec2_instance_recommendations.rs`.

The sibling that needs it and skips it — `src/ops/get_ecs_service_recommendations.rs:271-284`:
```
let mut requested: Vec<crate::state::EcsServiceRef> = Vec::new();
for arn in input.service_arns.iter().flatten() { ... requested.push(reference); }
```
whose entries each become a candidate at `:347-353` (`requested.into_iter().map(|reference| crate::state::ecs_named_candidate(&inventory, reference, &region))`) before `page_size` is applied. Same shape in `src/ops/get_ebs_volume_recommendations.rs:413-434` (`requested_volume_ids`, no length check) and `src/ops/get_lambda_function_recommendations.rs:413-443` (`requested_functions`, no length check).

Test coverage: the bound is pinned for the one reader that applies it — `tests/all/ec2_instance_recommendations.rs:1023-1051` ("bound is the crate's ([`MAX_REQUESTED_IDENTIFIERS`]), so a list at it is answered" and a list past it is refused with the bound in the message). No sibling reader has such a test, and no comment in those handlers states a reason for not bounding their list.

## Steps to Reproduce

1. Start the emulator, call UpdateEnrollmentStatus with status=Active and let the analysis become due.
2. Call GetEC2InstanceRecommendations with 1001 well-formed instance ARNs in `instanceArns`: it is refused with InvalidParameterValueException naming the bound.
3. Call GetECSServiceRecommendations with 1001 well-formed `serviceArns`, GetEBSVolumeRecommendations with 1001 `volumeArns`, and GetLambdaFunctionRecommendations with 1001 `functionArns`: each is accepted with HTTP 200 after building one candidate/selector per named ARN.
4. Note that the two answers to the same request shape disagree, while src/state.rs:186-188 states the bound is "Stated once so no two readers of this service bound their identifier list differently".
