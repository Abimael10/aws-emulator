Issue-Source: trace-sweep-divergence
Issue-Trace: wellarchitected-custom-lens-import-version-lifecycle
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wellarchitected-custom-lens-import-version-lifecycle
Issue-Shape-Id: com.amazonaws.wellarchitected#WellArchitectedApiServiceLambda
Issue-Key: wellarchitected|ImportLens|4xx->4xx|$.Message
Issue-Commit: d2d81c0034aa9e73de1da910ad0ee37749603f85

# ImportLens lens-document validation reports raw JSON-schema diagnostics instead of AWS's semantic message

> _Note: this issue was identified against the code base at commit `d2d81c0034aa9e73de1da910ad0ee37749603f85`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ImportLens

## Description
Cause 1 of 1: ImportLens rejects a structurally invalid custom-lens document with the correct HTTP status and error shape, but the ValidationException Message carries a raw JSON-Schema validator diagnostic rather than the semantic sentence AWS words for that defect class.

AWS: Smithy: ImportLens (PUT /importLens, http code 200) takes JSONString -> LensJSON (@required, @length min 2 max 500000) and models ValidationException (errorKind client, httpError 400) whose Message member targets ExceptionMessage (@required), with optional Reason (ValidationExceptionReason: UNKNOWN_OPERATION | CANNOT_PARSE | FIELD_VALIDATION_FAILED | OTHER) and Fields (ValidationExceptionFieldList). The model constrains the envelope but not the message text, so the message itself is recorded behavior only. The recording shows AWS answering HTTP 400 with a ValidationException whose Message is the single sentence "[Validation] Empty pillars list". Its fixed text is the bracketed "[Validation] " prefix plus a short noun-phrase naming the semantic defect found in the lens document (here, that the document's pillars list carries no pillars); the variable part is that defect phrase, which is derived from which lens-document rule the submitted JSONString broke. AWS validates the lens document semantically and names the violated lens-schema rule in prose; it does not surface the underlying validator's internal report.

Emulator: for the same step the emulator answers HTTP 400 with the same error shape, but Message is "[Validation] [object has missing required properties ([\"pillars\"])]" — it keeps the "[Validation] " prefix and then interpolates the JSON-Schema library's own failure record (a bracketed "object has missing required properties ([...])" report naming the raw JSON member) in place of AWS's sentence. It also classifies the defect differently from AWS: AWS reports the pillars list as present-but-empty, while the emulator reports pillars as absent.

Why: a client that branches on, logs, or surfaces the ValidationException message to distinguish lens-authoring errors sees text that exists in no AWS response. Message-matching assertions in SDK integration tests and lens-authoring tooling that explains to the user which part of the lens document to fix both break, and the raw diagnostic points at a JSON member name and a "missing required property" cause that misdescribe the actual violation (an empty collection, not an omitted one), so the operator is sent to fix the wrong thing.

Fix: the ImportLens handler's lens-document validation step in the wellarchitected service crate. It must validate the decoded lens document against the lens schema's semantic rules itself and emit AWS's sentence for each rule it enforces — formatting ValidationException.Message as the "[Validation] " prefix followed by the defect phrase for the rule that failed (empty pillars list, and the equivalent phrases for the other lens-document rules) — rather than stringifying whatever its JSON-Schema validator returned into the message. Pillar-list presence and pillar-list emptiness must be distinguished so each maps to AWS's own phrase.

Traces: wellarchitected-custom-lens-import-version-lifecycle#task#8 (ImportLens, expected status 400, actual status 400, $.Message value_mismatch).

No open issue covers this: the closest, 20261111T154501Z-wellarchitected-listanswers-invalid-pillarid-validationexception-uses-a-generic-summary.md, is about ListAnswers' invalid-PillarId field message, and 20261103T090801Z-wellarchitected-exportlens-re-serializes-the-lens-document-instead-of.md is about ExportLens serialization, not ImportLens validation messages.

## Evidence
Fixture wellarchitected-custom-lens-import-version-lifecycle, step task#8, operation ImportLens.
Expected status 400, actual status 400 (status and error shape agree).
Divergent field $.Message (verdict mismatch):
  expected: '[Validation] Empty pillars list'
  actual:   '[Validation] [object has missing required properties (["pillars"])]'
Fixed text of AWS's message: the '[Validation] ' prefix plus a semantic defect phrase for the violated lens-document rule. Variable part: that defect phrase, selected from the submitted JSONString's actual violation. The emulator substitutes its JSON-Schema library's internal failure record and additionally mis-classifies the violation as an absent member rather than an empty list.
Smithy citations: ImportLens.JSONString -> LensJSON @required @length(min 2, max 500000); error ValidationException (client, httpError 400) with Message -> ExceptionMessage @required, Reason -> ValidationExceptionReason, Fields -> ValidationExceptionFieldList.

## Steps to Reproduce
1. Replay fixture wellarchitected-custom-lens-import-version-lifecycle against the emulator and stop at step task#8.
2. That step calls ImportLens (PUT /importLens) with a custom-lens JSONString whose pillars collection carries no pillars.
3. Observe the response: HTTP 400 with a ValidationException, as expected.
4. Compare $.Message against the recording: AWS returns '[Validation] Empty pillars list'; the emulator returns '[Validation] [object has missing required properties (["pillars"])]'.
5. The step is graded a mismatch on $.Message only; status and error shape pass.
