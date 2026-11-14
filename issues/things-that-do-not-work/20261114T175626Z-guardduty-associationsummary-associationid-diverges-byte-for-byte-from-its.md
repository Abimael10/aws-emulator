Issue-Source: trace-sweep-divergence
Issue-Trace: guardduty-association-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: guardduty-association-lifecycle guardduty-association-list-filters-across-two-rules guardduty-custom-detection-rule-association-lifecycle
Issue-Shape-Id: com.amazonaws.guardduty#GuardDutyAPIService
Issue-Key: guardduty|ListCustomDetectionRuleAssociations|2xx->2xx|$.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-role/association/1180f609368e1ed719b54c0432860002"].associationId
Issue-Key: guardduty|ListCustomDetectionRuleAssociations|2xx->2xx|$.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-role/association/1a622a0dab0d76f05fc2089bce090002"].associationId
Issue-Key: guardduty|ListCustomDetectionRuleAssociations|2xx->2xx|$.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-user/association/3fdcc07cdea739faa7b2f63a74c70003"].associationId
Issue-Key: guardduty|ListCustomDetectionRuleAssociations|2xx->2xx|$.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-role/association/f29d1b66ef11e787aff7c783d72a0002"].associationId
Issue-Commit: 161256edb21d00f789595a0968fdc81e8b5da465

# AssociationSummary associationId diverges byte-for-byte from its own ARN suffix in every listing

> _Note: this issue was identified against the code base at commit `161256edb21d00f789595a0968fdc81e8b5da465`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCustomDetectionRuleAssociations

## Description
This issue bundles 4 distinct net-new replay clusters, all in the guardduty crate and all on ListCustomDetectionRuleAssociations: each listing returns HTTP 200 with the right entry present, but the entry's `associationId` member is reported as a value_mismatch by the deterministic comparator when the entry is selected by its own `arn`. Fix ALL 4 causes and re-replay every fixture#step named in Traces (and in trace_ids) before submitting. None of the 4 clusters is marked owned by an open issue and none is blocked by a stub.

The shared fault: the identity an AssociationSummary advertises is not interchangeable with the identity named by the trailing `/association/<id>` segment of that same summary's own `arn`, so a caller cannot take an id out of a listing and use it as a request path label. Note for the fixer: the emulator's own projection helper (`detection_rule_association_summary` -> `detection_rule_association_view` -> `custom_detection_rule_association_arn`) is documented as deriving both members from `record.association_id`, and the replay renders the expected and the actual id with the SAME visible hex digits. So the compared difference does NOT lie in the digits: it lies in a dimension the report's rendering collapses (the serialized member's JSON type, its case, or leading/trailing whitespace in the emitted string). Compare the raw serialized `associationId` byte-for-byte against the ARN's trailing segment in the same payload when reproducing, rather than by eye.

Several OPEN guardduty issues describe this same fault family but do NOT cover these clusters — the replay reports all four with an empty owner, because the owning issues' replay keys embed a per-run ARN (hence a per-run minted id) and this run minted different ids, so the keys do not join. Treat these as still-open behaviour and re-verify the list projection end to end. The non-covering issues are: 20261107T064647Z-guardduty-custom-detection-rule-association-identity-is-not-stable.md (its causes 4-7 name the same four fixture#steps but key on ARNs containing ids e5d07a4c.../b7ec5eee.../01a3967b.../a255b621...), 20261107T151807Z-guardduty-listcustomdetectionruleassociations-summaries-report-an-associationid-that-does-not.md, 20261107T235238Z-guardduty-listcustomdetectionruleassociations-summary-reports-an-associationid-that-is-not.md, 20261108T181442Z-guardduty-associationsummary-associationid-is-serialized-in-a-form-that.md, 20261112T052523Z-guardduty-custom-detection-rule-associations-are-listed-under-a.md and 20261112T075616Z-guardduty-listcustomdetectionruleassociations-summaries-report-an-associationid-that-does-not-e1ac75.md. A fix that makes the serialized summary id byte-identical to its ARN suffix (and accepted as a Get path label) closes this bundle and that family together.

