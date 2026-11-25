Issue-Source: trace-sweep-divergence
Issue-Trace: resiliencehubv2-assertion-lifecycle
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resiliencehubv2-assertion-lifecycle
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2|ListAssertions|2xx->2xx|$.assertions[*]
Issue-Related: 20261111T074347Z-resiliencehubv2-error-bodies-diverge-from-aws-validation-messages.md
Issue-Commit: 20b8419944fb94f563e8819023aa5fee802036aa

# resiliencehubv2 ListAssertions serves stale assertion state: neither edits nor deletions are reflected

> _Note: this issue was identified against the code base at commit `20b8419944fb94f563e8819023aa5fee802036aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAssertions

## Description
This issue bundles 2 net-new causes, both in resiliencehubv2's assertion read path: the assertion collection ListAssertions reports is never mutated, so an assertion's text edit is invisible and a deleted assertion keeps being listed. ListAssertions itself reads the store correctly (services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/list_assertions.rs:213-250); the store is simply never changed, because the two mutating operations are scaffold stubs that answer 501. Both causes MUST be closed before this issue is submitted.

Cause 1 of 2: ListAssertions keeps reporting the pre-edit assertion text after UpdateAssertion.
AWS: Smithy: UpdateAssertion (POST /v2/update-assertion) takes serviceArn (required), assertionId (required, Uuid) and text (AssertionText, @length 1..1000) and returns the updated Assertion (output member `assertion`, required); Assertion.text is @required and Assertion.updatedAt is the timestamp of the last edit. ListAssertions (GET /v2/list-assertions, @paginated, output member `assertions` @required) lists the assertions for the service, so after a successful UpdateAssertion the listed assertion carrying that assertionId must carry the text last written by UpdateAssertion, with updatedAt advanced. AWS raises ResourceNotFoundException (404) for an assertionId that does not exist, so a 200 from UpdateAssertion means the stored text changed.
Emulator: the step that edits the assertion is an unimplemented stub (ops/update_assertion.rs returns Scaffolded::not_implemented("UpdateAssertion"), HTTP 501), so nothing in the store moves; the subsequent ListAssertions answers 200 and still reports the text supplied at CreateAssertion time. On the recorded fixture the asserted element diverged as `$.assertions[0].text: value_mismatch (expected 'Survives the loss of one Region.', got 'Survives the loss of one Availability Zone.')` — the two strings are the request's new text and the originally created text respectively; neither string is part of the requirement, the requirement is that the listed text equals the text the preceding UpdateAssertion request carried.
Why: a client that edits an assertion and re-lists to confirm sees its edit silently discarded. Nothing reports a failure (the list call is 200), so the client proceeds believing the resilience assertion it stored is the one it wrote, and any workflow keyed on the assertion wording (review, drift comparison, reporting) operates on superseded text.
Fix: the UpdateAssertion handler and the assertion record in this crate's state — the handler must locate the AssertionRecord by (serviceArn, assertionId), replace its text, advance updatedAt and return the updated Assertion, so the existing ListAssertions projection reports it. 404 ResourceNotFoundException when the assertionId is unknown under that service.
Traces: resiliencehubv2-assertion-lifecycle#task#6 (ListAssertions after the edit).

Cause 2 of 2: ListAssertions still returns an assertion after it has been deleted.
AWS: Smithy: DeleteAssertion (POST /v2/delete-assertion) takes serviceArn (required) and assertionId (required, Uuid) and returns the deleted `assertionId`; its modeled errors include ResourceNotFoundException (404) for an assertion that does not exist. A deleted assertion is therefore no longer a member of the service's assertion collection, so a following ListAssertions for that serviceArn must not include an element whose assertionId is the deleted one — and when it was the service's only assertion, `assertions` must be the empty list (the member is @required, so it is still present, just empty).
Emulator: the delete step is an unimplemented stub (ops/delete_assertion.rs, HTTP 501), so the record survives and the following ListAssertions answers 200 with the assertion still in place: `$.assertions[0]: extra (expected None, got {serviceArn, assertionId, text, source: 'USER', createdAt, updatedAt})`. The expected list for that step is empty. The arn, uuid and timestamps in that payload are per-run values and are not the requirement; the requirement is the absence of the deleted assertionId from the list.
Why: a client that deletes an assertion and re-lists sees it resurrected, so delete appears to have silently failed while actually returning no failure at all. Clean-up loops never terminate (the list never shrinks), and quota/count logic and any "assertions currently in force" view keep counting an assertion the caller removed.
Fix: the DeleteAssertion handler plus the assertion collection in this crate's state — remove the AssertionRecord keyed by (serviceArn, assertionId), return the deleted assertionId, and raise 404 ResourceNotFoundException for an unknown assertionId (and for an unknown serviceArn) instead of 501.
Traces: resiliencehubv2-assertion-lifecycle#task#10 (ListAssertions after the delete).

Relation to already-open issues: 'Implement resiliencehubv2 UpdateAssertion: unimplemented scaffold stub' (20261116T202413Z-resiliencehubv2-updateassertion-stub.md) and 'Implement resiliencehubv2 DeleteAssertion: unimplemented scaffold stub' (20261116T202413Z-resiliencehubv2-deleteassertion-stub.md) cover the 501s on the mutating calls themselves; neither carries the replay keys for the READ-back steps above, which are what prove the assertion collection actually changed. Re-replay the two fixture steps named here after implementing those operations.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, /__admin/health 200):

Cause 1 — fixture resiliencehubv2-assertion-lifecycle, step task#6, operation ListAssertions, verdict mismatch, expected status 200 / actual status 200:
  $.assertions[0].text: value_mismatch (expected 'Survives the loss of one Region.', got 'Survives the loss of one Availability Zone.')
  The expected string is the text the preceding UpdateAssertion request set; the actual string is the text from CreateAssertion. The edit was never applied because UpdateAssertion answered 501 (services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/update_assertion.rs:163 — Scaffolded::not_implemented("UpdateAssertion")).

Cause 2 — fixture resiliencehubv2-assertion-lifecycle, step task#10, operation ListAssertions, verdict mismatch, expected status 200 / actual status 200:
  $.assertions[0]: extra (expected None, got {'serviceArn': 'arn:aws:resiliencehub:us-west-2:000000000000:service/5b395f70-ae1c-8276-ab39-5ea4a8d8a023', 'assertionId': '0ad32c09-bd5c-492b-ba13-e3d3cb64bad2', 'text': 'Survives the loss of one Availability Zone.', 'source': 'USER', 'createdAt': 1794899181.285, 'updatedAt': 1794899181.285})
  Expected: an empty `assertions` list, the deleted assertion being the service's only one. The arn/uuid/timestamps shown are per-run values, not requirements.

Model evidence (smithy_query, services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/smithy-model/com-amazonaws-resiliencehubv2-ngrhservicecore.json):
  UpdateAssertion: input serviceArn(required)/assertionId(required, Uuid 36)/text(AssertionText @length 1..1000); output assertion: Assertion (required); errors AccessDenied 403, Conflict 409, InternalServer 500, ResourceNotFound 404, Validation 400.
  DeleteAssertion: input serviceArn(required)/assertionId(required); output assertionId; errors AccessDenied 403, InternalServer 500, ResourceNotFound 404, Validation 400.
  ListAssertions: GET /v2/list-assertions, output assertions: AssertionList (required), @paginated; Assertion members serviceArn/assertionId/text/source all @required, plus createdAt/updatedAt.

Read path is not at fault: src/ops/list_assertions.rs:213-250 looks the service up by serviceArn (404 when absent) and projects assertions_in_creation_order() straight from the store.

## Steps to Reproduce
Cause 1 (fixture resiliencehubv2-assertion-lifecycle, step task#6):
1. POST /v2/create-service, then POST /v2/create-assertion for that serviceArn with some text T1; note the returned assertionId.
2. POST /v2/update-assertion with that serviceArn + assertionId and text T2 (T2 != T1). Emulator answers 501 NotImplemented instead of 200 with the updated Assertion.
3. GET /v2/list-assertions?serviceArn=<arn>. Emulator answers 200 with assertions[0].text == T1; AWS reports T2 and an advanced updatedAt.

Cause 2 (same fixture, step task#10):
4. With exactly one assertion stored under the service, POST /v2/delete-assertion with that serviceArn + assertionId. Emulator answers 501 instead of 200 with the deleted assertionId.
5. GET /v2/list-assertions?serviceArn=<arn>. Emulator answers 200 with the assertion still present; AWS returns assertions == [].

Re-replay both resiliencehubv2-assertion-lifecycle#task#6 and resiliencehubv2-assertion-lifecycle#task#10 to confirm a fix.
