Issue-Source: trace-sweep-divergence
Issue-Trace: drs-recovery-plan-lifecycle
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: drs-recovery-plan-lifecycle drs-recovery-plan-lifecycle-and-tag-round-trip drs-resource-not-found-and-tag-arn-errors
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs|CreateRecoveryPlan|2xx->2xx|$.recoveryPlan.status
Issue-Key: drs|GetRecoveryPlan|2xx->2xx|$.recoveryPlan.status
Issue-Key: drs|GetRecoveryPlan|2xx->2xx|$.recoveryPlan.description
Issue-Key: drs|GetRecoveryPlan|2xx->2xx|$.recoveryPlan.name
Issue-Key: drs|GetRecoveryPlan|4xx->2xx|$.__type
Issue-Key: drs|GetRecoveryPlan|4xx->2xx|$.message
Issue-Key: drs|GetRecoveryPlan|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: drs|GetRecoveryPlan|4xx->2xx|status
Issue-Key: drs|CreateRecoveryPlan|2xx->2xx|$.recoveryPlan.recoveryPlanArn
Issue-Key: drs|GetRecoveryPlan|4xx->4xx|$.message
Issue-Commit: 656cbf9b11e99225d4fd85cb8b7394b019211c7b

# drs Recovery Plan lifecycle is not faithful: status, update/delete persistence and the not-found message all diverge

