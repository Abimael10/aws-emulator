Issue-Source: trace-sweep-divergence
Issue-Trace: codestar-notifications-list-and-filter-rules
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codestar-notifications-list-and-filter-rules
Issue-Shape-Id: com.amazonaws.codestarnotifications#CodeStarNotifications_20191015
Issue-Key: codestar-notifications|ListNotificationRules|2xx->2xx|$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/06422eef-ecf6-489a-89c7-b385f488ef07"].Id
Issue-Key: codestar-notifications|ListNotificationRules|2xx->2xx|$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/33d90459-1da9-43a8-bc4c-7b92b14ae412"].Id
Issue-Commit: ef271dbe26f379074e11bf8bd2be74a0451dc7db

# codestar-notifications ListNotificationRules reports a NotificationRuleSummary.Id the recorded listing does not accept

> _Note: this issue was identified against the code base at commit `ef271dbe26f379074e11bf8bd2be74a0451dc7db`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListNotificationRules

## Description
Three replay clusters in codestar-notifications share ONE fault: the `Id` this service reports for a rule in a ListNotificationRules page is not the identity the recorded step accepts for that rule. Every step answers the modeled HTTP 200 with the modeled members and selects the right rules (the entry is anchored by its `Arn`, and the anchor matches), so nothing about status, error code or error message is involved: the divergence is purely in the reported rule identity. It shows up on the unfiltered page (both listed rules) and on each filtered page (the single listed rule), i.e. on every listing this service serves. A fixer must close all THREE causes below before submitting.

Shared root cause. A rule's identity is minted once, in `crate::state::new_rule_id` (services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/src/state.rs:575), which draws a fresh random version-4 UUID per call; `crate::state::notification_rule_arn` (state.rs:368) embeds that value as the ARN's `notificationrule/<id>` segment, and ListNotificationRules projects the stored value straight into `NotificationRuleSummary.Id` (src/ops/list_notification_rules.rs:193-201). So one minting decision decides both the ARN segment and every `Id` reported in a listing, and all three clusters move together.