Cause 1 of 4: the association-lifecycle listing reports a summary whose `associationId` is not byte-identical to the id embedded in that same entry's `arn`. Fixture guardduty-association-lifecycle, step task#7.
AWS: Smithy ListCustomDetectionRuleAssociations (GET /custom-detection-rule/association, success 200; input MaxResults @httpQuery maxResults @range 1..100, NextToken @httpQuery nextToken, RuleId @httpQuery ruleId @length 1..100 @pattern ^[a-z0-9]+(-[a-z0-9]+)*$, Mode @httpQuery mode enum; readonly; paginated inputToken/outputToken NextToken, items RuleAssociations; errors AccessDeniedException 403, BadRequestException 400, InternalServerErrorException 500) returns a required `ruleAssociations` list of AssociationSummary. Smithy AssociationSummary.AssociationId -> AssociationId, jsonName `associationId`, required, @length 1..64, @pattern ^[a-zA-Z0-9_-]{1,64}$, "The unique identifier for the association."; AssociationSummary.Arn -> DetectionRuleArn, jsonName `arn`, required, @length 1..1024, @pattern ^arn:[a-zA-Z0-9\-]+:[a-zA-Z0-9\-]+:[a-zA-Z0-9\-]*:([0-9]*|aws):.+$, "The Amazon Resource Name (ARN) of the association." Recorded behavior only for the VALUE: one summary's two identity members agree — the ARN's trailing `/association/<id>` segment is exactly that entry's `associationId`, and that same string is what a following Get/Update/Delete accepts as its AssociationId path label. The id's digits, the region and the account in the ARN are per-run values and are NOT the requirement; the requirement is the equality between the two members of one payload.
Emulator: status 200 matches and the entry is found by its ARN, but `$.ruleAssociations[arn="...detection-rule/custom/<ruleId>/association/<id>"].associationId` is reported as value_mismatch with expected and actual rendering as the same characters (this run: '1180f609368e1ed719b54c0432860002' both sides) — i.e. the served member differs from the ARN suffix in serialized form, not in digits.
Why: list-then-act is the normal discovery path. A client that lists associations and then calls GetCustomDetectionRuleAssociation / UpdateCustomDetectionRuleAssociation / DeleteCustomDetectionRuleAssociation with the id the listing advertised addresses a handle the store does not resolve (the resolver filters on `record.association_id == association_id`), so the following call 404s even though the association plainly exists; ARN-keyed IAM policy and inventory joins built from the listing silently stop matching.
Fix: the AssociationSummary projection of the list handler and the shared association view/ARN helper it serializes through, in the guardduty crate's list operation and its state-side association projection, so the id a summary advertises and the id its own ARN names are one and the same serialized string.
Traces: guardduty-association-lifecycle#task#7 (affected_count 1).

Cause 2 of 4: the same list-summary identity divergence on the FIRST rule of the two-rule filtered listing. Fixture guardduty-association-list-filters-across-two-rules, step task#5.
AWS: same ListCustomDetectionRuleAssociations / AssociationSummary citation as cause 1 (RuleId is an equality filter on the member the summary reports; selecting nothing is an empty page, not an error). Recorded behavior only: a listing narrowed by RuleId reports each association under the identity it was created with, with each entry's ARN suffix equal to that entry's `associationId`.
Emulator: 200 matches; the entry located by its own ARN reports `associationId` as a value_mismatch, rendered identically on both sides (this run: '1a622a0dab0d76f05fc2089bce090002').
Why: a caller that narrows discovery by rule id cannot trust the ids it reads back, so a filtered discovery pass yields handles that do not resolve.
Fix: the same list-handler AssociationSummary projection as cause 1; listed separately because it is its own replay cluster with its own fixture and step and must be re-replayed on its own.
Traces: guardduty-association-list-filters-across-two-rules#task#5 (affected_count 1).