> _Note: this issue was identified against the code base at commit `656cbf9b11e99225d4fd85cb8b7394b019211c7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRecoveryPlan

## Description
The emulator's Recovery Plan resource diverges from AWS Elastic Disaster Recovery in FOUR independent ways. All four are drs, all four concern the one `RecoveryPlan` resource, and a fixer must close all FOUR before submitting.

Cause 1 of 4: a Recovery Plan is reported with status INVALID from the moment it is created, where AWS reports ACTIVE.
AWS: Smithy: `CreateRecoveryPlan` -> output `recoveryPlan` (RecoveryPlan), http code 201; `GetRecoveryPlan` -> output `recoveryPlan`, http code 200; `RecoveryPlan$status` is required, target `RecoveryPlanStatus`, an enum whose `values` are exactly [ACTIVE, INVALID]. The model's prose for that enum says INVALID means "the plan has no SERVER type steps and cannot be executed". The recorded responses show that AWS nonetheless reports `status: "ACTIVE"` for a plan that was just created and holds no steps at all, and keeps reporting ACTIVE on the following read. So on AWS a plan's reported status is NOT a function of the steps it currently holds at create/read time; a newly created, stepless plan is ACTIVE. (Hypothesis: AWS only degrades a plan to INVALID once step composition makes it unexecutable, not for an empty plan.)
Emulator: every response that reports a plan carries `status: "INVALID"`. The status is computed from the stored steps rather than taken from the plan's own state: `RecoveryPlanRecord::status()` returns STATUS_ACTIVE only when some step is a `serverStepConfiguration`, else STATUS_INVALID, and `to_wire()` is the single place that renders it — so create and every read agree on the wrong value. Replay: `$.recoveryPlan.status: value_mismatch (expected 'ACTIVE', got 'INVALID')` on CreateRecoveryPlan 201 and on GetRecoveryPlan 200.
Why: a client that gates on `status == ACTIVE` before adding steps, tagging, or executing a plan sees every plan it just created as permanently unexecutable, so the whole plan workflow stalls against the emulator while it succeeds against AWS. The value is also one of only two enum members, so there is no third state to disambiguate it.
Fix: the plan's status in its state/serialization — `RecoveryPlanRecord::status()` / `to_wire()` in the drs recovery-plan state module, so a plan with no steps reports ACTIVE as the recording shows.
Traces: drs-recovery-plan-lifecycle#task#1 (CreateRecoveryPlan 201), drs-recovery-plan-lifecycle#task#2 (GetRecoveryPlan 200), drs-recovery-plan-lifecycle-and-tag-round-trip#task#1 (CreateRecoveryPlan 201).

Cause 2 of 4: an update to a plan's name/description never reaches the store, so a later GetRecoveryPlan reports the pre-update plan.
AWS: Smithy: `UpdateRecoveryPlan` input `recoveryPlanArn` (required, StrictDRSARN), `name` (RecoveryPlanName, len 1..256, pattern ^[a-zA-Z0-9][a-zA-Z0-9 _-]*$), `description` (RecoveryPlanDescription, len 0..1024); output `recoveryPlan` required; http POST /UpdateRecoveryPlan, code 200. On AWS the update is durable: the following `GetRecoveryPlan` reports the NEW name and the NEW description.
Emulator: `UpdateRecoveryPlan::handle` is still the scaffold body — it binds its arguments and returns `Scaffolded::not_implemented("UpdateRecoveryPlan")` (501), writing nothing. Consequently the subsequent read serves the stale record: `$.recoveryPlan.name: value_mismatch (expected the updated name, got the name the create minted)` and `$.recoveryPlan.description: missing (expected the description the update set, got None)`.
Why: a client that renames or re-describes a plan and then reads it back gets the old name and no description, so any read-after-write reconciliation (idempotent provisioning, drift detection, a console refresh) concludes the update was lost. `description` being absent rather than empty also makes the member look as if it was never set.
Fix: the UpdateRecoveryPlan handler plus the recovery-plan state it must mutate (set `name`/`description` when present, restamp `updatedAt`, return the stored plan) so the write is visible to GetRecoveryPlan.
Traces: drs-recovery-plan-lifecycle#task#4 (GetRecoveryPlan 200, stale name + missing description), drs-recovery-plan-lifecycle-and-tag-round-trip#task#3 (GetRecoveryPlan 200, missing description).

Cause 3 of 4: a deleted Recovery Plan is still readable — GetRecoveryPlan answers 200 with the plan instead of a 404 ResourceNotFoundException.
AWS: Smithy: `DeleteRecoveryPlan` input `recoveryPlanArn` required, output `recoveryPlanArn` required, http POST /DeleteRecoveryPlan code 200; `GetRecoveryPlan` models `ResourceNotFoundException` with `httpError` 404. After the plan is deleted, AWS answers the read with 404, body `__type: "com.amazon.drs#ResourceNotFoundException"`, the `x-amzn-errortype` header set, and a message in the not-found form of Cause 4.
Emulator: the plan survives the delete, so the read returns 200 with the full plan body. `DeleteRecoveryPlan::handle` is the scaffold body returning `Scaffolded::not_implemented("DeleteRecoveryPlan")`, so nothing is ever removed from the store. Replay: `status: expected 404, got 200`, with `$.message`, `$.__type` and `$headers.x-amzn-errortype` all absent from the actual response.
Why: a client cannot tell a deleted plan from a live one. Teardown loops that poll until the read 404s never terminate; test suites that assert deletion pass incorrectly; and a re-create-after-delete flow sees the stale plan and its stale ARN.
Fix: the DeleteRecoveryPlan handler and the recovery-plan state (remove the record keyed by the id in the ARN, answer 200 echoing `recoveryPlanArn`), after which the existing not-found path in GetRecoveryPlan produces the 404.
Traces: drs-recovery-plan-lifecycle#task#8 (GetRecoveryPlan after delete), and the same cluster's drs-recovery-plan-lifecycle-and-tag-round-trip#teardown#9.

Cause 4 of 4: the Recovery Plan ResourceNotFoundException message is worded differently from AWS and names the ARN where AWS names the plan id.
AWS: Smithy: `GetRecoveryPlan` error `ResourceNotFoundException`, errorKind client, `httpError` 404, members message/code/resourceId/resourceType. The message text itself is recorded behavior only; the recording shows AWS's form for a missing recovery plan as the fixed text `Recovery plan '` + <value> + `' not found.` where the single variable part is the RecoveryPlanID taken from the `recoveryPlanArn` the request named (the `plan-...` resource suffix), and the fixed text includes the lower-case "plan", the single quotes and the trailing period. The HTTP status (404) and the error shape/code (ResourceNotFoundException) already match.
Emulator: the message is built in one place for every plan operation as `"{RESOURCE_TYPE} with Arn: '{plan_arn}' was not found"`, i.e. `Recovery Plan with Arn: '<full arn>' was not found` — capitalised "Plan", the inserted words "with Arn:", the whole ARN instead of the plan id, and no trailing period. Replay: `$.message: value_mismatch (expected "Recovery plan '<planId>' not found.", got "Recovery Plan with Arn: '<arn>' was not found")`.
Why: the message is part of the error contract. A client (or a human runbook) that matches on AWS's wording, or that parses the plan id out of the message to report which plan is missing, gets no match and no id — it sees an ARN embedded in prose AWS never emits.
Fix: the shared not-found constructor for the recovery-plan resource in the drs state module (the one `not_found` helper every plan operation calls), so the message uses AWS's fixed text with the plan id from the request's ARN as its only variable part.
Traces: drs-resource-not-found-and-tag-arn-errors#task#3 (GetRecoveryPlan 404, status and error code already correct, message wrong).

Not filed as causes here: the four clusters the replay already attributes elsewhere — TagResource/ListTagsForResource/UntagResource (501 scaffold stubs, stub issues own them; 20261110T042638Z-drs-tagresource-stub.md names one) and CreateReplicationConfigurationTemplate dropping `tags` (owned by 20261115T172254Z-drs-createreplicationconfigurationtemplate-201-response-drops-the-template-s.md). Also NOT a requirement: the `$.recoveryPlan.recoveryPlanArn` difference in drs-recovery-plan-lifecycle-and-tag-round-trip#task#1 — the expectation there is an unresolved fixture placeholder and the ARN's account/region/plan-id are per-environment generated values, so only the `status` key of that step is claimed above. The open issue 'drs: request-body validation refusals use the wrong error shape and message' concerns request-body validation, not any of these four; none of these four is covered by it.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 41.2%, 35 mismatched steps of 68), drs only.

Cause 1 — status:
- drs-recovery-plan-lifecycle#task#1 CreateRecoveryPlan, expected 201 / got 201: `$.recoveryPlan.status: value_mismatch (expected 'ACTIVE', got 'INVALID')`.
- drs-recovery-plan-lifecycle#task#2 GetRecoveryPlan, 200/200: same mismatch.
- drs-recovery-plan-lifecycle-and-tag-round-trip#task#1 CreateRecoveryPlan, 201/201: `$.recoveryPlan.status` ACTIVE vs INVALID (plus the unresolved-placeholder ARN key, not claimed).

Cause 2 — update not persisted:
- drs-recovery-plan-lifecycle#task#4 GetRecoveryPlan, 200/200: `$.recoveryPlan.name: value_mismatch (expected 'sim2real-probe-drs-plan-upd-351457f9', got 'sim2real-probe-drs-plan-8dc05914')` — i.e. the name the update set vs the name the create minted; `$.recoveryPlan.description: missing (expected 'sim2real probe recovery plan', got None)`.
- drs-recovery-plan-lifecycle-and-tag-round-trip#task#3 GetRecoveryPlan, 200/200: `$.recoveryPlan.description: missing (expected 'sim2real probe description', got None)`.
- Source: UpdateRecoveryPlan handler returns `Scaffolded::not_implemented("UpdateRecoveryPlan")` (services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/update_recovery_plan.rs:172).

Cause 3 — delete not persisted:
- drs-recovery-plan-lifecycle#task#8 GetRecoveryPlan: `status: expected 404, got 200`; `$.message: missing (expected "Recovery plan 'plan-...' not found.", got None)`; `$.__type: missing (expected 'com.amazon.drs#ResourceNotFoundException', got None)`; `$headers.x-amzn-errortype: missing`. Same cluster: drs-recovery-plan-lifecycle-and-tag-round-trip#teardown#9.
- Source: DeleteRecoveryPlan handler returns `Scaffolded::not_implemented("DeleteRecoveryPlan")` (src/ops/delete_recovery_plan.rs:171).

Cause 4 — not-found message wording:
- drs-resource-not-found-and-tag-arn-errors#task#3 GetRecoveryPlan, expected 404 / got 404: `$.message: value_mismatch (expected "Recovery plan 'plan-00000000000000000000000000000000' not found.", got "Recovery Plan with Arn: 'arn:aws:drs:us-west-2:000000000000:recovery-plan/plan-00000000000000000000000000000000' was not found")`. Fixed text per AWS: `Recovery plan '` ... `' not found.`; variable part: the plan id from the request's recoveryPlanArn. The quoted account id, region and plan id above are that run's values, not part of the template.
- Source: `not_found()` builds `"{RESOURCE_TYPE} with Arn: '{plan_arn}' was not found"` (src/state/recovery_plans.rs:145-154).

Model citations: smithy_query operation CreateRecoveryPlan/GetRecoveryPlan/UpdateRecoveryPlan/DeleteRecoveryPlan and shape RecoveryPlan/RecoveryPlanStatus/ResourceNotFoundException from services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/smithy-model/com-amazonaws-drs-elasticdisasterrecoveryservice.json.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay trace set simparity-wire-aws-v1 (replay_wire_corpus) and read the drs clusters.

Per cause, by hand against the drs endpoint:

Cause 1: POST /CreateRecoveryPlan {"name":"<valid name>"} -> 201. Observe `recoveryPlan.status == "INVALID"`; AWS reports "ACTIVE". POST /GetRecoveryPlan {"recoveryPlanArn":"<arn from the 201>"} -> 200 with the same wrong status. (fixtures drs-recovery-plan-lifecycle#task#1, #task#2, drs-recovery-plan-lifecycle-and-tag-round-trip#task#1)

Cause 2: create a plan, then POST /UpdateRecoveryPlan {"recoveryPlanArn":"<arn>","name":"<new name>","description":"<text>"}; then POST /GetRecoveryPlan {"recoveryPlanArn":"<arn>"}. The read reports the ORIGINAL name and no `description`. (drs-recovery-plan-lifecycle#task#4, drs-recovery-plan-lifecycle-and-tag-round-trip#task#3)

Cause 3: create a plan, POST /DeleteRecoveryPlan {"recoveryPlanArn":"<arn>"}, then POST /GetRecoveryPlan {"recoveryPlanArn":"<arn>"}. The read answers 200 with the plan; AWS answers 404 ResourceNotFoundException with `__type` and the `x-amzn-errortype` header. (drs-recovery-plan-lifecycle#task#8)

Cause 4: POST /GetRecoveryPlan with a syntactically valid drs recovery-plan ARN for a plan that was never created -> 404. Compare `message` with AWS's form `Recovery plan '<planIdFromTheArn>' not found.`; the emulator answers `Recovery Plan with Arn: '<full arn>' was not found`. (drs-resource-not-found-and-tag-arn-errors#task#3)
