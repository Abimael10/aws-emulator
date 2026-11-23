Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: ce9da1949a31b48f6e624e7aad44b9692a7b8b4d

# The boundary-token page tail is hand-rolled in six readers instead of stated once like offset_page

_Filed: 2026-11-23T20:03:38Z_
_Operation: GetEC2InstanceRecommendations | Class: maintainability_

> _Note: this issue was identified against the code base at commit `ce9da1949a31b48f6e624e7aad44b9692a7b8b4d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states each paging rule once for the offset-paged readers (`page_size`, `offset_page`), but the readers that page by a BOUNDARY identifier re-derive the same two-step rule — resolve the resume position, then hand out a token exactly when the selected sequence continues one item past the page — inline in six handlers, and already in two different spellings (`page.get(page_size)` over a re-sliced tail versus `matching.get(start.saturating_add(page_size))` over the whole sequence). One rule in six copies can drift: a future change to when a token is handed out, or to which item it names, has to be made six times. Extracting the tail into one `crate::state` helper beside `offset_page` (taking the boundary iterator, the resume value, the page size and the fingerprint) leaves every response byte-identical and is covered by the existing table-driven paging tests.

## Evidence

The single-rule intent — `services/.../src/state.rs:11-14`: "The two rules EVERY paginated reader of this service applies live here for the same reason: how large a page a request asks for ([`page_size`]) and which items that page carries, with the token that resumes the walk after it ([`offset_page`])." and `state.rs:2306` `pub fn offset_page<T>(selected: Vec<T>, offset: usize, page_size: usize, selection_fingerprint: String) -> (Vec<T>, Option<String>)`.

Copy 1 — `src/ops/get_ec2_instance_recommendations.rs:334-348`: `let start = match crate::state::resume_index(resume_at.as_deref(), resolved.iter().map(|(_, held, _)| held.as_str())) { ... }; let page = resolved.get(start..).unwrap_or_default(); let next_token = page.get(page_size).map(|(_, arn, _)| crate::state::encode_resource_page_token(&selection_fingerprint, arn));`

Copy 2 — `src/ops/get_auto_scaling_group_recommendations.rs:386-400`: same three statements over `resolved.iter().map(|(_, arn, _)| arn.as_str())`.

Copy 3 (second spelling) — `src/ops/get_ebs_volume_recommendations.rs:321-343`: `let start = match crate::state::resume_index(...)`; `let next_token = matching.get(start.saturating_add(page_size)).and_then(...encode_resource_page_token...)`; `matching.into_iter().skip(start).take(page_size)`.

Copy 4 — `src/ops/get_lambda_function_recommendations.rs:340-363`: identical to copy 3 with `recommendation.function_arn`.

Copy 5 — `src/ops/get_recommendation_preferences.rs:264-275`: `resume_index` + `selected.get(start..)` + `page.get(page_size).map(|(key, _)| crate::state::encode_page_token(...))`.

Copy 6 — `src/ops/get_ecs_service_recommendations.rs:399-426`: `resume_index` then the same "one past the page names the next page" decision written as a loop break.

Coverage that must stay green: `tests/all/shared_paging_rules.rs:3-28` — "The two paging rules EVERY paginated reader of this service shares ... asserted over a TABLE of every reader of this service whose page the request sizes ... the eleven rows below".

## Steps to Reproduce

1. Read services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/state.rs:11-14 and state.rs:2306 for the single-rule statement and the offset-paged helper
2. Compare src/ops/get_ec2_instance_recommendations.rs:334-348, get_auto_scaling_group_recommendations.rs:386-400, get_recommendation_preferences.rs:264-275 (tail re-sliced, `page.get(page_size)`)
3. Compare src/ops/get_ebs_volume_recommendations.rs:321-343 and get_lambda_function_recommendations.rs:340-363 (whole sequence, `start.saturating_add(page_size)`) and get_ecs_service_recommendations.rs:399-426 (loop form)
4. Observe one rule in six copies with two spellings, each reproducing resume_index + the one-past-the-page token decision
