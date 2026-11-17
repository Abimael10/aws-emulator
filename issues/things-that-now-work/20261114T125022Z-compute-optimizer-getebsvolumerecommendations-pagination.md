Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 9a1cab38181c81438a46747e76c041451acccd46

# A nextToken minted by one resource-ARN reader is silently accepted by another, yielding an empty final page instead of a refusal

_Filed: 2026-11-14T12:50:22Z_
_Operation: GetEBSVolumeRecommendations | Class: pagination_

> _Note: this issue was identified against the code base at commit `9a1cab38181c81438a46747e76c041451acccd46`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`encode_resource_page_token` / `decode_resource_page_token` carry only the boundary ARN — no operation discriminator — and five readers share that one encoding (GetEC2InstanceRecommendations, GetAutoScalingGroupRecommendations, GetEBSVolumeRecommendations, GetLambdaFunctionRecommendations, GetECSServiceRecommendations). The decoder's own doc claims it returns `None` for "a token issued for a different page sequence, whose encoded shape is not this one", but the encoded shape is IDENTICAL across all five, so a token one reader issued decodes cleanly in another.

GetEC2InstanceRecommendations and GetAutoScalingGroupRecommendations catch this with a second check — the decoded ARN must be a position in the current selection, otherwise the request is refused — and the offset-token family folds the operation name into its fingerprint for exactly this reason. GetEBSVolumeRecommendations (and GetLambdaFunctionRecommendations, and GetECSServiceRecommendations) apply the decoded ARN only as an ordering comparison (`arn >= start` / `arn < start`), so a foreign token is accepted: because `arn:aws:ec2:...:volume/...` sorts before `arn:aws:lambda:...`, feeding a Lambda reader's token to GetEBSVolumeRecommendations filters out every volume and answers 200 with an empty `volumeRecommendations` and no `nextToken` — which a client reads as "the walk is finished" rather than as the bad token it was.

## Evidence

The shared, undiscriminated token — `src/state.rs:1284-1300`:
```
/// The opaque `nextToken` a recommendation reader hands out: the ARN of the resource the
/// next page starts at, hex-encoded so a client can neither read nor construct one.
pub fn encode_resource_page_token(resource_arn: &str) -> String {
    ::hex::encode(::serde_json::json!({ "startAt": resource_arn }).to_string())
}

/// The resource ARN a `nextToken` this service issued stands for, or `None` for a token
/// it did not — including a token issued for a different page sequence, whose encoded
/// shape is not this one.
pub fn decode_resource_page_token(token: &str) -> Option<String> {
```
The permissive use — `src/ops/get_ebs_volume_recommendations.rs:285-300`:
```
            .filter(|recommendation| {
                recommendation
                    .volume_arn
                    .as_deref()
                    .is_some_and(|arn| resume_at.as_deref().is_none_or(|start| arn >= start))
            })
...
        let next_token = matching.get(page_size).and_then(|recommendation| {
```
(the same `arn >= start` in `src/ops/get_lambda_function_recommendations.rs:295-300`, and `arn < start → continue` in `src/ops/get_ecs_service_recommendations.rs:451-457`).

The sibling that refuses it — `src/ops/get_ec2_instance_recommendations.rs:314-330`:
```
        // ... A token
        // this service issued whose instance the current selection does not hold cannot name
        // a boundary of THIS result set ... so it is refused rather than answered with an empty final page a caller
        // would read as "no more recommendations".
        let start = match &resume_at {
            None => 0,
            Some(arn) => match resolved.iter().position(|(_, held, _)| held == arn) {
                Some(index) => index,
                None => {
                    return Err(invalid_parameter(
```
and the rule the other token family states — `src/state.rs:2162-2164`: "own operation name into the selection fingerprint, so one reader's token resumes no other's walk."
Test coverage: `tests/all/ebs_volume_recommendations.rs:654-696` walks pages with this service's own tokens; `tests/all/shared_paging_rules.rs:1-29` asserts the shared paging rules; no test feeds one reader's token to another.

## Steps to Reproduce

1. Boot the emulator, opt in and let the analysis become due, with more Lambda functions than one page holds and at least one EBS volume recommendation.
2. Call GetLambdaFunctionRecommendations with {"maxResults": 1} and keep the returned nextToken.
3. Call GetEBSVolumeRecommendations with {"nextToken": "<that token>"}: the response is 200 with an empty volumeRecommendations list and no nextToken, instead of the InvalidParameterValueException GetEC2InstanceRecommendations answers an out-of-selection token with.
