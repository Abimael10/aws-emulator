Issue-Source: trace-sweep-divergence
Issue-Trace: codestar-notifications-list-and-filter-rules
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codestar-notifications-list-and-filter-rules
Issue-Shape-Id: com.amazonaws.codestarnotifications#CodeStarNotifications_20191015
Issue-Key: codestar-notifications|ListNotificationRules|2xx->2xx|$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/c28a72ec-4151-4ee5-9be3-8dfb9633f4f5"].Id
Issue-Key: codestar-notifications|ListNotificationRules|2xx->2xx|$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/f90f30a1-2f96-4e0a-8b05-2fcbe4187947"].Id
Issue-Commit: 7bfda40add59a57de4c47a21257187f7eb42eb8e

# codestar-notifications ListNotificationRules Id diverges on every page after the id-minting rewrite

## Resolution (all three causes declined: a per-run generated resource id)

The emulator's behaviour is correct and all THREE causes are DECLINED, not implemented.
They are one difference on one member: `NotificationRuleSummary.Id` carries the id the
service drew for that rule in THIS run, while the recording holds the id AWS drew in the
recording run. `crate::state::new_rule_id` already draws a canonical hyphenated lowercase
version-4 UUID (inside `NotificationRuleId`'s `@length` 1..40 — the only constraint the
model places on it), `crate::state::notification_rule_arn` embeds that exact value as the
ARN's `notificationrule/<id>` segment, and `src/ops/list_notification_rules.rs` reports the
stored id verbatim beside the stored ARN on every page, filtered or not. The two values
therefore differ only in their draw: same modelled type, same length, same form, same
provenance.

Do NOT implement the remedy the report implies (serve the recording's uuid, or derive the
id from the request): a rule id is minted once per rule and recurs in no later run, and an
id taken from the recording would also have to differ from the resource segment of the ARN
the emulator minted — which this very report requires to stay byte-for-byte identical. The
replay keys `codestar-notifications|ListNotificationRules|2xx->2xx|$.NotificationRules[Arn=
"...notificationrule/<uuid>"].Id` therefore stay failing by construction, and each new
replay reports them under that run's own uuids.

What the resolution added instead is the test that pins the rule:
`tests/all/rule_id_correlation.rs` asserts, over a table of rules spanning region, resource
type, detail type, event-type count and target count, that the identifier is DRAWN (two
independent stores answering the same create sequence share no id, and no id is a value its
own request carried), that the `Id` a listing reports — at the modelled minimum page size
and with no `MaxResults` — rebuilds exactly that rule's ARN and that the rebuilt ARN
addresses the rule through describe, subscribe, tag, list-tags, update and delete, and that
an id names a rule only in its own region. `tests/all/rule_id_identity.rs` keeps the id
form and the id's stability across reads, updates and re-creations.

