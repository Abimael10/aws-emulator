Issue-Source: trace-sweep-divergence
Issue-Trace: comprehend-batch-detect-suite
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: comprehend-batch-detect-suite
Issue-Shape-Id: com.amazonaws.comprehend#Comprehend_20171127
Issue-Key: comprehend|BatchDetectEntities|2xx->2xx|$.ResultList[*].Entities[*].Score
Issue-Commit: e99960bc24a06e08ddf96250dc369db8427b79f1

# Comprehend BatchDetectEntities emits uncalibrated per-entity confidence scores shared with DetectEntities

> _Note: this issue was identified against the code base at commit `e99960bc24a06e08ddf96250dc369db8427b79f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchDetectEntities

## Description
ONE root cause in the comprehend crate's synchronous entity-recognition scorer, reached through the BatchDetectEntities fan-out. The operation answers HTTP 200 with the correct entity set, spans and types; the divergence is confined to Entity.Score, which the emulator synthesizes from a shared table of constants rather than computing a per-detection confidence. Because each ResultList entry is per-document, the cause reproduces for every document of the batch whose entities are scored. A fixer must close this ONE cause.

NOTE on prior art: three earlier issues on this operation's scoring are RESOLVED (20261030T053310Z-comprehend-batchdetectentities-truncates-entity-spans-and-emits-bucketed.md, 20261103T080324Z-comprehend-batchdetectentities-scores-detections-from-a-shared-constant.md and 20261108T024409Z-comprehend-batchdetectentities-returns-uncalibrated-per-entity-confidence-scores.md), and the step is not owned by any open issue on replay, so this is filed net-new (effectively a reopen: the span/type truncation those issues describe is fixed, the confidence calibration is not). The open issue 20261030T173304Z-comprehend-batch-and-targeted-inference-re-segment-text.md carries the other four BatchDetect* steps of this fixture but explicitly NOT task#2, and 20261113T194011Z-comprehend-single-document-detect-inference-synthesizes-ladder-constant.md carries the single-document DetectEntities step, not this one.

Cause 1 of 1: BatchDetectEntities returns Entity.Score values drawn from a constant table instead of a per-detection confidence. Fixture comprehend-batch-detect-suite, step task#2.
  AWS: Smithy: BatchDetectEntities(TextList required, @length min 1, sensitive; LanguageCode required) -> ResultList: ListOfDetectEntitiesResult (required) + ErrorList: BatchItemErrorList (required); errors BatchSizeLimitExceededException/InvalidRequestException/TextSizeLimitExceededException/UnsupportedLanguageException (400) and InternalServerException (500). Entity.Score is documented as "The level of confidence that Amazon Comprehend has in the accuracy of the detection." Recorded behavior: every detected entity carries its OWN confidence, derived from the evidence for that mention in that document, so distinct detections within a document — and the same surface form in different documents — carry distinct values, and the value tracks how unambiguous the mention is.
  Emulator: Entity.Score is assigned from a small fixed set of values that recurs across detections and across operations rather than being computed per detection. Two different entities of the same document come back with values from that set in place of their two distinct recorded confidences, and one of the very same values the emulator returns here is also returned for an unrelated entity of the single-document DetectEntities step of this corpus — evidence the number is a property of the scorer's constant table, not of the detection. The entity set, Type, BeginOffset and EndOffset all match the recording; only Score diverges, and the HTTP status is 200 as recorded.
  Why: Score is the only signal a client has for deciding whether to trust a detection. Clients threshold on it ("only persist entities above 0.99"), rank by it to pick the best of several overlapping candidates, and route low-confidence mentions to human review. When the value comes from a constant table, the ordering between detections is meaningless, a weak detection can outrank a strong one, and the low-confidence branch of a client's logic is either permanently taken or permanently unreachable depending on where the constants happen to fall relative to the client's threshold — all while the response looks entirely well-formed.
  Fix: the BatchDetectEntities handler's entity scorer in the comprehend crate's shared synchronous text-analysis layer — compute one confidence per detection from that detection's own evidence, instead of reading it from a per-shape or per-type constant table. The same scorer backs the single-document DetectEntities path, so the fix belongs in the shared scoring component rather than in the batch fan-out.
  Traces: comprehend-batch-detect-suite#task#2.

The recorded numbers below are this recording's values for its own input text. The requirement is the convention — one confidence per detection, derived from that detection's evidence and therefore distinct between distinct detections — not these literals.

## Evidence
Fixture comprehend-batch-detect-suite, step task#2, BatchDetectEntities (multi-document TextList, LanguageCode en). HTTP 200 expected, HTTP 200 returned; the divergence is entirely in the response body, and only in Entity.Score:

  $.ResultList[2].Entities[0].Score: expected 0.9917884469032288, got 0.9938162565231323
  $.ResultList[2].Entities[1].Score: expected 0.9983819723129272, got 0.9962049126625061

The two recorded values are distinct per-detection confidences. The value the emulator returns for ResultList[2].Entities[1] (0.9962049126625061) is the SAME value it returns for $.Entities[0].Score in the DetectEntities step of fixture comprehend-detect-suite-single-document (task#2, where the recording has 0.9613922834396362) — a different operation, a different document and a different entity. A confidence that is reproduced byte-for-byte across unrelated detections is a constant of the scorer, not a measurement of the detection.

No other member of the response diverges: Entity Text, Type, BeginOffset and EndOffset, the ResultList Index values and the ErrorList all match the recording, so the recognizer itself is correct and the defect is isolated to the confidence computation.

## Steps to Reproduce
1. Boot the emulator from the checkout and replay the wire corpus (replay_wire_corpus). Confirm the step is live: comprehend-batch-detect-suite#task#2 reports stub_path empty and owned_by empty, so it is neither blocked by an unimplemented operation nor owned by an open issue.
2. Call BatchDetectEntities with LanguageCode 'en' and the fixture's multi-document TextList, where the third document contains two named entities whose mentions carry different amounts of supporting context.
3. Inspect $.ResultList[2].Entities[*].Score. Both values come back from the emulator's constant set instead of the two distinct recorded confidences (expected 0.9917884469032288 and 0.9983819723129272).
4. Confirm the values are scorer constants rather than per-detection measurements: replay step task#2 of fixture comprehend-detect-suite-single-document (DetectEntities) and observe $.Entities[0].Score return the identical 0.9962049126625061 for an unrelated entity in a different document.
5. Verify the rest of the body matches: Text, Type, BeginOffset, EndOffset, Index and ErrorList all agree with the recording, and the status is 200 — only Score diverges.
