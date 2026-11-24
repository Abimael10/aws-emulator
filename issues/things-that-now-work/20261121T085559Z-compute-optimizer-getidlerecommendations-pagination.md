Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: ef271dbe26f379074e11bf8bd2be74a0451dc7db

# Page token fingerprint omits the Region the idle selection is reported in, so a token resumes another Region's walk

_Filed: 2026-11-21T08:55:59Z_
_Operation: GetIdleRecommendations | Class: pagination_

> _Note: this issue was identified against the code base at commit `ef271dbe26f379074e11bf8bd2be74a0451dc7db`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This reader binds a selection fingerprint into its `nextToken` and states that a walk resumed with a different selection must be refused "rather than answered with a page of a different result set". The selection IS region-dependent: the stored idle analyses are region-free, and each reported recommendation's `resourceArn` is stamped with the Region of the endpoint the request addressed (`analysis.to_recommendation(&region, ...)`), which is what the `resourceArns` selection is matched against. But `region` is read AFTER the fingerprint is computed and is never bound into it.

So a token minted at one Region's endpoint for a given `resourceArns` list is ACCEPTED at another Region's endpoint for the same list — where every named ARN carries the first Region and therefore matches nothing — and the caller gets a successful empty page (`offset_page` over an empty selection yields no items and no token), which a client reads as "no more idle recommendations", instead of the `InvalidParameterValueException` the handler's own comment promises for a token that names no position in this result set.

The sibling `GetLicenseRecommendations`, region-scoped in exactly the same way and using the same offset-token helpers, binds `&region` into its fingerprint. The ARN-token readers (EBS, Lambda, EC2, ASG, ECS) are immune because the boundary they resume at is itself a region-stamped ARN. This reader is the remaining offset-token reader that neither binds the Region nor carries it in the cursor.

## Evidence

The fingerprint, with no region — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_idle_recommendations.rs:247-256:
```
        // The page token carries this operation's name and the selection it was taken with,
        // so resuming a walk with a different selection, or with a token another operation
        // issued, is refused rather than answered with a page of a different result set.
        let selection_fingerprint = crate::state::fingerprint(&(
            Self::NAME,
            &resource_arns,
            &account_id,
            &filters,
            &input.order_by,
        ));
```
The Region, read later, is what the selection is matched on — same file, lines 297-299 and 319-342:
```
        let region = crate::state::request_region(ctx);
...
        let analysed: Vec<IdleRecommendation> = guard
            .visible_idle_recommendations()
            .map(|analysis| {
                analysis.to_recommendation(&region, look_back_period, last_refresh_timestamp)
            })
            .collect();
...
            .filter(|recommendation| {
                resource_arns.is_empty()
                    || recommendation
                        .resource_arn
                        .as_ref()
                        .is_some_and(|arn| resource_arns.contains(arn))
            })
```
The stored rows really are region-free, so the Region only enters through the reported ARN — src/state/idle.rs:47-48 and 87-89:
```
/// keeps the resource's REGION-FREE identity for the reason [`resource_arn_in`] gives: the
/// ARN a reader reports names the region of the endpoint the request addressed.
...
    /// This resource's ARN as the endpoint in `region` reports it ([`resource_arn_in`]).
    pub fn arn_in(&self, region: &str) -> String {
```
The sibling that binds the Region — src/ops/get_license_recommendations.rs:247-253:
```
        let selection_fingerprint = crate::state::fingerprint(&(
            Self::NAME,
            &resource_arns,
            &account_id,
            &filters,
            &region,
        ));
```
Test coverage: tests/all/shared_paging_rules.rs:417-426 lists the readers whose "token does not resume a different selection" rule is pinned (`autoScalingGroupArns`, `volumeArns`, `instanceArns`, `serviceArns`, `functionArns`) — `GetIdleRecommendations` is not among them, and tests/all/idle_recommendations.rs pages within one Region only, so nothing pins today's behaviour.

## Steps to Reproduce

1. Opt in with UpdateEnrollmentStatus(status=Active) and let the analysis become due.
2. Sign a GetIdleRecommendations call for us-east-1 with `{"resourceArns": [<two us-east-1 ARNs the reader reports>], "maxResults": 1}` and keep the returned `nextToken`.
3. Replay `{"resourceArns": [<the same two us-east-1 ARNs>], "maxResults": 1, "nextToken": <that token>}` signed for eu-west-1.
4. Expected: InvalidParameterValueException — the token names no position in the eu-west-1 result set, which is what the handler's comment at lines 247-249 promises.
5. Actual: HTTP 200 with an empty `idleRecommendations` and no `nextToken`, indistinguishable from "the walk is finished".
