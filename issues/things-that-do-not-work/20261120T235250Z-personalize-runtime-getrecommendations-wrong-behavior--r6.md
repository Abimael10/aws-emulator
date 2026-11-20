Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 7012ff15aecebd44e821216c3792d3327cf3e3b1

# A filter whose create has not completed is applied instead of refused: the registry's ACTIVE rule is enforced for campaigns only, and the filter seam keeps no activation instant at all

_Filed: 2026-11-20T23:52:50Z_
_Operation: GetRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `7012ff15aecebd44e821216c3792d3327cf3e3b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states ONE rule for a well-formed resource ARN the inference plane cannot serve: `Denied::unresolvable` is "The refusal for a well-formed resource ARN the registry cannot resolve to an ACTIVE campaign, recommender or filter", and its message is `<arn> does not exist or is not active yet.` The doc under that refusal names filters explicitly and even quotes the Smithy warning on `filterArn` ("When using this parameter, be sure the filter resource is ACTIVE.").

Only ONE of the three read paths enforces it. `State::campaign` resolves a campaign through `.filter(|campaign| campaign.is_active(SystemTime::now()))`, so a campaign whose create the clock has not completed earns exactly that refusal. `State::filter` (the path `bind_filter` takes for `GetRecommendations$filterArn`, the promotion's own `filterArn`, and the same member on both sibling operations) and `State::recommender` apply no such check — and cannot: `Filter` carries no activation instant, and `register_filter` accepts none, so no filter in this registry can ever be "created, not ACTIVE yet".

The condition is not hypothetical in the assembled emulator: the sibling control plane's `CreateFilter` registers the filter in this registry at `create_filter.rs:249` and then records it with `active_at: due_after(now, group_active_at)` — a FUTURE instant from which `DescribeFilter` renders `CREATE PENDING` — while `CreateCampaign` hands this crate the campaign's own due instant (`.deployed(&input.solution_version_arn, active_at)`). So a `GetRecommendations` call naming a filter the control plane reports as `CREATE PENDING` is served with that filter applied (items included/excluded by it), where the crate's own refusal says the request must be answered `InvalidInputException` 400 "does not exist or is not active yet" — the answer the same request earns one line earlier for a campaign in the identical state.

The mechanism the fix needs already exists for campaigns (`Campaign::active_at`, `Campaign::is_active`, and a seam parameter carrying the instant); the filter seam simply drops it.

## Evidence

The rule, stated once in this crate — `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/state.rs:676-687`:
```
    /// The refusal for a well-formed resource ARN the registry cannot resolve to an
    /// ACTIVE campaign, recommender or filter.
    ///
    /// It is the INVALID-INPUT channel (`InvalidInputException`, HTTP 400), not the
    /// not-found one: the inference plane serves a resource only once it is ACTIVE and
    /// cannot distinguish "never created" from "created, not ACTIVE yet" — which is
    /// why the message states both alternatives, and why `filterArn`'s own
    /// documentation warns "When using this parameter, be sure the filter resource is
    /// ACTIVE." The ARN in the message is the one the request named.
    pub fn unresolvable(arn: &str) -> Self {
        Self::invalid(format!("{arn} does not exist or is not active yet."))
    }
```
The sibling path that DOES enforce it — `src/state.rs:599-610`:
```
    /// A campaign whose create the clock has not completed yet resolves to the SAME
    /// refusal as one that was never registered: the inference plane serves a campaign
    /// only once it is ACTIVE and cannot tell the two apart, which is what that refusal
    /// states (see [`Denied::unresolvable`] and [`Campaign::active_at`]).
    pub fn campaign(&self, arn: &str) -> Result<&Campaign, Denied> {
        require_resource_kind(arn, ResourceKind::Campaign)?;
        self.campaigns
            .get(arn)
            .filter(|campaign| campaign.is_active(SystemTime::now()))
            .ok_or_else(|| Denied::unresolvable(arn))
    }
```
The path that skips it — `src/state.rs:627-632`:
```
    pub fn filter(&self, arn: &str) -> Result<&Filter, Denied> {
        require_resource_kind(arn, ResourceKind::Filter)?;
        self.filters
            .get(arn)
            .ok_or_else(|| Denied::unresolvable(arn))
    }
