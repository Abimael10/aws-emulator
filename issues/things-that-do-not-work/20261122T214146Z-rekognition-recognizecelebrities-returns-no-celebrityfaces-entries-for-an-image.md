Issue-Source: trace-sweep-divergence
Issue-Trace: rekognition-celebrity-recognition-and-lookup
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rekognition-celebrity-recognition-and-lookup
Issue-Shape-Id: com.amazonaws.rekognition#RekognitionService
Issue-Key: rekognition|RecognizeCelebrities|2xx->2xx|$.CelebrityFaces[*]
Issue-Commit: 667711c55307ac8bb9b0b208bd1f622cac293819

# RecognizeCelebrities returns no CelebrityFaces entries for an image containing a recognizable celebrity

> _Note: this issue was identified against the code base at commit `667711c55307ac8bb9b0b208bd1f622cac293819`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RecognizeCelebrities

## Description
One cause.

Cause 1 of 1: RecognizeCelebrities answers HTTP 200 but with no recognized celebrities at all, so CelebrityFaces is empty/absent where AWS populates it.

AWS: Smithy model services/service-com-amazonaws-rekognition-rekognitionservice/smithy-model/com-amazonaws-rekognition-rekognitionservice.json — RecognizeCelebrities(input RecognizeCelebritiesRequest{Image required}) -> RecognizeCelebritiesResponse{CelebrityFaces: CelebrityList, UnrecognizedFaces: ComparedFaceList, OrientationCorrection: OrientationCorrection}. CelebrityList members are the Celebrity structure, whose members are Id (RekognitionUniqueId, pattern ^[0-9A-Za-z]*$), Name (String, "The name of the celebrity"), Urls (Urls, @length 0..255, documented as "an array of URLs pointing to additional information about the celebrity"), Face (ComparedFace), MatchConfidence (Percent, @range 0..100) and KnownGender. The operation's modeled failure modes are AccessDeniedException, ImageTooLargeException, InvalidImageFormatException, InvalidParameterException, InvalidS3ObjectException, ProvisionedThroughputExceededException (400), InternalServerError and ThrottlingException (500) — i.e. for an image AWS can decode and in which it recognizes a celebrity, the modeled contract is a 200 whose CelebrityFaces carries one Celebrity per recognized face, each with at least Id and Name populated and Urls present (empty list when there is no extra information). Recorded behavior for this fixture step: a 200 whose CelebrityFaces[0] is a Celebrity with an Id, a Name, a KnownGender.Type and a one-element Urls list.

Emulator: the same request returns 200, but $.CelebrityFaces[0] is missing — the list is empty (or the member is omitted), so the response says the service recognized nobody. Replay detail: "$.CelebrityFaces[0]: missing (expected {'Id': ..., 'Name': ..., 'KnownGender': {'Type': ...}, 'Urls': [...]}, got None)". Note the specific id, name, gender and URL in the recording are that recording's values, not requirements: the requirement is that a recognized face yields a Celebrity element with a non-empty Name and an Id matching ^[0-9A-Za-z]*$, a Face/MatchConfidence consistent with the detected face, and Urls present as a (possibly empty) list.

Why: a client cannot distinguish "no celebrity in this image" from "recognition did not run", because both come back as 200 with an empty CelebrityFaces. Celebrity-recognition workflows branch on CelebrityFaces being non-empty — they look up the returned Id/Name, index it, tag the image, or drive a downstream step — so against the emulator every such branch takes the not-found path, downstream lookups receive no id or name, and any assertion that a known celebrity was found fails while the call itself reports success. Hypothesis: the operation is wired to a response builder that always emits an empty CelebrityFaces rather than to the recognition/fixture-backed inference path, which is the same class of defect already filed for DetectLabels in 20261121T034816Z-rekognition-detectlabels-returns-an-empty-labels-list-for-images.md (that issue is scoped to DetectLabels and does not cover RecognizeCelebrities, hence this separate filing).

Fix: the rekognition RecognizeCelebrities handler and its response shaping — it must derive recognized faces from the request's Image (inline Bytes or S3Object) via whatever recognition source the service's other detect/recognize operations use, and serialize each one into CelebrityFaces as a Celebrity with Id, Name, Urls, Face and MatchConfidence, leaving genuinely unmatched faces in UnrecognizedFaces, instead of emitting an empty CelebrityFaces unconditionally.

Traces: rekognition-celebrity-recognition-and-lookup#task#2 (RecognizeCelebrities, expected 200 / actual 200, key $.CelebrityFaces[0]).

## Evidence
Fixture rekognition-celebrity-recognition-and-lookup, step task#2, operation RecognizeCelebrities.
Expected status 200, actual status 200 — the divergence is in the body.
Expected: $.CelebrityFaces[0] present, a Celebrity structure carrying Id (matching ^[0-9A-Za-z]*$), Name (the celebrity's name), KnownGender.Type and Urls (list of informational URLs). Recorded example value: {'Id': '4rr8sP5H', 'Name': 'Rob Nabors', 'KnownGender': {'Type': 'Male'}, 'Urls': ['www.wikidata.org/wiki/Q7340410']} — the id, name, gender and URL are that recording's values; the requirement is the presence and shape of the element.
Actual: $.CelebrityFaces[0] missing (got None) — CelebrityFaces empty or absent, so the response reports zero recognized celebrities.
Cluster: 1 affected step (rekognition-celebrity-recognition-and-lookup#task#2). verdict=mismatch, divergence_key=service=rekognition|RecognizeCelebrities|status=200->200|keys=$.CelebrityFaces[0]. No stub_path and no owning issue.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay fixture rekognition-celebrity-recognition-and-lookup up to step task#2, which calls rekognition RecognizeCelebrities with an Image containing a face AWS recognizes as a celebrity.
3. Observe the response is HTTP 200 but CelebrityFaces is empty/absent, so $.CelebrityFaces[0] does not exist.
4. Expected: 200 with CelebrityFaces[0] a Celebrity carrying a populated Name, an Id matching ^[0-9A-Za-z]*$, a Urls list and face/match-confidence data for the recognized face.
