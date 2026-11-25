Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 5261a6608fb5c4ad7b21097cccea3743144efaab

# GetECSServiceRecommendations omits the Region from its page-token fingerprint, so a token minted at one endpoint resumes a walk at another

_Filed: 2026-11-25T07:45:11Z_
_Operation: GetECSServiceRecommendations | Class: pagination_

> _Note: this issue was identified against the code base at commit `5261a6608fb5c4ad7b21097cccea3743144efaab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every paginated reader in this crate binds the inputs that scope its selection into the opaque page token's selection fingerprint, and the four readers whose selection the request's Region scopes bind `&region` explicitly, with a comment saying a token must be refused "at a different Region's endpoint". `GetECSServiceRecommendations` does not: its fingerprint is `(Self::NAME, &account_id, &input.service_arns, &input.filters)` with no Region, although its own handler states two lines earlier that "only that region's services are in scope here" and its candidate set is built per Region (`ecs_service_candidates(&inventory, &region)` / `ecs_named_candidate(&inventory, reference, &region)`).

The crate's own shared paging suite states the rule for this reader and lists it among the Region-scoped readers, asserting a 400 refusal at any other endpoint. That assertion only passes incidentally: the suite's request body is `json!({})`, so at another endpoint the sweep yields no candidate at all and `resume_index` cannot find the boundary ARN, which is what produces the refusal. When the request NAMES `serviceArns`, `ecs_named_candidate` keeps a candidate for an ARN of another Region (it is merely given no configuration), so the candidate sequence is identical at every endpoint, the fingerprint matches, `resume_index` finds the boundary, and the walk resumes at a foreign endpoint against a different result set — exactly the outcome the sibling readers' comments and the suite's own doc say must be refused.

(The same omission exists in GetEC2InstanceRecommendations, GetAutoScalingGroupRecommendations, GetEBSVolumeRecommendations and GetLambdaFunctionRecommendations, but there the boundary ARN is rebuilt from the request's Region, so a cross-endpoint resume is still refused and nothing observable changes. GetECSServiceRecommendations is the one where the omission is observable.)

## Evidence

The reader that omits the Region — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_ecs_service_recommendations.rs:299-310:
```
        // A page token names a boundary of the walk that minted it and of no other: it
        // carries the fingerprint of THIS operation and of the members that select this
        // result set, so a token another reader issued — every one of them hands out a
        // boundary ARN — and one this reader issued for a different selection are both
        // refused rather than resolved against a sequence they name no position in.
        let selection_fingerprint = crate::state::fingerprint(&(
            Self::NAME,
            &account_id,
            &input.service_arns,
            &input.filters,
        ));
```
Its own Region scoping, same file, get_ecs_service_recommendations.rs:230-234:
```
        // Compute Optimizer is a REGIONAL service, and the region of a request is the one
        // its SigV4 credential scope names ([`crate::state::request_region`], the one place
        // this service reads it). An Amazon ECS service belongs to the region its own ARN
        // names, so only that region's services are in scope here.
        let region = crate::state::request_region(ctx);
```
The sibling that binds it — src/ops/get_rds_database_recommendations.rs:269-281:
```
        // The page token carries this operation's name and EVERY input that scopes the
        // selection it was taken over — the Region the request was signed for included, since
        // the databases analysed are that endpoint's — so resuming a walk with a different
        // selection, at a different Region's endpoint, or with a token another operation
        // issued, is refused rather than answered with a page of a different result set.
        let selection_fingerprint = crate::state::fingerprint(&(
            Self::NAME,
            &resource_arns,
            &account_id,
            &filters,
            &input.recommendation_preferences,
            &region,
        ));
```
(the same `&region` is bound at src/ops/get_idle_recommendations.rs:257-264, src/ops/get_license_recommendations.rs:247-253 and src/ops/describe_recommendation_export_jobs.rs:222-223).

Why a named ARN still yields a candidate at a foreign endpoint — src/state/ecs_recommendations.rs:112-126:
```
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
```
contrast the unnamed sweep, which drops them — src/state/ecs_recommendations.rs:98-102:
```
        .filter_map(|record| {
            let reference = parse_ecs_service_arn(&record.arn)?;
            if reference.region != region {
                return None;
            }
```
Test coverage that states the rule and only exercises the shape that passes — tests/all/shared_paging_rules.rs:561-571:
```
const REGION_SCOPED_READERS: &[&str] = &[
    "DescribeRecommendationExportJobs",
    "GetAutoScalingGroupRecommendations",
    "GetEBSVolumeRecommendations",
    "GetEC2InstanceRecommendations",
    "GetECSServiceRecommendations",
```
tests/all/shared_paging_rules.rs:578-583:
```
/// A `nextToken` resumes the walk of the REGIONAL ENDPOINT that minted it and of no other.
///
/// Compute Optimizer is regional: for every reader whose selection the request's Region scopes,
/// the sequence a token names a position in is that endpoint's, so the token is refused at
/// another endpoint rather than applied to its — different — sequence, which would skip items
/// or answer an empty final page a client reads as "the walk is finished".
```
and the request body it asserts that over — tests/all/shared_paging_rules.rs:119-126:
```
        PagedReader {
            operation: "GetECSServiceRecommendations",
            request: json!({}),
```
No test in tests/all/ecs_service_recommendations.rs presents a token across endpoints with `serviceArns` set (grep for `nextToken`/`region` there finds the paging walk at :1167-1186 and the regional sweep at :1246-1299, neither of which crosses a token).

## Steps to Reproduce

1. Boot the emulator with the ECS, CloudWatch and Compute Optimizer services mounted; opt the account in with UpdateEnrollmentStatus(status=Active) and wait out crate::state::ANALYSIS_DELAY.
2. Create two ACTIVE Fargate ECS services in us-east-1 that Compute Optimizer analyses (as tests/all/ecs_service_recommendations.rs does), with ARNs A < B.
3. Call GetECSServiceRecommendations against the us-east-1 endpoint with body {"serviceArns": [A, B], "maxResults": 1}: one recommendation plus a nextToken naming B.
4. Replay the same body with that nextToken against the eu-west-1 endpoint.
5. Observe HTTP 200 with an empty ecsServiceRecommendations list and no nextToken — the token was accepted at an endpoint that did not mint it and the walk reports a different result set as finished.
6. Run the same two steps against GetRDSDatabaseRecommendations or GetIdleRecommendations (which bind &region): the cross-endpoint replay is refused 400 InvalidParameterValueException.