Cause 1 of 3: unfiltered ListNotificationRules — BOTH listed rules diverge at their `Id`. Fixture codestar-notifications-list-and-filter-rules, step task#6.
  AWS: Smithy (services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/smithy-model/com-amazonaws-codestarnotifications-codestarnotifications-20191015.json): ListNotificationRules is POST /listNotificationRules, HTTP 200, @paginated(items=NotificationRules), output NotificationRules -> NotificationRuleBatch of NotificationRuleSummary{Id -> NotificationRuleId @length min 1 max 40, documented "The unique ID of the notification rule"; Arn -> NotificationRuleArn @pattern ^arn:aws[^:\s]*:codestar-notifications:[^:\s]+:\d{12}:notificationrule\/(.*\S)?$}. Errors modeled: InvalidNextTokenException (400), ValidationException (400) — none of them is in play here. The model constrains only the id's LENGTH, so the identity a listing must report for a rule is recorded behavior only: the recording shows each summary's `Id` being exactly the identity AWS issued for that rule and carries in that rule's ARN resource segment, stable for the life of the rule.
  Emulator: answers 200 with the right members and the right rules, but replay returns verdict=mismatch at `$.NotificationRules[Arn="..."].Id` for BOTH entries of the page. Detail, verbatim from the replay: `$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/06422eef-ecf6-489a-89c7-b385f488ef07"].Id: value_mismatch (expected '06422eef-ecf6-489a-89c7-b385f488ef07', got '06422eef-ecf6-489a-89c7-b385f488ef07')` and `$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/33d90459-1da9-43a8-bc4c-7b92b14ae412"].Id: value_mismatch (expected '33d90459-1da9-43a8-bc4c-7b92b14ae412', got '33d90459-1da9-43a8-bc4c-7b92b14ae412')`. Note that the two sides RENDER identically: the diverging aspect is therefore not the printed characters of the member but how the value is carried for that anchored entry. Hypothesis: the member the emulator emits for the anchored entry is not the plain `NotificationRuleId` string the recorded summary carries (a differing serialized form/representation, or an id that is only incidentally equal to the anchor ARN's segment in this run because it was minted in the same run), which is why the comparison fails while the two renderings agree. The quoted UUIDs are per-run minted values and are NOT a requirement — the requirement is that a listing's `Id` be the rule's own, stable identity, reported as a bare `NotificationRuleId` string identical to the resource segment of that same entry's `Arn`.
  Why: `Id` is the member a client uses to correlate a listing entry with a rule it already holds (and the value consoles, scripts and inventory tooling key on). A listing whose `Id` cannot be matched against the identity AWS reports for the same rule breaks that correlation for every rule in the account, on every page, even though the call looks successful.
  Fix: the ListNotificationRules response projection (src/ops/list_notification_rules.rs, where the stored rule is turned into a NotificationRuleSummary) together with the identity minting it reads from in crate::state (state.rs `new_rule_id`/`notification_rule_arn`); not the filter or pagination path, which selects correctly.
  Traces: codestar-notifications-list-and-filter-rules#task#6.

Cause 2 of 3: the same `Id` divergence on a FILTERED page returning one rule. Fixture codestar-notifications-list-and-filter-rules, step task#7.
  AWS: as cause 1 — Smithy: NotificationRuleSummary.Id -> NotificationRuleId @length 1..40, ListNotificationRules HTTP 200. Filtering (Filters: OR within one filter name, AND across names) narrows WHICH rules are listed and changes nothing about the identity reported for a listed rule; recorded behavior only for the id's form.
  Emulator: 200, the filter selects the correct single rule (its ARN anchors), and replay reports `$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/33d90459-1da9-43a8-bc4c-7b92b14ae412"].Id: value_mismatch (expected '33d90459-1da9-43a8-bc4c-7b92b14ae412', got '33d90459-1da9-43a8-bc4c-7b92b14ae412')` — again both sides rendering alike, so the same non-character divergence as cause 1.
  Why: a filtered listing is how a client finds one rule by resource, event type, target address or creator; the one entry it gets back carries an identity it cannot match against the rule it was looking for.
  Fix: same component as cause 1; nothing in `rule_satisfies` / `grouped_filters` (src/ops/list_notification_rules.rs:86-98 and crate::state) is implicated — the right rule is selected.
  Traces: codestar-notifications-list-and-filter-rules#task#7.

Cause 3 of 3: the same `Id` divergence on the other filtered page. Fixture codestar-notifications-list-and-filter-rules, step task#8.
  AWS: as cause 1/2.
  Emulator: 200, correct single rule selected, replay reports `$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/06422eef-ecf6-489a-89c7-b385f488ef07"].Id: value_mismatch (expected '06422eef-ecf6-489a-89c7-b385f488ef07', got '06422eef-ecf6-489a-89c7-b385f488ef07')`.
  Why: as cause 2 — the defect is not specific to one rule or one filter; every rule this service lists is reported with an identity the recording does not accept.
  Fix: as cause 1/2.
  Traces: codestar-notifications-list-and-filter-rules#task#8.

Relation to the open issues. Open issue 20261110T174500Z-codestar-notifications-rule-identifiers-diverge-createnotificationrule-arn-shape.md covers the CreateNotificationRule `$.Arn` cluster (codestar-notifications-rule-lifecycle#task#4, which this replay still reports and which that issue OWNS, so it is not re-filed here) and discusses these three listing steps in prose — but its `Issue-Trace-Ids` list bare fixture ids (`codestar-notifications-rule-lifecycle codestar-notifications-list-and-filter-rules`) with NO `#step`, so its replay pointer never expands to task#6/#7/#8 and the current replay reports all three with an EMPTY owned_by. They are filed here with proper `fixture#step` bindings so a fixer actually re-replays them. The other open codestar-notifications issues (CreateNotificationRule EventTypeIds catalog check; ListTagsForResource) do not touch listing identities.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 11 fixtures replayed, 95 steps total, 91 passed, 4 mismatched in 4 clusters, 0 errored, 0 blocked, score 95.8%, emulator_up=true (/__admin/health answered 200 and the :41211 data listener accepts), bootstrap_signal=false, not_implemented=0. All four clusters are service=codestar-notifications, verdict=mismatch, expected_status=200 / actual_status=200, stub_path empty. The fourth (CreateNotificationRule $.Arn, codestar-notifications-rule-lifecycle#task#4) carries owned_by=[20261110T174500Z-codestar-notifications-rule-identifiers-diverge-createnotificationrule-arn-shape.md] and is NOT re-filed; the three below carry owned_by=[].

Cause 1 — fixture codestar-notifications-list-and-filter-rules, step task#6, operation ListNotificationRules, affected=[codestar-notifications-list-and-filter-rules#task#6], affected_count=1:
  divergence_key: service=codestar-notifications|ListNotificationRules|status=200->200|keys=$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/06422eef-ecf6-489a-89c7-b385f488ef07"].Id,$.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/33d90459-1da9-43a8-bc4c-7b92b14ae412"].Id
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/06422eef-ecf6-489a-89c7-b385f488ef07"].Id: value_mismatch (expected '06422eef-ecf6-489a-89c7-b385f488ef07', got '06422eef-ecf6-489a-89c7-b385f488ef07')
          $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/33d90459-1da9-43a8-bc4c-7b92b14ae412"].Id: value_mismatch (expected '33d90459-1da9-43a8-bc4c-7b92b14ae412', got '33d90459-1da9-43a8-bc4c-7b92b14ae412')
  Expected: for each listed rule, a `NotificationRuleSummary.Id` that is the rule's stable identity as the recording holds it — a bare NotificationRuleId string (1..40 chars) identical to the resource segment after `notificationrule/` in that same entry's `Arn`.
  Actual: 200 with both rules present and correctly anchored by Arn, but the `Id` of EVERY entry on the page fails the comparison (two of two). The quoted UUIDs are per-run minted values; the verdict, not the value, is what reproduces on every replay.

Cause 2 — same fixture, step task#7 (filtered listing, one entry), affected=[codestar-notifications-list-and-filter-rules#task#7]:
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/33d90459-1da9-43a8-bc4c-7b92b14ae412"].Id: value_mismatch (expected '33d90459-1da9-43a8-bc4c-7b92b14ae412', got '33d90459-1da9-43a8-bc4c-7b92b14ae412')
  Expected/Actual: as cause 1, for the single rule the filter selects.

Cause 3 — same fixture, step task#8 (the other filtered listing, one entry), affected=[codestar-notifications-list-and-filter-rules#task#8]:
  detail: $.NotificationRules[Arn="arn:aws:codestar-notifications:us-west-2:000000000000:notificationrule/06422eef-ecf6-489a-89c7-b385f488ef07"].Id: value_mismatch (expected '06422eef-ecf6-489a-89c7-b385f488ef07', got '06422eef-ecf6-489a-89c7-b385f488ef07')
  Expected/Actual: as cause 1, for the single rule the filter selects.

No status, error code or error message diverges in any of the three causes: all answered the modeled 200 (POST /listNotificationRules) and no modeled error (InvalidNextTokenException, ValidationException) was involved.

Code under the causes (same checkout the emulator is built from):
  services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/src/ops/list_notification_rules.rs:182-217 — rules_in_region_order -> filter -> NotificationRuleSummary { id: Some(rule.id.clone()), arn: Some(rule.arn.clone()) }; pagination via encode_page_token.
  .../src/ops/list_notification_rules.rs:86-98 — rule_satisfies (CREATED_BY / EVENT_TYPE_ID / RESOURCE / TARGET_ADDRESS); selection is correct in all three steps.
  .../src/state.rs:575 — new_rule_id(): uuid::Uuid::new_v4().as_hyphenated().to_string() (per-run random identity).
  .../src/state.rs:368 — notification_rule_arn(region, id) -> emulator_arn("codestar-notifications", region, "notificationrule/<id>").
  .../src/ops/create_notification_rule.rs:329-331 — take_sequence() -> new_rule_id() -> notification_rule_arn(), stored as the rule's id/arn pair.

Smithy model consulted: ListNotificationRules (POST /listNotificationRules, 200, @paginated items=NotificationRules, errors InvalidNextTokenException 400 / ValidationException 400); NotificationRuleSummary.Id -> NotificationRuleId @length min 1 max 40, doc "The unique ID of the notification rule"; NotificationRuleSummary.Arn -> NotificationRuleArn @pattern ^arn:aws[^:\s]*:codestar-notifications:[^:\s]+:\d{12}:notificationrule\/(.*\S)?$. No pattern constrains the id's internal form, so that part is recorded behavior only.

## Steps to Reproduce
1. Clone the repository and boot the emulator built from it (the router that mounts service-com-amazonaws-codestarnotifications-codestarnotifications-20191015).
2. Replay trace set simparity-wire-aws-v1 against it (replay_wire_corpus). Summary: 95 steps, 91 passed, 4 mismatched, score 95.8%, bootstrap_signal=false.
3. Cause 1: replay fixture codestar-notifications-list-and-filter-rules through step task#6 — the unfiltered ListNotificationRules (POST /listNotificationRules with no Filters) after the fixture's CreateNotificationRule steps. The response is 200 and lists both rules, but the step diverges at $.NotificationRules[Arn="..."].Id for BOTH entries (verdict=mismatch, expected_status=200, actual_status=200).
4. Cause 2: continue the same fixture to step task#7 — a filtered ListNotificationRules returning a single rule. 200, correct rule selected, and $.NotificationRules[Arn="..."].Id diverges for that one entry.
5. Cause 3: continue to step task#8 — the other filtered ListNotificationRules. Same outcome for the other rule.
6. Confirm the fault is the reported identity and not one recording's values: replay the corpus twice. The UUIDs quoted in the details change between runs (the id is minted per run by state::new_rule_id) while all three clusters diverge identically every time.
7. After a fix, re-replay codestar-notifications-list-and-filter-rules#task#6, #task#7 and #task#8; all three must pass, and each summary's Id must be a bare NotificationRuleId string byte-for-byte identical to the resource segment of that entry's Arn, stable across calls for the same rule.