Cause 3 of 4: the same list-summary identity divergence on the SECOND rule of the two-rule filtered listing — a different ruleId, so a distinct association record and a distinct cluster. Fixture guardduty-association-list-filters-across-two-rules, step task#6.
AWS: same citation as cause 1. Recorded behavior only: the entry for the second rule reports its own association's identity, and its ARN suffix equals its own `associationId`.
Emulator: 200 matches; the entry for the second ruleId, selected by its ARN, reports a mismatching `associationId` (rendered identically on both sides; this run: '3fdcc07cdea739faa7b2f63a74c70003').
Why: the defect is per-association, not per-listing — a caller listing across two rules gets unusable handles for both, so neither rule's association can be acted on from the listing.
Fix: the same list-handler AssociationSummary projection as cause 1.
Traces: guardduty-association-list-filters-across-two-rules#task#6 (affected_count 1).

Cause 4 of 4: the same list-summary identity divergence on the second association-lifecycle fixture, after create/get/update have run. Fixture guardduty-custom-detection-rule-association-lifecycle, step task#7.
AWS: same citation as cause 1. Recorded behavior only: after the lifecycle has established an association, the final listing reports it under that established identity, with the entry's ARN suffix equal to its `associationId`.
Emulator: 200 matches; the entry selected by its ARN reports a mismatching `associationId` (rendered identically on both sides; this run: 'f29d1b66ef11e787aff7c783d72a0002').
Why: the lifecycle's final discovery step hands the caller an id that does not round-trip, so a delete issued from the listing addresses nothing and the association is leaked.
Fix: the same list-handler AssociationSummary projection as cause 1.
Traces: guardduty-custom-detection-rule-association-lifecycle#task#7 (affected_count 1).

## Evidence
Wire replay of trace set simparity-wire-aws-v1 against the booted emulator: trace_set_id simparity-wire-aws-v1, emulator_up=true ("/__admin/health answered 200 and the :34449 data listener accepts"), bootstrap_signal=false, 86 fixtures replayed, 559 steps total, 533 passed, 26 mismatched, 0 errored, 0 blocked, not_implemented=0, score 95.3%. 15 divergence clusters; 11 are marked owned by open guardduty issues and 0 are stub-blocked, leaving the 4 below with an empty `owned_by` and an empty `stub_path`. Expected = the recorded AWS response, got = the emulator.

Cause 1 — guardduty-association-lifecycle#task#7, ListCustomDetectionRuleAssociations, expected status 200, got 200:
  $.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-role/association/1180f609368e1ed719b54c0432860002"].associationId: value_mismatch (expected '1180f609368e1ed719b54c0432860002', got '1180f609368e1ed719b54c0432860002')
  divergence_key: service=guardduty|ListCustomDetectionRuleAssociations|status=200->200|keys=$.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-role/association/1180f609368e1ed719b54c0432860002"].associationId
  affected_count 1; owned_by []; stub_path "".
  Note: the two rendered values print identically, so the compared difference is NOT in the visible digits; the deterministic comparator still reports the member as a mismatch. Compare the raw serialized member against the ARN's trailing segment byte-for-byte (type, case, whitespace).

Cause 2 — guardduty-association-list-filters-across-two-rules#task#5, ListCustomDetectionRuleAssociations, expected 200, got 200:
  $.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-role/association/1a622a0dab0d76f05fc2089bce090002"].associationId: value_mismatch (expected '1a622a0dab0d76f05fc2089bce090002', got '1a622a0dab0d76f05fc2089bce090002')
  affected_count 1; owned_by []; stub_path "".

Cause 3 — guardduty-association-list-filters-across-two-rules#task#6, ListCustomDetectionRuleAssociations, expected 200, got 200:
  $.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-user/association/3fdcc07cdea739faa7b2f63a74c70003"].associationId: value_mismatch (expected '3fdcc07cdea739faa7b2f63a74c70003', got '3fdcc07cdea739faa7b2f63a74c70003')
  affected_count 1 (note the different ruleId, admin-policy-attached-to-user, i.e. a second association record); owned_by []; stub_path "".

