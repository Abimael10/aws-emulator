Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda__rekognition__s3.detect-labels-on-an-s3-object-narrows-by-maxlabels-and-minconfidence
Issue-Key: dynamodb__lambda__rekognition__s3.detect-labels-on-an-s3-object-narrows-by-maxlabels-and-minconfidence|run#8|AssertionError|assert len(labels) >= 2, (
Issue-Commit: 0fdfae3b09ad57be4aaac580935b8ba339068631

# DetectLabels on an S3-sourced image returns a single colour-name label instead of the real-world entity labels AWS reports

> _Note: this issue was identified against the code base at commit `0fdfae3b09ad57be4aaac580935b8ba339068631`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectLabels

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: DetectLabels answers an S3-sourced image with a label set that is too small and drawn from the wrong vocabulary, so the documented MaxLabels/MinConfidence narrowing relations cannot be observed at all. Failing check id: dynamodb__lambda__rekognition__s3.detect-labels-on-an-s3-object-narrows-by-maxlabels-and-minconfidence.

AWS: DetectLabels "detects instances of real-world entities within an image (JPEG or PNG)" and "for each object, scene, and concept the API returns one or more labels", returning for each one Name, Confidence, Parents, Aliases and Categories, and "a hierarchical taxonomy of detected labels ... The response includes the all ancestors for a label, where every ancestor is a unique label" (Smithy: DetectLabels docs; output Labels (Labels), LabelModelVersion (String); input MaxLabels (UInteger, range 0..) "returns the specified number of highest confidence labels", MinConfidence (Percent, range 0..100), default 55 percent). For the image this check submits (a 64x64 solid RGB(200,30,30) JPEG), the recorded AWS response carries at least two labels whose Confidence is >= 70, which is what makes the two documented narrowing relations observable: MaxLabels=1 returns exactly the highest-confidence label of the wider answer, and raising MinConfidence to 95 returns exactly the subset of the SAME scored labels that clear 95 (no re-scoring, no new names). Status 200 in both the recording and the emulator; no error code is involved — this is a response-content divergence, not a refusal.

Emulator: for the same bytes read out of this emulator's S3, DetectLabels(MaxLabels=10, MinConfidence=70) answers 200 with exactly ONE label, [('Red', 100.0)]. The handler is stateless and delegates the whole answer to crate::state::label_detection, whose entity vocabulary is only FACE/HEAD/PERSON (derived from the geometric face detector) plus ONE simplified dominant-colour family name taken from DominantColor.SimplifiedColor. 'Red' is a simplified colour name of the ImageProperties side of the model, not a general-label taxonomy entry, and label_detection deliberately declines to report any second label for a frame of essentially one colour ("a frame of essentially one colour is not reported a SECOND time under a composition name"). With one label in the answer every narrowing assertion is either vacuous or unobservable, and no parent/ancestor label is ever emitted for a measured entity even though the operation documents that all ancestors are returned as unique labels.

Why: a client that reads DetectLabels to classify image content gets a near-empty answer for an image AWS labels, and a client that exercises MaxLabels as a hard cap or MinConfidence as a monotone filter over a stable score set cannot distinguish a correct implementation from one that returns a single synthetic label. Pipelines that branch on label names see a colour-family name that the AWS general-label taxonomy does not use as a detected entity, so name-based routing never matches.

Fix: the label vocabulary and its taxonomy in crate::state::label_detection (and the ancestor/category expansion it calls in crate::state::label_taxonomy) for the rekognition crate; the DetectLabels handler's narrowing call path itself (narrowed(labels, settings, min_confidence, max_labels)) is only as good as the label set handed to it. The answer must report more than one label for an image AWS labels with more than one, must emit each label's ancestors as unique labels, and must not surface ImageProperties simplified colour-family names as general-label entities.

Traces: dynamodb__lambda__rekognition__s3.detect-labels-on-an-s3-object-narrows-by-maxlabels-and-minconfidence (mismatch: "the seeded image must yield at least two labels at MinConfidence 70 for the narrowing relations to be observable; DetectLabels returned [('Red', 100.0)]"). Handler: services/service-com-amazonaws-rekognition-rekognitionservice/src/ops/detect_labels.rs; measurement: services/service-com-amazonaws-rekognition-rekognitionservice/src/state/label_detection.rs.

Note on service attribution: the suite lists this check under the compound heading `dynamodb__lambda__rekognition__s3`; the diverging operation is Amazon Rekognition's DetectLabels, served by services/service-com-amazonaws-rekognition-rekognitionservice.

## Evidence
Expected (recorded AWS, same JPEG bytes, same request):
  DetectLabels(Image={S3Object:{Bucket:<b>, Name:'alpha.jpg'}}, MaxLabels=10, MinConfidence=70) -> 200 with Labels containing >= 2 entries, each Confidence >= 70, each Name a general-label taxonomy entity with its Parents/Categories populated, plus LabelModelVersion.
  DetectLabels(..., MaxLabels=1, MinConfidence=70) -> 200 with exactly the single highest-confidence label of the wide answer.
  DetectLabels(..., MaxLabels=10, MinConfidence=95) -> 200 with exactly the wide answer's labels whose (unchanged) Confidence >= 95.

Actual (emulator):
  DetectLabels(..., MaxLabels=10, MinConfidence=70) -> 200 with Labels = [{Name:'Red', Confidence:100.0}] — one label only, and its Name is a DominantColor.SimplifiedColor colour-family name rather than a detected real-world entity; no ancestor labels accompany it.
  Assertion raised by the check: "the seeded image must yield at least two labels at MinConfidence 70 for the narrowing relations to be observable; DetectLabels returned [('Red', 100.0)]".

Fixed text vs variable parts: no error message is involved (both sides are HTTP 200). The requirement is on the response content — label cardinality (>1 for an image AWS labels), label names drawn from the general-label taxonomy, ancestors present as unique labels, and the same Confidence values reused when MinConfidence is raised. The specific names/confidences in one recording ('Red', 100.0) are run values, not part of the requirement.

## Steps to Reproduce
1. Create a bucket and PUT the check's seeded object: s3.put_object(Bucket=<bucket>, Key='alpha.jpg', Body=<64x64 solid RGB(200,30,30) JPEG, quality 90>, ContentType='image/jpeg').
2. rekognition.detect_labels(Image={'S3Object': {'Bucket': <bucket>, 'Name': 'alpha.jpg'}}, MaxLabels=10, MinConfidence=70) -> observe a single label [('Red', 100.0)] instead of two or more taxonomy labels.
3. rekognition.detect_labels(..., MaxLabels=1, MinConfidence=70) and rekognition.detect_labels(..., MaxLabels=10, MinConfidence=95) -> with only one label in the wide answer the MaxLabels cap and the MinConfidence monotone-filter relations are unobservable.
4. Equivalent: re-replay check dynamodb__lambda__rekognition__s3.detect-labels-on-an-s3-object-narrows-by-maxlabels-and-minconfidence.