> _Note: this issue was identified against the code base at commit `7bfda40add59a57de4c47a21257187f7eb42eb8e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListNotificationRules

## Description
Three replay clusters of this run, all service=codestar-notifications / operation=ListNotificationRules / expected_status=200 -> actual_status=200, verdict=mismatch, stub_path empty, owned_by EMPTY, diverge on the same member: NotificationRuleSummary.Id. Every step answers the modeled 200 with the modeled members and selects the right rules; what the comparator rejects is the rule identity the listing reports, for EVERY entry of EVERY page, filtered or not. A fixer must close all THREE causes below before submitting.

What is NEW in this replay, and why this is filed rather than left to the open issues named at the end: the minting the earlier issues blamed has ALREADY been rewritten in this checkout and the divergence is unchanged. crate::state::new_rule_id is now `::uuid::Uuid::new_v4().as_hyphenated().to_string()` (services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/src/state.rs:562-577) — a genuinely random, canonical lowercase hyphenated 36-character v4 UUID, no longer the sha-256-of-sequence-and-wall-clock layout those issues describe at state.rs:569 — and all three clusters still report value_mismatch. So "derive the id from a digest" vs "draw it at random" is not the discriminator: a canonical hyphenated v4 UUID in the id slot is itself not the identity form the recording accepts for a notification rule. crate::state::notification_rule_arn (state.rs:366-374) embeds that same value as the ARN's `notificationrule/<id>` segment, crate::ops::create_notification_rule (create_notification_rule.rs:329-331) stores it on the rule, and ListNotificationRules echoes the stored value into NotificationRuleSummary.Id (src/ops/list_notification_rules.rs:193-201) — so one minting decision decides what every listing reports, and the listing and filter paths are faithful.

Hypothesis (neither the model nor the recording states this): the replay's rendered expected and actual read with the SAME text because the comparator binds a listed entry by its Arn and the emulator's Id is byte-identical to its own ARN's resource segment; what it rejects is the identity itself, i.e. the id the emulator minted cannot be reconciled with the rule identity the recording holds for that rule, so no visible character differs in the rendering while the comparison still fails. Treat the discriminator as the id's FORM/provenance, not any one recorded uuid: the reported values change every run while the verdict is identical every run.

Cause 1 of 3: unfiltered ListNotificationRules page (two rules listed) diverges at NotificationRuleSummary.Id for BOTH entries. Fixture codestar-notifications-list-and-filter-rules, step task#6.
  AWS: Smithy: ListNotificationRules — http POST /listNotificationRules, code 200, @paginated(inputToken=NextToken, outputToken=NextToken, items=NotificationRules, pageSize=MaxResults); output ListNotificationRulesResult.NotificationRules -> NotificationRuleBatch of NotificationRuleSummary{Id -> NotificationRuleId @length min 1 max 40, doc "The unique ID of the notification rule"; Arn -> NotificationRuleArn @pattern ^arn:aws[^:\s]*:codestar-notifications:[^:\s]+:\d{12}:notificationrule\/(.*\S)?$}; modeled errors InvalidNextTokenException (400) and ValidationException (400), neither involved. The model constrains only the id's LENGTH and never its internal form, so for the form this is recorded behavior only: the recording reports, for each listed rule, the unique rule id AWS minted for that rule, identical to the id segment of that rule's ARN. No error code and no error message is involved on either side — both sides are 200.
  Emulator: 200 with the right page and the right members, but value_mismatch at $.NotificationRules[Arn="...notificationrule/f90f30a1-2f96-4e0a-8b05-2fcbe4187947"].Id and $.NotificationRules[Arn="...notificationrule/c28a72ec-4151-4ee5-9be3-8dfb9633f4f5"].Id — two of two listed rules. The id it reports is the canonical hyphenated v4 UUID state::new_rule_id drew for that rule in this run.
  Why: Id is the member a client uses to correlate a listing entry with a rule it already knows, and (Id, Arn) is the rule's wire identity — the key DescribeNotificationRule, Subscribe, UpdateNotificationRule, DeleteNotificationRule and the tag calls read back. A listing whose ids cannot be matched against the ids AWS issues breaks that correlation for every rule in the account, on every page.
  Fix: the rule-id minting in crate::state (new_rule_id, and through it notification_rule_arn) — NOT the ListNotificationRules handler, which already returns the stored id unchanged, and not the filter path. Whatever the mint becomes, a summary's Id must stay byte-for-byte identical to the resource segment of that summary's Arn.
  Traces: codestar-notifications-list-and-filter-rules#task#6.

Cause 2 of 3: the same Id divergence on a FILTERED page returning a single rule. Fixture codestar-notifications-list-and-filter-rules, step task#7.
  AWS: as cause 1 — Smithy: NotificationRuleSummary.Id -> NotificationRuleId @length 1..40, HTTP 200. Filtering (Filters: OR within one filter name, AND across names, per the member's own documentation) narrows WHICH rules are listed and changes nothing about the identity reported for a listed rule.
  Emulator: 200, the filter selects the correct rule (its Arn is matched by the comparator's selector), and the single entry's $.NotificationRules[Arn="...notificationrule/c28a72ec-4151-4ee5-9be3-8dfb9633f4f5"].Id still reports value_mismatch.
  Why: a filtered listing is how a client finds one rule by resource, event type, target address or creator; the one entry it gets back carries an id it cannot match against AWS's.
  Fix: same component as cause 1 (crate::state::new_rule_id). Nothing in the filter path (rule_satisfies / grouped_filters, src/ops/list_notification_rules.rs:86-98 and 145-154) is implicated — the right rule is selected.
  Traces: codestar-notifications-list-and-filter-rules#task#7.

Cause 3 of 3: the same Id divergence on the OTHER filtered page. Fixture codestar-notifications-list-and-filter-rules, step task#8.
  AWS: as cause 1/2 — recorded behavior only for the id's form; the model gives NotificationRuleId @length 1..40 and nothing else. HTTP 200, no error.
  Emulator: 200, correct rule selected, $.NotificationRules[Arn="...notificationrule/f90f30a1-2f96-4e0a-8b05-2fcbe4187947"].Id diverges (value_mismatch) for the single entry.
  Why: as cause 2 — every listing this service serves, filtered or unfiltered, reports a rule id a client cannot match.
  Fix: as cause 1/2.
  Traces: codestar-notifications-list-and-filter-rules#task#8.

Relation to the open issues, none of which the replay reports as owning these clusters (owned_by is empty for all three): 20261110T174500Z-codestar-notifications-rule-identifiers-diverge-createnotificationrule-arn-shape.md (owns the fourth cluster of this run, CreateNotificationRule $.Arn at codestar-notifications-rule-lifecycle#task#4, which is therefore NOT filed here), 20261110T182312Z-codestar-notifications-listnotificationrules-reports-unmatched-rule-id-on.md and 20261110T210013Z-codestar-notifications-listnotificationrules-reports-unmatchable-notificationrulesummary-id-on.md. All three describe the same member on the same fixture steps, but (a) every one of their Issue-Key lines embeds a per-run random rule ARN from an earlier replay (notificationrule/2dcfbdba-..., /7b85c7a4-..., /4f2b7532-..., /ad4ff815-..., /ab795b6f-..., /bb4dd50e-...), a value that never recurs, which is why the ownership join fails on every new replay; (b) their Issue-Trace-Ids carries only the bare fixture id with no `#step` half, so task#7 and task#8 are not bound as replayable units in their pointers; and (c) their shared diagnosis (sha-256-derived id) no longer matches this checkout, where new_rule_id draws a real v4 UUID and the divergence persists. This issue binds task#6 as its representative step and task#7 / task#8 as well-formed fixture#step tokens, and records that the already-applied minting rewrite did not close it. If a triager prefers single ownership, close this against whichever of the three is kept — but the kept one must carry all three fixture#step bindings, or task#7 and task#8 go unreplayed.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: trace_set_id=simparity-wire-aws-v1, emulator_up=true (/__admin/health answered 200 and the data listener accepts), bootstrap_signal=false, fixtures_replayed=11, steps_total=95, steps_passed=87, steps_mismatched=8, steps_blocked=0, steps_errored=0, divergence_count=4, score_pct=91.6.

