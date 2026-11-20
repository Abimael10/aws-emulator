Issue-Source: trace-sweep-divergence
Issue-Trace: rekognition-image-analysis-detect-operations
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rekognition-image-analysis-detect-operations
Issue-Shape-Id: com.amazonaws.rekognition#RekognitionService
Issue-Key: rekognition|DetectLabels|2xx->2xx|$.Labels[*]
Issue-Commit: feea67bf354c9355f6d85247ae1f2e70f0f7b2a1

# DetectLabels returns dominant-colour family names instead of the general-label taxonomy entities AWS detects

> _Note: this issue was identified against the code base at commit `feea67bf354c9355f6d85247ae1f2e70f0f7b2a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectLabels

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: DetectLabels answers HTTP 200 with a Labels array drawn from the IMAGE_PROPERTIES dominant-colour vocabulary rather than the GENERAL_LABELS real-world-entity taxonomy, and with far fewer entries than AWS returns for the same image.

AWS: DetectLabels "detects instances of real-world entities within an image (JPEG or PNG) provided as input. This includes objects like flower, tree, and table; events like wedding, graduation, and birthday party; and concepts like landscape, evening, and nature", and "for each object, scene, and concept the API returns one or more labels" with Name, Confidence, Parents, Aliases, Categories and (for common objects) Instances[].BoundingBox. "DetectLabels returns a hierarchical taxonomy of detected labels ... The response includes the all ancestors for a label, where every ancestor is a unique label." (Smithy: DetectLabels docs; DetectLabelsRequest.Image required, MaxLabels (UInteger @range min 0) default/upper limit 1000, MinConfidence (Percent @range 0..100) default 55; DetectLabelsResponse.Labels (Labels), LabelModelVersion, ImageProperties.) The recorded response for this step is 200 with a multi-entry Labels list whose entries are taxonomy entities with populated Parents and Categories from the general-label taxonomy — e.g. an entity label with Categories [{Name: 'Person Description'}] and Parents [{Name: 'Head'}, {Name: 'Person'}], a second entity label that is that one's ancestor emitted as its own unique label, an apparel entity carrying an Instances[] entry with a BoundingBox and a per-instance Confidence, and further entities in the nature/animal/hobby categories. Status 200 on both sides and no error code is involved: this is a response-content divergence, not a refusal.

Emulator: for the same request the emulator answers 200 but Labels contains only dominant-colour family names with empty Parents and the Categories value 'Colors and Visual Composition' — the first two entries are colour-family names at confidences of 100.0 and ~75 and every remaining expected index is absent (got None). No general-label entity is ever emitted, no ancestor label is emitted as a unique label, and no Instances[]/BoundingBox is produced for a common-object label, so the whole GENERAL_LABELS side of the answer is missing and only the IMAGE_PROPERTIES colour measurement is surfaced as if it were a detected entity.

Why: a client that reads DetectLabels to classify image content gets colour names where AWS returns objects, scenes and concepts, so name-based routing and category filters never match; a client that walks Parents to roll a detection up its taxonomy finds the hierarchy empty; and a client that localises a detected object through Instances[].BoundingBox gets nothing to draw. Settings-based inclusive/exclusive label and category filtering is also untestable because no label in the answer belongs to a filterable general-label category.

Fix: the label vocabulary and its ancestor/category expansion in the rekognition crate's label-detection state (the component the DetectLabels handler delegates its answer to), plus the handler's GENERAL_LABELS path so that colour-family names from the ImageProperties dominant-colour measurement are no longer surfaced as general labels. The answer must report one or more taxonomy entity labels per detected object/scene/concept, emit every ancestor as its own unique label with Parents/Categories populated, and attach Instances[].BoundingBox for common-object labels, before MaxLabels/MinConfidence narrowing is applied.

Related open issue (does NOT cover this wire step; it carries a behavioral check key, not this fixture's replay key): 20261120T143926Z-dynamodb-lambda-rekognition-s3-detectlabels-on-an-s3-sourced-image-returns-a.md, which reports the same vocabulary defect from the behavioral suite for an S3-sourced solid-colour JPEG.

Traces: rekognition-image-analysis-detect-operations#task#3 (mismatch at $.Labels[0]..$.Labels[9]).

## Evidence
Fixture rekognition-image-analysis-detect-operations, step task#3, operation DetectLabels.

Expected (recorded AWS): HTTP 200 with Labels[] holding general-label taxonomy entities, e.g.
  $.Labels[0] = {Name: 'Face', Confidence: 83.1154, Parents: [{Name: 'Head'}, {Name: 'Person'}], Categories: [{Name: 'Person Description'}], Aliases: [], Instances: []}
  $.Labels[1] = {Name: 'Head', Confidence: 83.1154, Parents: [{Name: 'Person'}], Categories: [{Name: 'Person Description'}], ...}   (an ancestor emitted as its own unique label)
  $.Labels[2] = {Name: 'Hardhat', Confidence: 67.8628, Parents: [{Name: 'Clothing'}, {Name: 'Helmet'}], Categories: [{Name: 'Apparel and Accessories'}], Instances: [{BoundingBox: {...}, Confidence: 67.8628}]}
  $.Labels[3..9] = further entities in categories 'Nature and Outdoors', 'Hobbies and Interests', 'Animals and Pets' (e.g. a scene label and an animal label with its 'Animal' parent).

Actual (emulator): HTTP 200 with
  $.Labels[0] = {Name: 'White', Confidence: 100.0, Parents: [], Aliases: [], Instances: [], Categories: [{Name: 'Colors and Visual Composition'}]}
  $.Labels[1] = {Name: 'Black', Confidence: 74.9793, Parents: [], Aliases: [], Instances: [], Categories: [{Name: 'Colors and Visual Composition'}]}
  $.Labels[2] .. $.Labels[9] = absent (None).

No error message is involved (both sides are 200). The requirement is on response content: label names drawn from the general-label taxonomy, one or more labels per detected object/scene/concept, every ancestor present as a unique label with Parents/Categories populated, and Instances[].BoundingBox for common-object labels. The specific names, confidences and bounding-box coordinates in the recording ('Face', 83.1154, 'Hardhat', the box values) are per-run values that fill the template, not part of it.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Replay fixture rekognition-image-analysis-detect-operations and stop at step task#3 (DetectLabels on the fixture's image).
3. Inspect the response body: Labels[] contains only colour-family names ('White', 'Black') with Categories 'Colors and Visual Composition', empty Parents and no Instances, instead of the recorded general-label entities ('Face', 'Head', 'Hardhat', ...).
4. Equivalent: re-replay the whole wire corpus and observe the mismatch cluster keyed service=rekognition|DetectLabels|status=200->200|keys=$.Labels[0]..$.Labels[9].
