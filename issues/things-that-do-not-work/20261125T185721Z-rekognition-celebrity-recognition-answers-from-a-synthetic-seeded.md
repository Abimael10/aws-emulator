Issue-Source: trace-sweep-divergence
Issue-Trace: rekognition-celebrity-recognition-and-lookup
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rekognition-celebrity-recognition-and-lookup
Issue-Shape-Id: com.amazonaws.rekognition#RekognitionService
Issue-Key: rekognition|RecognizeCelebrities|2xx->2xx|$.CelebrityFaces[*]
Issue-Key: rekognition|GetCelebrityInfo|2xx->2xx|$.KnownGender.Type
Issue-Key: rekognition|GetCelebrityInfo|2xx->2xx|$.Name
Issue-Key: rekognition|GetCelebrityInfo|2xx->2xx|$.Urls[*]
Issue-Commit: 8bf2bd5c40cd5968f6fa936e1c0608724e59a2f4

# Rekognition celebrity recognition answers from a synthetic seeded catalogue, so the recognize-then-lookup chain reports the wrong celebrity identity

> _Note: this issue was identified against the code base at commit `8bf2bd5c40cd5968f6fa936e1c0608724e59a2f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RecognizeCelebrities

## Description
This issue bundles 2 causes, both in the rekognition service and both rooted in the same defect: the emulator resolves a detected face against a hard-coded table of INVENTED people instead of a catalogue of real celebrities, and it names one for any face at all. Both causes must be closed before submitting.

Cause 1 of 2 - RecognizeCelebrities reports an invented celebrity identity (fixture rekognition-celebrity-recognition-and-lookup, step task#2).
AWS: RecognizeCelebrities takes an Image and returns CelebrityFaces, a list of Celebrity (Smithy: RecognizeCelebrities input Image (required) -> output CelebrityFaces: CelebrityList; shape Celebrity members Id (RekognitionUniqueId, pattern ^[0-9A-Za-z]*$), Name, Urls (Urls, @length 0..255, doc "An array of URLs pointing to additional information about the celebrity"), Face (ComparedFace), MatchConfidence (Percent, range 0..100, doc "The confidence, in percentage, that Amazon Rekognition has that the recognized face is the celebrity"), KnownGender). The identity AWS reports is an entry of AWS's own celebrity knowledge base: a real person's Name, a stable public Id, and Urls pointing at a real reference page. AWS only places a face in CelebrityFaces when it actually recognizes the person; a face it does not recognize goes to UnrecognizedFaces. The recorded response for this step is 200 with CelebrityFaces[0] = {Id, Name, KnownGender.Type, Urls} where the single Url is a real knowledge-base reference (the recording's own values are an example of the class, not the requirement).
Emulator: 200, but CelebrityFaces[0] carries an identity minted by the emulator itself: a Name and Id from the 12-entry table in services/service-com-amazonaws-rekognition-rekognitionservice/src/state/celebrity_catalogue.rs (SEEDED), a KnownGender.Type from that table, and a Urls entry of the fixed form "www.example.com/celebrity/<Id>" - the RFC 2606 documentation host, never a real reference page. The entry chosen is simply the seeded reference face the measured face correlates with most, and MINIMUM_RESEMBLANCE is 0.0, so ANY positive correlation names a celebrity: the replayed answer reports a match at MatchConfidence ~13, i.e. a face AWS would have reported as unrecognized is reported as a named person. The emulator's entry additionally carries Face and MatchConfidence, which the recorded entry for this step does not (both are modeled Celebrity members, so their presence is not the fault; the identity values are).
Why: a client that recognizes celebrities in a known image and then stores or displays the Name/Id/Urls it got back is handed a person who does not exist, with a link to example.com, and any image containing a face is reported as a celebrity at all - so negative paths (nobody recognized -> UnrecognizedFaces) never exercise, and the recognition result is unusable as an identity.
Fix: the RecognizeCelebrities handler's identity resolution and the catalogue it reads - services/service-com-amazonaws-rekognition-rekognitionservice/src/state/celebrity_catalogue.rs (the SEEDED table and MINIMUM_RESEMBLANCE) together with crate::ops::recognize_celebrities. The catalogue must hold the celebrity identities the corpus recognizes (real Name/Id/Urls), the face-to-entry resolution must reproduce the recorded identity for the recorded image, and a face resembling no entry must stay in UnrecognizedFaces rather than being named at a near-chance MatchConfidence.
Traces: rekognition-celebrity-recognition-and-lookup#task#2.

Cause 2 of 2 - GetCelebrityInfo returns the invented entry's Name/Urls/KnownGender for the Id carried forward from the recognition (fixture rekognition-celebrity-recognition-and-lookup, step task#3).
AWS: GetCelebrityInfo takes the required Id (RekognitionUniqueId, pattern ^[0-9A-Za-z]*$) - "use the Celebrity ID property as a unique identifier for the celebrity" - and returns Name, Urls and KnownGender for that celebrity, or ResourceNotFoundException (HTTP 400) when the Id names no celebrity (Smithy: GetCelebrityInfo input Id required; output Urls/Name/KnownGender; errors include ResourceNotFoundException). The chain RecognizeCelebrities -> Celebrity.Id -> GetCelebrityInfo must close on the SAME real person, so the Name and Urls here are the real knowledge-base values for that person. The recorded response is 200 with the recognized person's real Name, a KnownGender.Type and a real reference Url.
Emulator: 200, but with the synthetic catalogue entry's values - Name and KnownGender.Type from the SEEDED table and Urls = ["www.example.com/celebrity/<Id>"] - because the Id the step carries forward is the emulator-minted Id from cause 1 and store.celebrities.by_id resolves it inside the same invented table. The lookup itself is faithful (an Id the table does not hold is answered ResourceNotFoundException); what diverges is the identity the table holds, so this step cannot be made to pass without cause 1 being fixed, and it must be re-replayed separately to confirm the chain closes on the right person.
Why: a client that stored only the Id from recognition - exactly the pattern the operation's documentation prescribes - and later resolves it to a name and reference links gets the wrong person and a documentation-host URL, so celebrity metadata lookups built on the emulator never reproduce AWS's answer.
Fix: the GetCelebrityInfo handler's backing state, i.e. the same catalogue in services/service-com-amazonaws-rekognition-rekognitionservice/src/state/celebrity_catalogue.rs that crate::ops::get_celebrity_info reads via store.celebrities.by_id; no change is needed in the handler's not-found path.
Traces: rekognition-celebrity-recognition-and-lookup#task#3.

Neither cause is covered by an already-open issue: the open rekognition issues 20261121T034816Z-rekognition-detectlabels-returns-an-empty-labels-list-for-images.md and 20261121T045409Z-dynamodb-lambda-rekognition-s3-rekognition-detectlabels-answers-an-empty-labels-list-for.md are about DetectLabels, and the remaining open ones are unimplemented-operation stubs for DetectModerationLabels, ListProjectPolicies, SearchUsersByImage and GetLabelDetection.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200):

Cause 1 - divergence service=rekognition|RecognizeCelebrities|status=200->200|keys=$.CelebrityFaces[0], fixture rekognition-celebrity-recognition-and-lookup step task#2, verdict mismatch, expected_status 200 / actual_status 200.
  Expected CelebrityFaces[0]: {'Id': '4rr8sP5H', 'Name': 'Rob Nabors', 'KnownGender': {'Type': 'Male'}, 'Urls': ['www.wikidata.org/wiki/Q7340410']}
  Actual   CelebrityFaces[0]: {'Id': 'Wd6Lq1Hs', 'Name': 'Marisol Vega', 'KnownGender': {'Type': 'Female'}, 'Urls': ['www.example.com/celebrity/Wd6Lq1Hs'], 'MatchConfidence': 13.131485939025879, 'Face': {BoundingBox/Confidence/Landmarks/Pose/Quality ...}}
  The actual Id/Name/Urls/KnownGender are verbatim the third entry of SEEDED in src/state/celebrity_catalogue.rs (id "Wd6Lq1Hs", name "Marisol Vega", urls ["www.example.com/celebrity/Wd6Lq1Hs"], KNOWN_GENDER_FEMALE), which is what shows the answer comes from the invented table rather than from the image. MatchConfidence 13.13 with MINIMUM_RESEMBLANCE = 0.0 shows the entry was named on a near-chance correlation.

Cause 2 - divergence service=rekognition|GetCelebrityInfo|status=200->200|keys=$.KnownGender.Type,$.Name,$.Urls[0], fixture rekognition-celebrity-recognition-and-lookup step task#3, verdict mismatch, expected_status 200 / actual_status 200.
  $.Name: expected 'Rob Nabors', got 'Marisol Vega'
  $.KnownGender.Type: expected 'Male', got 'Female'
  $.Urls[0]: expected 'www.wikidata.org/wiki/Q7340410', got 'www.example.com/celebrity/Wd6Lq1Hs'
  Same seeded entry as cause 1, reached through store.celebrities.by_id with the Id task#2 handed forward.

Requirement vs example: the requirement is that the recognized identity and its GetCelebrityInfo resolution are AWS's real celebrity entry for the submitted image, with Urls pointing at a real reference page, and that an unrecognized face stays in UnrecognizedFaces. The specific names, Ids and URLs above are the recording's example values filling that template, not literals to hard-code.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus: replay_wire_corpus() (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture rekognition-celebrity-recognition-and-lookup step task#2 - POST RecognizeCelebrities with the fixture's Image. Compare $.CelebrityFaces[0]: the emulator answers 200 with an Id/Name/Urls/KnownGender from the 12-entry SEEDED table in src/state/celebrity_catalogue.rs (Urls of the form www.example.com/celebrity/<Id>) plus a MatchConfidence of ~13, where the recording carries the real celebrity's Id/Name/KnownGender and a real reference Url.
3. Cause 2: replay the same fixture's step task#3 - GetCelebrityInfo with the Id that step task#2 returned. The emulator answers 200 with that same seeded entry's Name, KnownGender.Type and Urls[0] instead of the recorded celebrity's values.
4. Both steps must be re-replayed after the fix: task#3 only closes once the catalogue and the face-to-entry resolution behind task#2 report the recorded identity.
