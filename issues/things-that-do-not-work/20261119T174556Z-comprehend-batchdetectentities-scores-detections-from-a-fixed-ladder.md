Issue-Source: trace-sweep-divergence
Issue-Trace: comprehend-batch-detect-suite
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: comprehend-batch-detect-suite
Issue-Shape-Id: com.amazonaws.comprehend#Comprehend_20171127
Issue-Key: comprehend|BatchDetectEntities|2xx->2xx|$.ResultList[*].Entities[*].Score
Issue-Commit: 401697ae9ee670bfd3238e900cda9afc11ec812d

# Comprehend BatchDetectEntities scores detections from a fixed ladder instead of per-detection confidence

> _Note: this issue was identified against the code base at commit `401697ae9ee670bfd3238e900cda9afc11ec812d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchDetectEntities

## Description
ONE net-new root cause in the comprehend crate's BatchDetectEntities entity scorer: the per-entity confidence returned in ResultList[*].Entities[*].Score is synthesized from the detector's own fixed value ladder rather than computed from the evidence for that particular detection, so the Score does not track how strongly the mention supports the reported entity Type. The step answers HTTP 200 exactly as recorded and every entity's Type, Text, BeginOffset and EndOffset match; only the Score members diverge.

Cause 1 of 1: BatchDetectEntities returns uncalibrated per-entity Score values. Fixture comprehend-batch-detect-suite, step task#2.
  AWS: Smithy (services/service-com-amazonaws-comprehend-comprehend-20171127): BatchDetectEntities(TextList required @length min 1, LanguageCode required) -> BatchDetectEntitiesResponse{ResultList required: list of BatchDetectEntitiesItemResult{Index, Entities: list of Entity}, ErrorList required}; errors BatchSizeLimitExceededException/InvalidRequestException/TextSizeLimitExceededException/UnsupportedLanguageException (400) and InternalServerException (500). Entity.Score is documented as "The level of confidence that Amazon Comprehend has in the accuracy of the detection." Recorded behavior: AWS returns a distinct Score per detected entity, each one a function of that mention's own evidence - two entities of the SAME document and the same Type come back with different confidences, and a weakly supported mention scores materially lower than an unambiguous one.
  Emulator: the Scores it returns for the same entities are not the recorded per-detection confidences but values from the handler's own small set of near-1.0 constants, and they do not preserve the recorded ordering between the two entities of the batch's third document: the entity AWS scored lower comes back HIGHER than the one AWS scored higher (the recorded pair 0.9917884469032288 / 0.9983819723129272 is returned as 0.9938162565231323 / 0.9962049126625061 - the relation between the two is inverted). The spans, Texts and Types are correct, so the defect is confined to the scorer.
  Why: a client consuming entity extraction gates and ranks on Score - it drops detections below a confidence threshold, picks the best candidate when two mentions overlap or compete, and surfaces only high-confidence entities for automated action. With confidences drawn from a fixed ladder the threshold no longer separates strong from weak detections (every mention looks near-certain), and because the relative order of two detections is not preserved, "take the highest-scoring entity" selects the wrong mention. No error path is involved: the response is a 200 whose numbers silently misrepresent the model's certainty.
  Fix: the BatchDetectEntities handler's entity scorer in the comprehend crate's synchronous text-analysis layer - derive each Entity.Score from the evidence for that individual detection (as DetectEntities must too, since the two share the detector), instead of assigning it from a per-shape constant table. The recorded floats below are this recording's values for its own input text; the requirement is the convention - one calibrated confidence per detection, distinct across detections and ordered by how strongly the text supports each one - not these literals.
  Traces: comprehend-batch-detect-suite#task#2.

NOTE on prior art: no OPEN issue owns this step (the replay reports owned_by empty for comprehend-batch-detect-suite#task#2). The open issue 20261030T173304Z-comprehend-batch-and-targeted-inference-re-segment-text.md covers the OTHER four steps of this fixture (task#1 BatchDetectDominantLanguage, task#3 BatchDetectKeyPhrases, task#4 BatchDetectSentiment, task#5 BatchDetectSyntax) plus DetectTargetedSentiment, and explicitly excludes task#2; the open 20261113T194011Z-comprehend-single-document-detect-inference-synthesizes-ladder-constant.md covers the single-document Detect* fixture. Several RESOLVED issues named BatchDetectEntities (20261030T053310Z, 20261103T080324Z, 20261108T024409Z, 20261119T064554Z) targeted this same scorer, but the step still diverges on replay, so this is filed net-new - a resolved issue never covers a live failure.

## Evidence
Fixture comprehend-batch-detect-suite, step task#2, operation BatchDetectEntities (three-document TextList, LanguageCode en). HTTP status: expected 200, got 200 - no status, error code or error message divergence; the divergence is entirely in the response body, and only in Score members (no Type/Text/BeginOffset/EndOffset key is reported):

  $.ResultList[2].Entities[0].Score: expected 0.9917884469032288, got 0.9938162565231323
  $.ResultList[2].Entities[1].Score: expected 0.9983819723129272, got 0.9962049126625061

The two recorded values for document 2 are ordered Entities[0] < Entities[1]; the emulator returns Entities[0] > Entities[1], i.e. the relative confidence of the two detections is inverted, which is what rules out a mere floating-point/rounding difference and points at a synthesized value rather than a computed one.

## Steps to Reproduce
1. Boot the emulator from the checkout and replay the wire corpus (replay_wire_corpus). The step comprehend-batch-detect-suite#task#2 is neither blocked by a stub nor owned by an open issue.
2. Issue BatchDetectEntities (X-Amz-Target Comprehend_20171127.BatchDetectEntities) with LanguageCode 'en' and the fixture's three-document TextList, the third document containing two entity mentions of differing strength.
3. Compare ResultList[2].Entities[*].Score against the recording: the emulator's values are not the recorded per-detection confidences, and their ordering between Entities[0] and Entities[1] is reversed relative to the recording.
4. Note that Type, Text, BeginOffset and EndOffset for the same entities match the recording, so only the scorer is implicated.