Cause 4 — guardduty-custom-detection-rule-association-lifecycle#task#7, ListCustomDetectionRuleAssociations, expected 200, got 200:
  $.ruleAssociations[arn="arn:aws:guardduty:us-west-2:000000000000:detection-rule/custom/admin-policy-attached-to-role/association/f29d1b66ef11e787aff7c783d72a0002"].associationId: value_mismatch (expected 'f29d1b66ef11e787aff7c783d72a0002', got 'f29d1b66ef11e787aff7c783d72a0002')
  affected_count 1; owned_by []; stub_path "".

Model evidence (smithy_query on services/service-com-amazonaws-guardduty-guarddutyapiservice/smithy-model/com-amazonaws-guardduty-guarddutyapiservice.json):
  ListCustomDetectionRuleAssociations: GET /custom-detection-rule/association, code 200, readonly, paginated {inputToken: NextToken, outputToken: NextToken, items: RuleAssociations, pageSize: MaxResults}; output RuleAssociations -> AssociationSummaryList jsonName ruleAssociations, required.
  AssociationSummary.AssociationId -> AssociationId, jsonName associationId, required, length 1..64, pattern ^[a-zA-Z0-9_-]{1,64}$.
  AssociationSummary.Arn -> DetectionRuleArn, jsonName arn, required, length 1..1024, pattern ^arn:[a-zA-Z0-9\-]+:[a-zA-Z0-9\-]+:[a-zA-Z0-9\-]*:([0-9]*|aws):.+$.

Supporting code reading (for the fix, not a requirement): services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/list_custom_detection_rule_associations.rs lines 221-224 build each entry via `crate::state::detection_rule_association_summary(record)`. services/service-com-amazonaws-guardduty-guarddutyapiservice/src/state/custom_detection_rules.rs lines 462-475 (`detection_rule_association_summary`) delegates to lines 431-450 (`detection_rule_association_view`), which sets `association_id: record.association_id.clone()` and derives `arn` through lines 300-310 (`custom_detection_rule_association_arn`, format "<rule path>/{rule_id}/association/{association_id}"). Both members are therefore claimed to come from the one stored field, yet the comparator still rejects the serialized summary id — so inspect how that field is written (state.rs `mint_id`, documented at lines ~313-326 as 32 hex characters whose low bits carry the store's sequence counter) and how the member is serialized, not just how it is cloned.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator, then replay the simparity-wire-aws-v1 wire corpus. Confirm emulator_up=true and bootstrap_signal=false before reading any divergence.
2. Cause 1: replay fixture guardduty-association-lifecycle through step task#7 (GET /custom-detection-rule/association). Capture the RAW response body. For the entry whose `arn` ends '/association/<id>', compare that entry's serialized `associationId` byte-for-byte with the ARN's trailing segment — including JSON type, case and any leading/trailing whitespace, since the two values render identically in the replay report.
3. Cause 2: replay fixture guardduty-association-list-filters-across-two-rules through step task#5 and make the same ARN-suffix vs associationId byte comparison for the admin-policy-attached-to-role entry.
4. Cause 3: continue the same fixture to step task#6 and repeat the comparison for the admin-policy-attached-to-user entry (a second association record under a different ruleId).
5. Cause 4: replay fixture guardduty-custom-detection-rule-association-lifecycle through step task#7 and repeat the comparison.
6. Harness-free repro: ensure a detector exists (ListDetectors, else CreateDetector(Enable=true)); ListCustomDetectionRules and pick a published ruleId (e.g. 'admin-policy-attached-to-role'); CreateCustomDetectionRuleAssociation(RuleId=<ruleId>, Mode='DRY_RUN'); then ListCustomDetectionRuleAssociations(RuleId=<ruleId>). For every entry assert (a) the serialized `associationId` is byte-identical to the trailing `/association/<id>` segment of that entry's own `arn`, and (b) GetCustomDetectionRuleAssociation(RuleId=<ruleId>, AssociationId=<the listed associationId, used verbatim as the path label>) answers 200 for that same association. Either assertion failing is this defect.
7. Re-replay all four fixture#steps (guardduty-association-lifecycle#task#7, guardduty-association-list-filters-across-two-rules#task#5, guardduty-association-list-filters-across-two-rules#task#6, guardduty-custom-detection-rule-association-lifecycle#task#7) before submitting; all four must pass.
