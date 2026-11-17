Issue-Source: trace-sweep-divergence
Issue-Trace: resiliencehubv2-assertion-lifecycle
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resiliencehubv2-assertion-lifecycle
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2|ListAssertions|2xx->2xx|$.assertions[*].text
Issue-Commit: 160a922c69f1bb93438761fd214abd9479460531

# resiliencehubv2 UpdateAssertion edit is never reflected in the ListAssertions read-back

> _Note: this issue was identified against the code base at commit `160a922c69f1bb93438761fd214abd9479460531`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateAssertion

## Description
CORRECTION/companion to 20261117T070946Z-resiliencehubv2-listassertions-serves-stale-assertion-state-neither-edits.md: that issue describes this same read-back fault but its representative replay key was mis-transcribed (it names task#3, the CreateAssertion validation-message step owned by 20261111T074347Z-resiliencehubv2-error-bodies-diverge-from-aws-validation-messages.md), so the step that proves an assertion EDIT is readable back — resiliencehubv2-assertion-lifecycle#task#6 — was left out of its pointer. This issue carries that key. 1 cause.

Cause 1 of 1: an assertion text edit is never reflected in the assertion ListAssertions reports.
AWS: Smithy: UpdateAssertion (POST /v2/update-assertion) takes serviceArn (required), assertionId (required, Uuid) and text (AssertionText, @length 1..1000) and returns the updated Assertion in output member `assertion` (@required); Assertion.text is @required and Assertion.updatedAt carries the time of the last edit. ListAssertions (GET /v2/list-assertions, @paginated, output `assertions` @required) lists the service's assertions. So after a 200 from UpdateAssertion, the listed element carrying that assertionId must carry the text that UpdateAssertion request supplied, with updatedAt advanced past createdAt; an assertionId that does not exist under the service is ResourceNotFoundException (404), so a 200 means the stored text really changed.
Emulator: the edit never happens — UpdateAssertion is an unimplemented scaffold handler that answers HTTP 501 (services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/update_assertion.rs:163, Scaffolded::not_implemented("UpdateAssertion")) — and the following ListAssertions answers 200 while still reporting the text supplied at CreateAssertion time. In the recorded fixture this surfaced as `$.assertions[0].text: value_mismatch (expected 'Survives the loss of one Region.', got 'Survives the loss of one Availability Zone.')`; those two strings are just this run's new text and original text. The requirement is that the listed text equals the text carried by the preceding UpdateAssertion request, whatever it is.
Why: a client that edits an assertion and re-lists to confirm sees its edit silently discarded, with no failure anywhere in the 200 read-back, so it proceeds on superseded assertion wording — review, drift comparison and reporting flows keyed on the assertion text all read the stale value.
Fix: the UpdateAssertion handler and the assertion record in this crate's state — locate the AssertionRecord by (serviceArn, assertionId), replace text, advance updatedAt, return the updated Assertion, and raise 404 ResourceNotFoundException for an unknown serviceArn/assertionId rather than 501. The read path needs no change: src/ops/list_assertions.rs:213-250 already projects the store.
Traces: resiliencehubv2-assertion-lifecycle#task#6 (ListAssertions after the edit).

Also related: 'Implement resiliencehubv2 UpdateAssertion: unimplemented scaffold stub' (20261116T202413Z-resiliencehubv2-updateassertion-stub.md) covers the 501 on the mutating call itself but carries no replay key for the read-back step above.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, /__admin/health 200):

fixture resiliencehubv2-assertion-lifecycle, step task#6, operation ListAssertions, verdict mismatch, expected status 200 / actual status 200:
  $.assertions[0].text: value_mismatch (expected 'Survives the loss of one Region.', got 'Survives the loss of one Availability Zone.')
  Expected = the text the preceding UpdateAssertion request set; actual = the text CreateAssertion stored. divergence_key: service=resiliencehubv2|ListAssertions|status=200->200|keys=$.assertions[0].text. stub_path empty, owned_by empty — no open issue carried this replay key.

Code: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/update_assertion.rs:153-163 binds its input and returns Scaffolded::not_implemented("UpdateAssertion") (HTTP 501), so the store is never mutated; src/ops/list_assertions.rs:213-250 reads the service record and projects assertions_in_creation_order() faithfully.

Model (smithy_query on services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/smithy-model/com-amazonaws-resiliencehubv2-ngrhservicecore.json): UpdateAssertion input serviceArn(required)/assertionId(required Uuid, len 36)/text(@length 1..1000); output assertion: Assertion (required); errors ResourceNotFoundException 404, ValidationException 400, ConflictException 409, AccessDeniedException 403, InternalServerException 500. Assertion.text and Assertion.source are @required; Assertion carries createdAt/updatedAt.

## Steps to Reproduce
1. POST /v2/create-service, then POST /v2/create-assertion under that serviceArn with text T1; note the returned assertionId.
2. POST /v2/update-assertion with that serviceArn + assertionId and text T2 (T2 != T1). Emulator answers 501 NotImplemented; AWS answers 200 with the updated Assertion (text T2, updatedAt advanced).
3. GET /v2/list-assertions?serviceArn=<arn>. Emulator answers 200 with assertions[0].text == T1 (the pre-edit text); AWS reports T2.

Re-replay resiliencehubv2-assertion-lifecycle#task#6 to confirm a fix.
