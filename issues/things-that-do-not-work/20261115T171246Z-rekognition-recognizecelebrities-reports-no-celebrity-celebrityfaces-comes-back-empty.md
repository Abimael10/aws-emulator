Issue-Source: trace-sweep-divergence
Issue-Trace: rekognition-celebrity-recognition-and-lookup
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rekognition-celebrity-recognition-and-lookup
Issue-Shape-Id: com.amazonaws.rekognition#RekognitionService
Issue-Key: rekognition|RecognizeCelebrities|2xx->2xx|$.CelebrityFaces[*]
Issue-Commit: 860eb666e989875e1daa85fefb4b7f7b73b30cd3

# RecognizeCelebrities reports no celebrity: CelebrityFaces comes back empty for an image whose face AWS recognizes

> _Note: this issue was identified against the code base at commit `860eb666e989875e1daa85fefb4b7f7b73b30cd3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RecognizeCelebrities

## Description
ONE net-new cause in rekognition RecognizeCelebrities. Count: 1 cause — Cause 1 of 1 below; it must be closed before the fix is complete.

--- Cause 1 of 1: the recognized side of the CelebrityFaces/UnrecognizedFaces partition is always empty, so no face is ever reported as a celebrity ---
AWS: Smithy (RecognizeCelebrities, com.amazonaws.rekognition, model services/service-com-amazonaws-rekognition-rekognitionservice/smithy-model/com-amazonaws-rekognition-rekognitionservice.json): input {Image (required)}; output {CelebrityFaces: list<Celebrity>, UnrecognizedFaces: list<ComparedFace>, OrientationCorrection}. Shape Celebrity carries Id (RekognitionUniqueId, @pattern ^[0-9A-Za-z]*$), Name (String), Urls (Urls, @length 0..255), Face (ComparedFace), MatchConfidence (Percent, @range 0..100) and KnownGender (KnownGender{Type: KnownGenderType, one of Male|Female|Nonbinary|Unlisted}). The operation's own documentation: it "lists the recognized celebrities in the CelebrityFaces array and any unrecognized faces in the UnrecognizedFaces array", and "for each celebrity recognized, RecognizeCelebrities returns a Celebrity object" containing "the celebrity name, ID, URL links to additional information, match confidence, and a ComparedFace object". Recorded behavior for this request: HTTP 200 whose CelebrityFaces list has at least one element, and that element is a populated Celebrity — an Id matching the RekognitionUniqueId pattern, a Name, a KnownGender whose Type is one of the modeled values, and a Urls list of reference links. The particular identity in the recording (Id '4rr8sP5H', Name 'Rob Nabors', KnownGender.Type 'Male', Urls ['www.wikidata.org/wiki/Q7340410']) is data from AWS's own global celebrity catalogue — a per-recording value, NOT part of the requirement. The requirement is structural and positional: a face this service recognizes must be reported on the CelebrityFaces side of the partition as a Celebrity carrying those members, with its measured ComparedFace and a MatchConfidence in 0..100. No error is involved: the status is 200 on both sides, so there is no error code or message to match here.
Emulator: HTTP 200, but CelebrityFaces is empty — $.CelebrityFaces[0] is missing (expected a Celebrity object, got None). Every measured face falls on the unrecognized side of the partition, so the recognized list is never non-empty. The handler at services/service-com-amazonaws-rekognition-rekognitionservice/src/ops/recognize_celebrities.rs builds the partition from crate::state::celebrity_catalogue::CelebrityCatalogue::identify, which admits a match only at or above MINIMUM_RESEMBLANCE = 25.0 against the seeded reference portraits (src/state/celebrity_catalogue.rs); Hypothesis: for a photographic face no seeded synthetic reference portrait correlates above that floor (or the detector measures no face at all for this image), so identify() returns None for every face and CelebrityFaces is emitted as [].
Why: a client that calls RecognizeCelebrities to learn who is in an image gets an unconditional "nobody" with a 200 — indistinguishable from "analysed, no celebrity present". Every celebrity-gated branch (tagging a photo with a person, routing by KnownGender, following Celebrity.Id into GetCelebrityInfo) takes the empty path, and the documented RecognizeCelebrities -> Celebrity.Id -> GetCelebrityInfo chain can never be exercised because no Id is ever handed out. Code written against AWS's behavior is never run by the emulator, and the failure is silent.
Fix: the celebrity resolution this operation reads — crate::state::celebrity_catalogue (the seeded reference appearances and the MINIMUM_RESEMBLANCE admission floor) together with the partition the RecognizeCelebrities handler builds in crate::ops::recognize_celebrities, so that a face the service measures in a submitted image resolves to a catalogue entry and is emitted as a populated Celebrity in CelebrityFaces rather than always landing in UnrecognizedFaces.
Traces: rekognition-celebrity-recognition-and-lookup#task#2.

Relation to already-filed issues: the resolved issue 20261106T074501Z-rekognition-recognizecelebrities-always-answers-an-empty-celebrityfaces-array-so.md reported this same always-empty CelebrityFaces behavior and is marked FIXED, yet the divergence still reproduces at the current commit — so this is filed net-new rather than treated as covered. The open issue 20261111T040622Z-rekognition-detectlabels-diverges-label-shape-omits-aliases-categories-instances.md is a different operation (DetectLabels) and does not cover this, and 20261106T205525Z-rekognition-searchfacesbyimage-invalidparameterexception-message-is-generic-instead-of-naming.md covers SearchFacesByImage only.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, score 74.5%, 47 steps: 35 passed, 10 mismatched, 2 blocked, 6 divergence clusters). The one cluster not already owned by an open issue or blocked by a stub:

fixture rekognition-celebrity-recognition-and-lookup, step task#2 — RecognizeCelebrities, expected status 200 / actual status 200, verdict mismatch, divergence_key service=rekognition|RecognizeCelebrities|status=200->200|keys=$.CelebrityFaces[0]:
  $.CelebrityFaces[0]: missing (expected {'Id': '4rr8sP5H', 'Name': 'Rob Nabors', 'KnownGender': {'Type': 'Male'}, 'Urls': ['www.wikidata.org/wiki/Q7340410']}, got None)

Expected vs actual, as a requirement: expected CelebrityFaces to carry at least one Celebrity element populated with Id (@pattern ^[0-9A-Za-z]*$), Name, KnownGender.Type (Male|Female|Nonbinary|Unlisted) and Urls; actual CelebrityFaces is an empty list, so index 0 does not exist. The literal Id/Name/Urls above are AWS catalogue values carried by the recording and are not themselves asserted as a requirement.

Model evidence (smithy_query, model_path services/service-com-amazonaws-rekognition-rekognitionservice/smithy-model/com-amazonaws-rekognition-rekognitionservice.json):
  operation RecognizeCelebrities: input {Image: Image (required)}; output {CelebrityFaces: CelebrityList (list), UnrecognizedFaces: ComparedFaceList (list), OrientationCorrection: enum}; errors AccessDeniedException, ImageTooLargeException, InternalServerError, InvalidImageFormatException, InvalidParameterException, InvalidS3ObjectException, ProvisionedThroughputExceededException, ThrottlingException.
  shape CelebrityList = list<Celebrity>; shape Celebrity members Urls (Urls, @length 0..255), Name (String), Id (RekognitionUniqueId, @pattern ^[0-9A-Za-z]*$), Face (ComparedFace), MatchConfidence (Percent, @range 0..100), KnownGender (KnownGender); shape KnownGender member Type (KnownGenderType).

Code evidence for the component to fix: services/service-com-amazonaws-rekognition-rekognitionservice/src/ops/recognize_celebrities.rs lines 242-269 build CelebrityFaces/UnrecognizedFaces from store.celebrities.identify(&appearances); services/service-com-amazonaws-rekognition-rekognitionservice/src/state/celebrity_catalogue.rs line 65 sets MINIMUM_RESEMBLANCE = 25.0 and lines 476-504 return None for any face resembling no seeded reference portrait above that floor.

## Steps to Reproduce
1. git clone the repository and boot the emulator built from that checkout.
2. Replay trace set simparity-wire-aws-v1.
3. Replay fixture rekognition-celebrity-recognition-and-lookup, step task#2 — RecognizeCelebrities on the fixture's image.
4. Observe HTTP 200 with an empty CelebrityFaces list ($.CelebrityFaces[0] absent), where AWS answers 200 with at least one populated Celebrity in CelebrityFaces (Id, Name, KnownGender.Type, Urls, plus Face and MatchConfidence).
5. After the fix, re-replay rekognition-celebrity-recognition-and-lookup#task#2 and confirm CelebrityFaces carries a Celebrity element with those members populated and that the corresponding face is no longer also reported in UnrecognizedFaces.
