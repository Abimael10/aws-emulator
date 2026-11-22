Issue-Source: trace-sweep-divergence
Issue-Trace: codestar-notifications-list-and-filter-rules
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codestar-notifications-list-and-filter-rules
Issue-Shape-Id: com.amazonaws.codestarnotifications#CodeStarNotifications_20191015
Issue-Key: codestar-notifications|ListNotificationRules|2xx->2xx|$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9"].Id
Issue-Key: codestar-notifications|ListNotificationRules|2xx->2xx|$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/e9aca2a2-a60f-4f8c-86ad-53860daea551"].Id
Issue-Commit: 9a6c9844b7b2b3cba2945c38dfe9b6ac305cdb4c

# codestar-notifications rule id minting leaves every ListNotificationRules page Id unverifiable

> _Note: this issue was identified against the code base at commit `9a6c9844b7b2b3cba2945c38dfe9b6ac305cdb4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListNotificationRules

## Description
Three replay clusters, all service=codestar-notifications / operation=ListNotificationRules / expected_status=200 -> actual_status=200, diverge on the SAME output member: NotificationRuleSummary.Id. Every one of them answers the modeled 200 with the modeled members and selects the correct rules, so no status, error code or error message is in play; what diverges is the rule IDENTITY the listing reports, for EVERY entry of EVERY page, filtered and unfiltered. This issue bundles THREE causes and a fixer must close all THREE (task#6, task#7 and task#8) before submitting.

Shared root cause. The id a listing reports is the id state minted for the rule. In this checkout crate::state::new_rule_id draws a fresh random version-4 UUID per rule (services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/src/state.rs:575-577, `::uuid::Uuid::new_v4().as_hyphenated().to_string()`), crate::state::notification_rule_arn (state.rs:368-374) embeds that same value as the ARN's `notificationrule/<id>` segment, and ListNotificationRules echoes the stored id straight into NotificationRuleSummary.Id (src/ops/list_notification_rules.rs:193-201). One minting decision therefore decides what every listing reports, and nothing in the id is derived from the rule the request describes - new_rule_id's own doc comment says so outright: the id "is not a function of anything this service knows (not the creation order, not the clock, not the request)". Note that the replay detail prints the expected and the actual id with the SAME text, so the divergence is NOT a visible textual difference in one recording's value: the comparator binds a listed entry by its Arn and still rejects the Id the emulator carries for that entry, on every replay, while the printed value changes from run to run. Hypothesis: the identity the recording holds for a rule is not reconstructible from a per-run random draw, so the listing's Id cannot be matched back to the recorded identity even when it renders alike. The quoted UUIDs below are per-run fill-ins and are NOT part of the requirement; the requirement is that a listed rule's Id be that rule's own stable identity, a bare NotificationRuleId string byte-for-byte identical to the resource segment after `notificationrule/` in that same entry's Arn.

Cause 1 of 3: unfiltered ListNotificationRules page (two rules listed) diverges at NotificationRuleSummary.Id for BOTH entries. Fixture codestar-notifications-list-and-filter-rules, step task#6.
  AWS: Smithy (services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/smithy-model/com-amazonaws-codestarnotifications-codestarnotifications-20191015.json): ListNotificationRules http POST /listNotificationRules code 200, @paginated(inputToken=NextToken, outputToken=NextToken, items=NotificationRules, pageSize=MaxResults), errors InvalidNextTokenException (400) and ValidationException (400) - neither involved here. Output ListNotificationRulesResult.NotificationRules targets NotificationRuleBatch of NotificationRuleSummary{Id -> NotificationRuleId @length min 1 max 40, doc "The unique ID of the notification rule"; Arn -> NotificationRuleArn @pattern ^arn:aws[^:\s]*:codestar-notifications:[^:\s]+:\d{12}:notificationrule\/(.*\S)?$}. The model constrains only the id's LENGTH and never its internal form or provenance, so for the form this is recorded behavior only: the recording shows AWS reporting, for each listed rule, the unique identity it issued for that rule, identical to that rule's ARN resource segment and stable for the life of the rule.
  Emulator: answers 200 with the right page, the right members and both rules correctly anchored by Arn, but replay returns verdict=mismatch with value_mismatch at $.NotificationRules[Arn="...notificationrule/072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9"].Id and $.NotificationRules[Arn="...notificationrule/e9aca2a2-a60f-4f8c-86ad-53860daea551"].Id - two of two listed rules. The id reported is whatever state::new_rule_id drew in this run.
  Why: Id is the member a client uses to correlate a listing entry with a rule it already holds, and together with Arn it is the wire identity DescribeNotificationRule, Subscribe, UpdateNotificationRule, DeleteNotificationRule and the tag calls read back. A listing whose ids cannot be matched against the identity AWS issues breaks that correlation for every rule in the account, on every page, for any client that persisted, logged or asserted an id recorded from AWS - even though the call looks successful.
  Fix: the id minting in crate::state::new_rule_id, and through it crate::state::notification_rule_arn - NOT the ListNotificationRules handler, which already returns the stored id unchanged, and not the filter/pagination path, which selects the right rules. Whatever the mint becomes, a summary's Id must stay byte-for-byte identical to the resource segment of that summary's Arn.
  Traces: codestar-notifications-list-and-filter-rules#task#6.

Cause 2 of 3: the same Id divergence on a FILTERED page returning a single rule. Fixture codestar-notifications-list-and-filter-rules, step task#7.
  AWS: as cause 1 - Smithy: NotificationRuleSummary.Id -> NotificationRuleId @length 1..40, no @pattern; HTTP 200. Filtering (ListNotificationRulesRequest.Filters, OR within one filter name and AND across names, as the member documents) narrows WHICH rules are listed and changes nothing about the identity reported for a listed rule; the id's form is recorded behavior only.
  Emulator: 200, the filter selects the correct single rule (its Arn anchors), and that one entry still reports value_mismatch at $.NotificationRules[Arn="...notificationrule/e9aca2a2-a60f-4f8c-86ad-53860daea551"].Id, with expected and actual rendering alike - the same non-textual divergence as cause 1.
  Why: a filtered listing is how a client finds one rule by resource, event type, target address or creator; the single entry it gets back carries an identity it cannot match against the rule it was looking for.
  Fix: same component as cause 1 (crate::state::new_rule_id). Nothing in rule_satisfies (src/ops/list_notification_rules.rs:86-98) or crate::state::grouped_filters is implicated - selection is correct.
  Traces: codestar-notifications-list-and-filter-rules#task#7.

Cause 3 of 3: the same Id divergence on the OTHER filtered page. Fixture codestar-notifications-list-and-filter-rules, step task#8.
  AWS: as cause 1/2 - NotificationRuleId @length 1..40 and nothing else in the model; HTTP 200, no modeled error.
  Emulator: 200, correct single rule selected, value_mismatch at $.NotificationRules[Arn="...notificationrule/072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9"].Id.
  Why: as cause 2 - the fault is not specific to one rule or one filter; every listing this service serves reports an identity the recording does not accept.
  Fix: as cause 1/2.
  Traces: codestar-notifications-list-and-filter-rules#task#8.

Open issues that do NOT own these three clusters' replay keys (this replay reports owned_by EMPTY for all three, so none of them gates these steps and a fixer closing them would not re-run these keys): 20261121T090342Z-codestar-notifications-listnotificationrules-reports-a-notificationrulesummary-id-the.md, 20261120T023944Z-codestar-notifications-mints-notification-rule-ids-with-no.md and 20261118T184204Z-codestar-notifications-random-per-run-new-rule-id.md all describe this same minting root cause, but their Issue-Key / Issue-Trace-Ids lines either name per-run ARNs from earlier replays (notificationrule/3dcb3abd-..., /88e0d6e4-..., /06422eef-..., /33d90459-...) or list the fixture id with NO #step, so ownership can never match while the mint stays random - itself a consequence of this defect. 20261110T174500Z-codestar-notifications-rule-identifiers-diverge-createnotificationrule-arn-shape.md OWNS the fourth cluster of this replay (CreateNotificationRule $.Arn, codestar-notifications-rule-lifecycle#task#4), which is reported owned_by that issue and is therefore skipped here; it also describes new_rule_id as a sha-256 of "notificationrule/<sequence>/<nanos>", while the code has since drifted to a random uuid::Uuid::new_v4 draw (state.rs:575-577) with the divergence unchanged. The remaining open codestar-notifications issue (TagResource reserved-tag-key wording) does not touch listing identities.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: trace_set_id=simparity-wire-aws-v1, fixtures_replayed=11, steps_total=95, steps_passed=91, steps_mismatched=4, steps_errored=0, steps_blocked=0, divergence_count=4, score_pct=95.8, emulator_up=true ("/__admin/health answered 200 and the :41893 data listener accepts"), bootstrap_signal=false, not_implemented=0.

All three clusters filed here: service=codestar-notifications, operation=ListNotificationRules, verdict=mismatch, expected_status=200, actual_status=200, stub_path empty, owned_by empty, affected_count=1. The fourth cluster (CreateNotificationRule $.Arn, codestar-notifications-rule-lifecycle#task#4, detail "$.Arn: value_mismatch (expected '<shaped-not-asserted>', got 'arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/128f2cad-0b53-4cc0-8343-21e97a4edfaf')") carries owned_by=[20261110T174500Z-codestar-notifications-rule-identifiers-diverge-createnotificationrule-arn-shape.md] and is NOT re-filed.

Cause 1 - fixture codestar-notifications-list-and-filter-rules, step task#6, affected=[codestar-notifications-list-and-filter-rules#task#6]:
  divergence_key: service=codestar-notifications|ListNotificationRules|status=200->200|keys=$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9"].Id,$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/e9aca2a2-a60f-4f8c-86ad-53860daea551"].Id
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9"].Id: value_mismatch (expected '072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9', got '072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9')
          $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/e9aca2a2-a60f-4f8c-86ad-53860daea551"].Id: value_mismatch (expected 'e9aca2a2-a60f-4f8c-86ad-53860daea551', got 'e9aca2a2-a60f-4f8c-86ad-53860daea551')
  Expected: for each listed rule, a NotificationRuleSummary.Id that is the rule's stable recorded identity - a bare NotificationRuleId string (1..40 chars) identical to the resource segment after `notificationrule/` in that same entry's Arn.
  Actual: 200 with both rules present and correctly anchored by Arn, but the Id of EVERY entry on the page fails the comparison (two of two), with expected and actual rendering identically. The UUIDs above are per-run minted values; earlier replays of these same steps carried 3dcb3abd-.../88e0d6e4-... and 06422eef-.../33d90459-... while the verdict was identical, so the verdict - not the value - is what reproduces.

Cause 2 - same fixture, step task#7 (filtered listing, one entry), affected=[codestar-notifications-list-and-filter-rules#task#7]:
  divergence_key: service=codestar-notifications|ListNotificationRules|status=200->200|keys=$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/e9aca2a2-a60f-4f8c-86ad-53860daea551"].Id
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/e9aca2a2-a60f-4f8c-86ad-53860daea551"].Id: value_mismatch (expected 'e9aca2a2-a60f-4f8c-86ad-53860daea551', got 'e9aca2a2-a60f-4f8c-86ad-53860daea551')
  Expected/Actual: as cause 1, for the single rule the filter selects.

Cause 3 - same fixture, step task#8 (the other filtered listing, one entry), affected=[codestar-notifications-list-and-filter-rules#task#8]:
  divergence_key: service=codestar-notifications|ListNotificationRules|status=200->200|keys=$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9"].Id
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9"].Id: value_mismatch (expected '072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9', got '072a8ba9-bf06-4c65-98d6-ca95e3b9ffa9')
  Expected/Actual: as cause 1, for the single rule the filter selects.

No status, error code or error message diverges in any of the three causes: all answered the modeled 200 for POST /listNotificationRules and no modeled error (InvalidNextTokenException, ValidationException) was involved.

Code under the causes (same checkout the emulator is built from):
  services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/src/state.rs:562-577 - new_rule_id(): `::uuid::Uuid::new_v4().as_hyphenated().to_string()`, documented as OPAQUE and "not a function of anything this service knows (not the creation order, not the clock, not the request)".
  .../src/state.rs:368-374 - notification_rule_arn(region, id) -> emulator_arn("codestar-notifications", region, "notificationrule/<id>"), i.e. the ARN carries the same minted value.
  .../src/ops/list_notification_rules.rs:182-217 - rules_in_region_order -> filter -> NotificationRuleSummary { id: Some(rule.id.clone()), arn: Some(rule.arn.clone()) }, then page truncation and encode_page_token; the listing is faithful to the stored id.
  .../src/ops/list_notification_rules.rs:86-98 - rule_satisfies (CREATED_BY / EVENT_TYPE_ID / RESOURCE / TARGET_ADDRESS); selection is correct in all three steps.

Smithy model consulted: ListNotificationRules http POST /listNotificationRules code 200, @paginated(inputToken=NextToken, outputToken=NextToken, items=NotificationRules, pageSize=MaxResults), errors InvalidNextTokenException 400 / ValidationException 400; NotificationRuleSummary.Id -> NotificationRuleId @length min 1 max 40 (no @pattern, so the id's internal form is recorded behavior only), NotificationRuleSummary.Arn -> NotificationRuleArn @pattern ^arn:aws[^:\s]*:codestar-notifications:[^:\s]+:\d{12}:notificationrule\/(.*\S)?$.

## Steps to Reproduce
1. Clone the repository and boot the emulator built from it (the router that mounts service-com-amazonaws-codestarnotifications-codestarnotifications-20191015).
2. Replay trace set simparity-wire-aws-v1 against it (replay_wire_corpus). Summary: 11 fixtures, 95 steps, 91 passed, 4 mismatched in 4 clusters, score 95.8%, bootstrap_signal=false.
3. Cause 1: replay fixture codestar-notifications-list-and-filter-rules through step task#6 - the unfiltered ListNotificationRules (POST /listNotificationRules with no Filters) after the fixture's CreateNotificationRule steps. The response is 200 and lists both rules, but the step diverges at $.NotificationRules[Arn="..."].Id for BOTH entries (verdict=mismatch, expected_status=200, actual_status=200).
4. Cause 2: continue the same fixture to step task#7 - a filtered ListNotificationRules returning a single rule. 200, the correct rule is selected, and that one entry's $.NotificationRules[Arn="..."].Id diverges.
5. Cause 3: continue to step task#8 - the other filtered ListNotificationRules. Same 200, same single-entry Id divergence for the other rule.
6. Confirm the provenance of the identity, not one recorded value, is at fault: replay the corpus twice. The ids quoted in the details change between runs (state::new_rule_id draws a fresh uuid::Uuid::new_v4 per rule) while all three clusters diverge identically every time, and the detail prints expected and actual with the same text.
7. After a fix, re-replay codestar-notifications-list-and-filter-rules#task#6, #task#7 and #task#8; all three must pass, and each summary's Id must be a bare NotificationRuleId string byte-for-byte identical to the resource segment of that entry's Arn and stable across calls for the same rule.