Four clusters; the three filed here are service=codestar-notifications, operation=ListNotificationRules, verdict=mismatch, expected_status=200, actual_status=200, stub_path empty, owned_by empty, affected_count=1 each. The fourth (CreateNotificationRule $.Arn, codestar-notifications-rule-lifecycle#task#4, detail: $.Arn: value_mismatch (expected '<shaped-not-asserted>', got 'arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/53ffef12-2888-4286-a2be-97344a7a68c3')) reports owned_by 20261110T174500Z-...-createnotificationrule-arn-shape.md and is skipped.

Cause 1 — fixture codestar-notifications-list-and-filter-rules, step task#6 (affected: codestar-notifications-list-and-filter-rules#task#6):
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/f90f30a1-2f96-4e0a-8b05-2fcbe4187947"].Id: value_mismatch (expected 'f90f30a1-2f96-4e0a-8b05-2fcbe4187947', got 'f90f30a1-2f96-4e0a-8b05-2fcbe4187947')
           $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/c28a72ec-4151-4ee5-9be3-8dfb9633f4f5"].Id: value_mismatch (expected 'c28a72ec-4151-4ee5-9be3-8dfb9633f4f5', got 'c28a72ec-4151-4ee5-9be3-8dfb9633f4f5')
  divergence_key: service=codestar-notifications|ListNotificationRules|status=200->200|keys=$.NotificationRules[...c28a72ec...].Id,$.NotificationRules[...f90f30a1...].Id
  Expected: each summary's Id equal to the rule identity the recording holds for that rule — the id segment of that rule's ARN, in the id form AWS mints.
  Actual: the per-run canonical hyphenated v4 UUID state::new_rule_id drew; the comparison fails for every entry of the page. The rendered expected and actual read alike because the emulator's Id is byte-identical to its own ARN segment, by which the comparator bound the entry; the uuids above are per-run fill-ins and are NOT the requirement — earlier replays of these same steps carried 2dcfbdba/7b85c7a4, 4f2b7532/ad4ff815 and ab795b6f/bb4dd50e with an identical verdict.

Cause 2 — same fixture, step task#7 (affected: codestar-notifications-list-and-filter-rules#task#7):
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/c28a72ec-4151-4ee5-9be3-8dfb9633f4f5"].Id: value_mismatch (expected 'c28a72ec-4151-4ee5-9be3-8dfb9633f4f5', got 'c28a72ec-4151-4ee5-9be3-8dfb9633f4f5') — single filtered entry.

Cause 3 — same fixture, step task#8 (affected: codestar-notifications-list-and-filter-rules#task#8):
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/f90f30a1-2f96-4e0a-8b05-2fcbe4187947"].Id: value_mismatch (expected 'f90f30a1-2f96-4e0a-8b05-2fcbe4187947', got 'f90f30a1-2f96-4e0a-8b05-2fcbe4187947') — single filtered entry.

Code under the causes, at its CURRENT lines in this checkout (note the drift from the older issues' citations):
  services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/src/state.rs:562-577 — new_rule_id(): `::uuid::Uuid::new_v4().as_hyphenated().to_string()`; the doc comment states the id is a randomly drawn v4 UUID in canonical hyphenated lowercase form, 36 characters, inside the modeled 1..40. This is NOT the sha-256 layout the open issues describe — the rewrite is already in, and the divergence is unchanged.
  .../src/state.rs:366-374 — notification_rule_arn(region, id) = emulator_arn("codestar-notifications", region, "notificationrule/<id>").
  .../src/ops/create_notification_rule.rs:329-331 — take_sequence() -> st::new_rule_id() -> st::notification_rule_arn(), stored on the rule and returned as the create output's Arn.
  .../src/ops/list_notification_rules.rs:193-201 — NotificationRuleSummary { id: Some(rule.id.clone()), arn: Some(rule.arn.clone()) } — the listing is faithful to the stored id, so it is not the component at fault.

Smithy model consulted (services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/smithy-model/com-amazonaws-codestarnotifications-codestarnotifications-20191015.json): ListNotificationRules http POST /listNotificationRules code 200, @paginated(inputToken=NextToken, outputToken=NextToken, items=NotificationRules, pageSize=MaxResults), errors InvalidNextTokenException (400) / ValidationException (400); NotificationRuleSummary.Id -> NotificationRuleId @length min 1 max 40, no @pattern (so the id's internal form is recorded behavior only); NotificationRuleSummary.Arn -> NotificationRuleArn @pattern ^arn:aws[^:\s]*:codestar-notifications:[^:\s]+:\d{12}:notificationrule\/(.*\S)?$.

Bootstrap gate: codestar-notifications is stood up and serving (87 of 95 steps pass, these steps answer the modeled 200 with the modeled body), so this is a fidelity finding, not a bootstrap gap.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from this checkout (cargo run -p aws-emulator-router).
2. Replay trace set simparity-wire-aws-v1 against it (replay_wire_corpus).
3. Cause 1: replay fixture codestar-notifications-list-and-filter-rules through step task#6 — an unfiltered ListNotificationRules (POST /listNotificationRules) after the fixture's rules have been created. The response is 200 with both rules present and correctly shaped, but $.NotificationRules[Arn="..."].Id reports value_mismatch for BOTH entries.
4. Cause 2: continue to step task#7 — a filtered ListNotificationRules returning one rule. 200, the correct rule is selected, and that single entry's $.NotificationRules[Arn="..."].Id diverges.
5. Cause 3: continue to step task#8 — the other filtered listing. Same 200, same single-entry Id divergence.
6. Confirm the form, not one recorded value, is at fault: replay the corpus twice. The ids reported change between runs (state::new_rule_id draws a fresh v4 UUID per rule) while all three clusters diverge identically every time, and the rendered expected and actual always read alike because the emulator's Id equals its own ARN's resource segment.
7. Confirm the earlier diagnosis is spent: read services/.../src/state.rs:562-577 — new_rule_id already draws uuid::Uuid::new_v4() instead of the sha-256-of-sequence-and-clock layout the open rule-identifier issues cite, and the three clusters still mismatch. A fix must change the identity FORM the service mints, not merely its randomness source.
8. After a fix, re-replay codestar-notifications-list-and-filter-rules#task#6, #task#7 and #task#8; all three must pass, and each summary's Id must remain byte-for-byte identical to the resource segment of that summary's Arn.
