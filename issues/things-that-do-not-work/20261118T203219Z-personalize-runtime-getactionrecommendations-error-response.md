Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 888a7dd41752985eb1de1aec0bdb6cbbaaae5598

# GetActionRecommendations answers the invalid-campaign-ARN refusal before the request-only numResults bound, where its sibling settles the bound first

_Filed: 2026-11-18T20:32:19Z_
_Operation: GetActionRecommendations | Class: error-response_

> _Note: this issue was identified against the code base at commit `888a7dd41752985eb1de1aec0bdb6cbbaaae5598`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetActionRecommendations` settles two refusals that both read nothing but the request: a `campaignArn` the request did not supply at all, and a `numResults` past the documented maximum. The handler orders the ARN refusal FIRST (`get_action_recommendations.rs:188-190`, before `effective_num_results` at `:197-202`), and the comment it writes over the `numResults` check claims the opposite — that such a `numResults` "is refused whatever the campaign turns out to be".

So a request that is malformed in both ways is answered differently by the two sibling handlers: `GetActionRecommendations` with no `campaignArn` and `numResults=101` answers `The given campaign ARN is invalid: null`, while `GetRecommendations` with neither `campaignArn` nor `recommenderArn` and `numResults=501` answers the `numResults` clause, because that handler computes the bound (`get_recommendations.rs:245-253`) before `resolve` reaches its no-target refusal (`:263-268`, `NO_INFERENCE_TARGET`). One of the two orders is wrong, and the comment in `GetActionRecommendations` says it is that handler's.

## Evidence

The comment and the code it sits over, `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/ops/get_action_recommendations.rs:182-202`:
```
        // 1. What the REQUEST alone settles, before anything is resolved.
        //    `campaignArn` carries no `@required` in the model, but it is "the campaign
        //    to use for getting action recommendations" ...
        let Some(campaign_arn) = registry::present(input.campaign_arn.as_deref()) else {
            return Err(invalid_campaign_arn(input.campaign_arn.as_deref()));
        };
        //    How many results to compute is the value the caller sent against the
        //    documented maximum ("The default is 5. The maximum is 100."), both facts
        //    about the request -- so it is answered BEFORE the campaign is resolved,
        //    exactly as the sibling operations settle their request-only bounds: a
        //    `numResults` past the bound is refused whatever the campaign turns out to
        //    be.
        let num_results = registry::effective_num_results(
            input.num_results,
            registry::DEFAULT_NUM_ACTION_RESULTS,
            registry::MAX_NUM_ACTION_RESULTS,
        )
        .map_err(deny)?;
```
The sibling that does settle its request-only bound first, `src/ops/get_recommendations.rs:245-268`:
```
        let max_num_results = registry::metadata_bounded_max(registry::asks_for_metadata(
            input.metadata_columns.as_ref(),
        ));
        let num_results = registry::effective_num_results(
            input.num_results,
            registry::DEFAULT_NUM_RESULTS,
            max_num_results,
        )
        .map_err(deny)?;
        ...
        let resolved = resolve(
            &guard,
            Self::NAME,
            registry::present(input.campaign_arn.as_deref()),
            registry::present(input.recommender_arn.as_deref()),
        )?;
```
where `resolve` is what answers a request naming no target: `src/ops/get_recommendations.rs:516` — `(None, None) => Err(invalid(NO_INFERENCE_TARGET))`.

Test coverage: `tests/all/request_only_refusal_order.rs:6-10` states the rule ("a refusal whose every input comes from the REQUEST ... is settled BEFORE the request's campaign or recommender is resolved, so one malformed request earns one refusal whatever the registry holds"), and its `ACTION_RECOMMENDATIONS` table (`:300-336`) carries only rows whose `campaignArn` IS supplied, so the combination above is unasserted on either operation.

## Steps to Reproduce

1. POST `/action-recommendations` with body `{"userId": "user-1", "numResults": 101}` (no `campaignArn`; the member carries no `@required` trait, so the boundary admits the body).
2. Observe HTTP 400 `InvalidInputException` with message `The given campaign ARN is invalid: null` — the `numResults` bound the handler's own comment says is "refused whatever the campaign turns out to be" was never evaluated.
3. POST `/recommendations` with body `{"userId": "user-1", "numResults": 501}` (no `campaignArn`, no `recommenderArn`).
4. Observe HTTP 400 `InvalidInputException` with message `Value '501' at 'numResults' failed to satisfy constraint: Member must have value less than or equal to 500` — the sibling settles the request-only bound first and does not answer its own `campaignArn or recommenderArn required.` refusal.
5. Compare the two: the same pair of request-only faults earns the bound clause on one operation and the resource clause on the other.
