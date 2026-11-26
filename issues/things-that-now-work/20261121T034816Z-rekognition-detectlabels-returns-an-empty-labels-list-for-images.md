Issue-Source: trace-sweep-divergence
Issue-Trace: rekognition-image-analysis-detect-operations
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rekognition-image-analysis-detect-operations rekognition-refarch-rekreup-detect-labels-s3object-mistakes
Issue-Shape-Id: com.amazonaws.rekognition#RekognitionService
Issue-Key: rekognition|DetectLabels|2xx->2xx|$.Labels[*]
Issue-Commit: 3b48ac12c387b3298783034376ff37b10f64fe0b

# DetectLabels returns an empty Labels list for images AWS labels, for both inline Bytes and S3Object sources

> _Note: this issue was identified against the code base at commit `3b48ac12c387b3298783034376ff37b10f64fe0b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectLabels

## Description
Two net-new causes (Cause 1 of 2, Cause 2 of 2). Both are the same fault seen on the two image-source paths DetectLabels accepts: the operation answers HTTP 200 with an EMPTY `Labels` list for an image whose contents AWS reports as general-label taxonomy entities. An open issue, 20261120T143926Z-dynamodb-lambda-rekognition-s3-detectlabels-on-an-s3-sourced-image-returns-a.md, reports the related complaint that the answer carries ONE colour-family label instead of taxonomy entities; it does not cover these two wire steps (the replay reports no owner for either, and here `Labels` is empty rather than wrong), so both are filed here and that issue is named for the fixer.

Cause 1 of 2 — inline `Image.Bytes`: fixture rekognition-image-analysis-detect-operations, step task#3.
AWS: "Detects instances of real-world entities within an image (JPEG or PNG) provided as input... For each object, scene, and concept the API returns one or more labels", and "DetectLabels returns a hierarchical taxonomy of detected labels... The response includes the all ancestors for a label, where every ancestor is a unique label" (Smithy: DetectLabels docs; output DetectLabelsResponse.Labels -> Labels (list), LabelModelVersion (String); input Image (Image, required), MaxLabels (UInteger, range 0..), MinConfidence (Percent, range 0..100), Features (DetectLabelsFeatureList, len 0..2)). Each Label member carries Name, Confidence, Parents, Aliases, Categories and Instances (Instances only for common object labels, each with a BoundingBox and its own Confidence). For the submitted image the recorded AWS response is 200 with a NON-EMPTY Labels list of taxonomy entities, with ancestors present as their own unique labels and categories populated — the recording shows entity names with their Parents/Categories (e.g. a face/head entity whose Parents are the head/person ancestors, an apparel entity carrying an Instances entry with a BoundingBox, outdoor/animal entities with their categories). Status 200 on both sides; no modeled error is involved, so this is a response-content divergence rather than a refusal.
Emulator: the same request answers 200 but with no label at all — the replay reports `$.Labels[0]: missing (... got None)` for every expected index 0..9, i.e. `Labels` is serialized as an empty list. Nothing about Name, Confidence, Parents, Aliases, Categories or Instances can therefore be observed, and no ancestor label is emitted.
Why: a client that calls DetectLabels to classify image content receives an answer that says the image contains nothing, for an image AWS labels. Content-based routing, moderation gating and "did the pipeline see an object" branches all take the empty-result path, and a client exercising MaxLabels as a cap or MinConfidence as a monotone filter cannot distinguish a working implementation from one that measures nothing.
Fix: the measurement and its vocabulary in the rekognition crate's label-detection state — `crate::state::label_detection::analysed_submitted_image` and the taxonomy/ancestor expansion in `crate::state::label_taxonomy` — not the DetectLabels handler's narrowing path, which faithfully narrows whatever label set it is handed (`label_detection::narrowed(...)`). The handler's own documented rule, "an image carrying no entity this process can measure is answered with an EMPTY Labels list", is only correct if the measurement actually recognizes the entities AWS recognizes; today it recognizes essentially none, so the empty list is reached for ordinary photographic input.
Traces: rekognition-image-analysis-detect-operations#task#3.

Cause 2 of 2 — `Image.S3Object` source: fixture rekognition-refarch-rekreup-detect-labels-s3object-mistakes, steps task#5 and task#6.
AWS: same operation contract as above — "You pass the input image as base64-encoded image bytes or as a reference to an image in an Amazon S3 bucket", and an image read out of S3 is labelled identically to the same bytes submitted inline (Smithy: DetectLabels, Image.S3Object). The recorded response is 200 with at least one Label present, shape-asserted only: a Name, a Confidence, Categories with a category Name, and the Aliases/Instances/Parents lists present (the fixture asserts the SHAPE, marking the concrete name/confidence as not-asserted, so the requirement is that a label exists and is fully shaped, not any particular name or score).
Emulator: 200 with `$.Labels[0]: missing (... got None)` — the S3-sourced path likewise yields an empty `Labels` list, so not even one shaped label is returned. The same divergence repeats on the fixture's second DetectLabels step (task#6), so it is not specific to one object or one set of narrowing parameters.
Why: the dominant real-world usage of DetectLabels is on objects already in S3 (the reference architecture this fixture mirrors), and those clients see every image as unlabelled. Any downstream step keyed on a label name, a Confidence threshold or an Instances BoundingBox never fires.
Fix: same component as Cause 1 — the label measurement/taxonomy state used after the image is fetched through `crate::state::image_source`; the S3 fetch itself succeeds (the step returns 200, not InvalidS3ObjectException), so the gap is in what is measured from the decoded frame, not in sourcing it.
Traces: rekognition-refarch-rekreup-detect-labels-s3object-mistakes#task#5 and #task#6.

## Evidence
Cause 1 (fixture rekognition-image-analysis-detect-operations, step task#3), DetectLabels with an inline image:
  Expected (recorded AWS): HTTP 200, Labels non-empty — indices 0..9+ each a shaped taxonomy label, e.g. $.Labels[0] = {Name: <entity>, Confidence: <float>, Categories: [{Name: <category>}], Parents: [{Name: <ancestor>}, ...], Aliases: [], Instances: [...]} with ancestors appearing as their own unique labels and common-object labels carrying an Instances entry {BoundingBox:{Height,Left,Top,Width}, Confidence}.
  Actual (emulator): HTTP 200, but $.Labels[0] .. $.Labels[9] all missing — replay detail: "$.Labels[0]: missing (expected {...'Name': 'Face'...}, got None)" through "$.Labels[9]". Labels is an empty list.

Cause 2 (fixture rekognition-refarch-rekreup-detect-labels-s3object-mistakes, steps task#5 and task#6), DetectLabels with Image.S3Object:
  Expected (recorded AWS): HTTP 200 with at least $.Labels[0] present and fully shaped — {Aliases: [], Categories: [{Name: <shaped-not-asserted>}], Confidence: <shaped-not-asserted>, Instances: [], Name: <shaped-not-asserted>, Parents: []}.
  Actual (emulator): HTTP 200, $.Labels[0]: missing (got None) — empty Labels, on both DetectLabels steps of the fixture.

Fixed text vs variable parts: no error message is involved — both sides answer HTTP 200 and no error code is returned, so there is no message template to match. The requirement is on response CONTENT: Labels must be non-empty for an image AWS labels; each entry must carry Name, Confidence, Categories, Parents, Aliases and (for common objects) Instances with a BoundingBox; ancestors must appear as unique labels; LabelModelVersion must accompany a run of label detection. The specific names, confidences and bounding-box coordinates in any one recording ('Face', 83.1154..., 'Hardhat', the Left/Top/Width/Height floats) are per-run values that fill the template and are NOT part of the requirement.

## Steps to Reproduce
1. Inline bytes: call rekognition.detect_labels(Image={'Bytes': <a JPEG/PNG photograph containing recognizable entities>}) and observe HTTP 200 with Labels = [] instead of one or more shaped taxonomy labels. Equivalent: re-replay fixture rekognition-image-analysis-detect-operations, step task#3.
2. S3 source: PUT an image object into this emulator's S3, then call rekognition.detect_labels(Image={'S3Object': {'Bucket': <bucket>, 'Name': <key>}}) and observe HTTP 200 with Labels = [] — the S3 read succeeds (no InvalidS3ObjectException), yet no label is reported. Equivalent: re-replay fixture rekognition-refarch-rekreup-detect-labels-s3object-mistakes, steps task#5 and task#6.
3. Confirm the fix by re-replaying all three steps above; each must return at least one fully shaped label (Name, Confidence, Categories, Parents, Aliases, Instances) plus LabelModelVersion.