```
(and `State::recommender`, `src/state.rs:616-621`, likewise.)

Nothing to check against: `Filter` holds only the ARN, the expression and the parsed form — `src/state.rs:526-535`:
```
pub struct Filter {
    /// The filter ARN this record is keyed by.
    pub arn: String,
    /// The `filterExpression` as the control plane received it.
    pub expression: String,
    /// The parsed expression the runtime applies.
    pub parsed: FilterExpression,
}
```
and `register_filter` takes no instant — `src/state.rs:1035-1039`:
```
pub fn register_filter<Eng: Send + Sync + 'static>(
    engine: &Eng,
    arn: impl Into<String>,
    expression: impl Into<String>,
) -> Result<bool, String> {
```
where the campaign seam is handed one — `src/state.rs:415-419` (`pub fn deployed(mut self, solution_version_arn: impl Into<String>, active_at: SystemTime) -> Self`), used by the handler path at `src/ops/get_recommendations.rs:327-333`:
```
        let request_filter = guard
            .bind_filter(
                registry::present(input.filter_arn.as_deref()),
                Entity::Items,
                input.filter_values.as_ref(),
            )
            .map_err(deny)?;
```
(the same call at `src/ops/get_personalized_ranking.rs:274-280` and `src/ops/get_action_recommendations.rs:248-254`, plus the promotion's filter at `src/ops/get_recommendations.rs:634-643`.)

That the "created, not ACTIVE yet" state really occurs for filters — the sibling control plane registers the runtime filter first and then stores a FUTURE activation instant: `services/service-com-amazonaws-personalize-amazonpersonalize/src/ops/create_filter.rs:249` `match runtime::register_filter(&*self.engine, &arn, &input.filter_expression) {` and `create_filter.rs:273-276`:
```
                // The filter's own creation work is this insert, so its terminal status is
                // due the instant this create answers (`crate::state::CREATE_SPAN`), and
                // never before its dataset group is due.
                active_at: due_after(now, group_active_at),
```
which `creation_status` renders as `CREATE PENDING` until it falls due (`services/service-com-amazonaws-personalize-amazonpersonalize/src/state.rs:411-421`), while the campaign seam is given its instant: `create_campaign.rs:314-316` `.deployed(&input.solution_version_arn, active_at)`.

Test coverage: `tests/all/unresolved_resource_arns.rs` covers only ARNs the registry does not hold (`registry::register_campaign(... )` at :76-98 registers everything else as ready), and `tests/all/inference.rs:473` (`an_unregistered_arn_is_invalid_input_for_every_operation`) likewise; no test exercises a REGISTERED filter whose create is not yet complete, because this crate offers no way to register one.

## Steps to Reproduce

1. Read `src/state.rs:676-687`: the registry's one refusal for an unusable resource reference is documented as the refusal for an ARN "the registry cannot resolve to an ACTIVE campaign, recommender or filter", with the message "does not exist or is not active yet".
2. Read `src/state.rs:604-610`: `State::campaign` applies `.filter(|campaign| campaign.is_active(SystemTime::now()))`, so a campaign in that state is refused.
3. Read `src/state.rs:627-632` and `src/state.rs:526-535`: `State::filter` applies no such check and `Filter` carries no activation instant, so the refusal the doc above promises for a filter can never be produced.
4. In the assembled emulator: CreateDatasetGroup -> CreateDataset(INTERACTIONS) -> CreateEventTracker + PutEvents (so `holds_interaction_records` passes) -> CreateSolution -> CreateSolutionVersion -> CreateCampaign, and wait until DescribeCampaign reports ACTIVE.
5. Call CreateFilter with `INCLUDE ItemID WHERE Items.GENRE IN ("Comedy")`, then IMMEDIATELY call DescribeFilter: it reports `CREATE PENDING` (`services/service-com-amazonaws-personalize-amazonpersonalize/src/ops/create_filter.rs:276` + `state.rs:411-421`).
6. Immediately POST `/recommendations` with that `filterArn` and the ACTIVE campaign: the call answers HTTP 200 with the filter applied (`src/ops/get_recommendations.rs:327-333` -> `bind_filter` -> `State::filter`), instead of the `InvalidInputException` 400 "<arn> does not exist or is not active yet." that the same request earns for a campaign in the identical not-yet-ACTIVE state.
