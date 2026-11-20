Issue-Source: trace-sweep-divergence
Issue-Trace: wellarchitected-pagination-and-limit-validation
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wellarchitected-pagination-and-limit-validation
Issue-Shape-Id: com.amazonaws.wellarchitected#WellArchitectedApiServiceLambda
Issue-Key: wellarchitected|ListAnswers|4xx->4xx|$.Fields[*].Message
Issue-Key: wellarchitected|ListAnswers|4xx->4xx|$.Message
Issue-Commit: 1ca24a91a90c9de292a3da3027bdc18df2a22899

# ListAnswers refuses an undefined PillarId with a constant message that diverges from AWS's ValidationException diagnostic

> _Note: this issue was identified against the code base at commit `1ca24a91a90c9de292a3da3027bdc18df2a22899`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAnswers

## Description
Cause 1 of 1: ListAnswers' refusal of a PillarId the lens under review does not define carries the wrong diagnostic text, in both ValidationException.Message and ValidationException.Fields[0].Message. (fixture wellarchitected-pagination-and-limit-validation, step task#6)

AWS: Smithy (services/service-com-amazonaws-wellarchitected-wellarchitectedapiservicelambda/smithy-model/...json): ListAnswers input member PillarId is @httpQuery("PillarId") targeting PillarId (string, @length min 1 max 64) with no enumerated value set; the operation models ValidationException (HTTP 400), whose shape is Message (ExceptionMessage, required - "Description of the error"), Reason (ValidationExceptionReason: UNKNOWN_OPERATION | CANNOT_PARSE | FIELD_VALIDATION_FAILED | OTHER) and Fields (ValidationExceptionFieldList of ValidationExceptionField{Name, Message} - "Stores information about a field passed inside a request that resulted in an exception"). The model states no message text, so for the wording this is recorded behavior only: the recorded AWS response for this step is HTTP 400 and the corpus asserts $.Message and $.Fields[0].Message by SHAPE rather than literally (expected value reported as '<shaped-not-asserted>'), and the emulator's values do not satisfy that shaped assertion. The status (400) and the error class (ValidationException) both match; only the diagnostic text diverges. Hypothesis: AWS's diagnostic for this class is a template whose variable parts come from the request and the store (the PillarId value as sent, and the lens/review it was resolved against), which is why the recording asserts its shape instead of a literal string - and the emulator's text, having no variable part at all, cannot match that shape.

Emulator: answers HTTP 400 ValidationException with $.Message = "[Validation] The PillarId that was passed in is invalid." and $.Fields[0].Message = "The PillarId that was passed in is invalid.". That is a CONSTANT sentence: its only variable-looking element is the member name "PillarId", which is fixed for this operation, and it reproduces nothing from the request or from state - not the rejected pillar id, not the lens alias it was judged against, not the workload. In message terms: the emulator's fixed text is "The <Member> that was passed in is invalid." behind this service's "[Validation] " tag, and it carries no request- or state-derived values, whereas AWS's recorded message for this error class is asserted as a shaped (templated) value.

Why: a client that attributes a 400 from the diagnostic - support tooling, a CLI user, or a test that asserts on the wording of an invalid-filter refusal - cannot tell from the emulator which pillar value was rejected or against which lens, because the sentence names only the parameter. Anything written against AWS's message template (or any assertion that the message quotes the offending value) passes on AWS and fails on the emulator, and conversely a client developed against the emulator will see different text in production for the same request.

Fix: the shared wording for this refusal, not the handler's control flow (the handler correctly resolves PillarId against the lens's own pillar set and refuses rather than returning an empty page). It belongs in this service's lens-content validation wording - undefined_pillar_finding and the sentence helper state::invalid_parameter_sentence it calls - which must word the refusal as AWS words it for an undefined pillar filter: AWS's fixed text, with the variable parts (the PillarId value exactly as sent, and the lens the value was resolved against) substituted from the request and the store. The envelope must keep what the model and recordings already agree on: HTTP 400, ValidationException, Reason = FIELD_VALIDATION_FAILED, one Fields entry with Name = "PillarId", and the same finding restated in Message behind the "[Validation] " tag.

Not covered by the open issues: 20261115T052454Z-wellarchitected-importlens-lens-document-validation-reports-raw-json-schema.md is about ImportLens lens-document schema diagnostics, and 20261116T124849Z-wellarchitected-aws-official-lens-content-diverges-from-aws-framework.md is about lens catalog/answer-choice content. Neither owns this step's replay key.

Traces: wellarchitected-pagination-and-limit-validation#task#6 (ListAnswers, expected 400 / actual 400, $.Message and $.Fields[0].Message diverge).

## Evidence
Replay of fixture wellarchitected-pagination-and-limit-validation, step task#6, operation ListAnswers, verdict mismatch (expected_status 400, actual_status 400):

  $.Fields[0].Message: value_mismatch (expected '<shaped-not-asserted>', got 'The PillarId that was passed in is invalid.')
  $.Message: value_mismatch (expected '<shaped-not-asserted>', got '[Validation] The PillarId that was passed in is invalid.')

Expected (AWS, recorded): HTTP 400 ValidationException whose Message and Fields[0].Message are a templated diagnostic for the rejected pillar filter - fixed AWS text plus variable parts taken from the request/state - which the corpus asserts by shape.
Actual (emulator): HTTP 400 ValidationException whose Message is the tagged constant '[Validation] The PillarId that was passed in is invalid.' and whose Fields[0].Message is the same constant untagged; no part of the message is derived from the request or from the store.

Emulator source of the wording: services/service-com-amazonaws-wellarchitected-wellarchitectedapiservicelambda/src/service/lens_content_validation.rs lines 157-188 (undefined_pillar / undefined_pillar_finding, which builds the finding from state::PILLAR_ID_MEMBER and state::invalid_parameter_sentence) reached from src/ops/list_answers.rs lines 238-246.

## Steps to Reproduce
1. Start the emulator from this checkout and point an AWS client at it.
2. Create a workload with the AWS-official 'wellarchitected' lens associated (as the fixture's earlier steps do), so a live lens review exists.
3. Call ListAnswers for that workload and lens with a PillarId query parameter that satisfies the modeled length 1..64 but names no pillar of the lens, e.g.
   GET /workloads/<workloadId>/lensReviews/wellarchitected/answers?PillarId=not-a-pillar
   (aws wellarchitected list-answers --workload-id <id> --lens-alias wellarchitected --pillar-id not-a-pillar)
4. Observe HTTP 400 ValidationException with Message '[Validation] The PillarId that was passed in is invalid.' and Fields[0] = {Name: 'PillarId', Message: 'The PillarId that was passed in is invalid.'} - a constant sentence that quotes neither the rejected pillar id nor the lens it was resolved against, instead of AWS's templated diagnostic for this error class.
5. Equivalent replay: replay fixture wellarchitected-pagination-and-limit-validation and inspect step task#6.
