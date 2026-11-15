Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 252825d49be58514d58718d701ada3903d7ae46d

# ListEfficiencyMetrics aggregates the whole organization while its own doc claims the ListRecommendations view

_Filed: 2026-11-14T11:22:24Z_
_Operation: ListEfficiencyMetrics | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `252825d49be58514d58718d701ada3903d7ae46d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListEfficiencyMetrics` resolves its rows through `crate::state::...::readable_recommendation_view(caller)`, which widens the addressed accounts to EVERY account in the store whenever the caller is the recorded management account (`addressed_accounts(caller, &[], self.is_management_account(caller))` -> `ReadScope::WholeOrganization` -> `self.enrollments.keys()`).

The two list readers it claims to agree with do NOT use that view: `ListRecommendations` and `ListRecommendationSummaries` both go through `recommendation_view(caller, filter)`, which hard-codes `whole_organization = false`, so with no `filter.accountIds` the scope is `ReadScope::CallerOnly` — the caller's own account only, management account or not.

So for a store that records the caller as its organization's management account with member-account enrollments and catalog rows (the state `management_account_router_with_store()` builds for `account_scoped_reads` and `recommendation_read_scope_gate`), `ListEfficiencyMetrics` totals the spend and savings of the member accounts' rows into `score`, `savings` and `spend`, while an unfiltered `ListRecommendations`/`ListRecommendationSummaries` by the same caller reports none of those rows. The handler's own module doc and the comment directly above the call both state the opposite rule, so one of the two is wrong and the efficiency series is the one that reports figures no list read of the same caller accounts for. `GetRecommendation` uses the wide view deliberately (its doc explains it is an id lookup whose rows are "the union of what this caller's list reads can select"), but an AGGREGATE over that union is not the aggregate over what the list reports.

## Evidence

The claim, `src/ops/list_efficiency_metrics.rs:5-9`:
```
//! Reports cost efficiency as a TIME SERIES over the recommendation catalog the
//! calling account may read — the same view `ListRecommendations`,
//! `GetRecommendation` and `ListRecommendationSummaries` are served
//! (`crate::state::...::readable_recommendation_view`), so no reader of this
//! service disagrees with another about what the account spends or could save.
```
and again at `src/ops/list_efficiency_metrics.rs:310-319`:
```
        // The rows this caller may read, under its effective savings estimation
        // mode: the one view every recommendation reader of this service is
        // served, so these metrics are taken over exactly the recommendations
        // `ListRecommendations` reports. ...
        let view = state
            .lock()
            .readable_recommendation_view(crate::state::caller_account_id());
```

The code it breaks — `src/state.rs:1264-1267` (the wide view):
```
    pub fn readable_recommendation_view(&self, account_id: &str) -> RecommendationView {
        let addressed =
            self.addressed_accounts(account_id, &[], self.is_management_account(account_id));
        self.view_over(account_id, &addressed, None)
    }
```
versus what `ListRecommendations`/`ListRecommendationSummaries` are actually served, `src/state.rs:1329`:
```
        let addressed = self.addressed_accounts_for_read(account_id, named, false)?;
```
reached from `src/ops/list_recommendations.rs:217-219`:
```
        let view = state
            .lock()
            .recommendation_view(crate::state::caller_account_id(), input.filter.as_ref())
```
and `src/ops/list_recommendation_summaries.rs:270-272` (same call). `accounts_in` then resolves `ReadScope::WholeOrganization => self.enrollments.keys().cloned().collect()` against `ReadScope::CallerOnly => vec![caller.to_string()]` (`src/state.rs:1236-1240`).

Test coverage: `tests/all/efficiency_metrics.rs`, `efficiency_metrics_compute_optimizer_gate.rs` and `efficiency_metrics_period_refusal.rs` are the only suites for this operation and none of them uses `management_account_router*`, so the divergence is unasserted; `tests/all/one_view_for_every_recommendation_reader.rs` pins the one-view rule for the other three readers only.

## Steps to Reproduce

1. Build a store that records the caller as its organization's management account, as `tests/all/cost_optimization_hub.rs:166-171` (`management_account_router_with_store`) does, and record a member account's `Enrollment` (status `Active`, analysis due) plus catalog rows owned by that member account, as `tests/all/account_scoped_reads.rs:55` and `recommendation_read_scope_gate.rs:297` already do.
2. Call `ListRecommendations {"includeAllRecommendations": true}` (no `filter`) — `items` carries the caller's own rows only, because `recommendation_view` passes `whole_organization = false`.
3. Call `ListRecommendationSummaries {"groupBy": "AccountId"}` (no `filter`) — one group, the caller's own account.
4. Call `ListEfficiencyMetrics {"granularity": "Monthly", "timePeriod": {...}, "groupBy": "AccountId"}` — groups and `spend`/`savings` for the MEMBER accounts too, i.e. figures neither list read of the same caller reports, contradicting `list_efficiency_metrics.rs:5-9` and `:310-313`.
