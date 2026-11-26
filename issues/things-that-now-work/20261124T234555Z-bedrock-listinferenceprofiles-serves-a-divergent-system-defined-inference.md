Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-catalog-list-inference-profiles
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-catalog-list-inference-profiles
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock|ListInferenceProfiles|2xx->2xx|$.inferenceProfileSummaries[*]
Issue-Commit: f8e20ae46548b9b285010d18fd783588395da7bd

# bedrock ListInferenceProfiles serves a divergent system-defined inference-profile catalog

> _Note: this issue was identified against the code base at commit `f8e20ae46548b9b285010d18fd783588395da7bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListInferenceProfiles

## Description
Cause 1 of 1: the system-defined inference-profile catalog that ListInferenceProfiles serves diverges from AWS across almost the whole list — profile display names and descriptions are built from a truncated model label, and createdAt/updatedAt are a single synthesized placeholder instant rather than the per-profile creation and last-update instants AWS reports. Fixture bedrock-catalog-list-inference-profiles, step task#1.

AWS: Smithy (com.amazonaws.bedrock#AmazonBedrockControlPlaneService) models ListInferenceProfiles as a read-only GET /inference-profiles returning 200 with ListInferenceProfilesResponse.inferenceProfileSummaries, a list of InferenceProfileSummary. InferenceProfileSummary members: inferenceProfileName (required, @length 1..64, @pattern ^([0-9a-zA-Z][ _-]?)+$), description (@length 1..200, sensitive), createdAt and updatedAt (Timestamp, @timestampFormat date-time), inferenceProfileArn (required), models (required, @length 1..5), inferenceProfileId (required), status (required enum; ACTIVE means ready), type (required enum; SYSTEM_DEFINED means 'defined by Amazon Bedrock ... route inference requests across regions'). The model does not enumerate which SYSTEM_DEFINED profiles exist, so for the catalog's membership and per-profile values this is recorded behavior only: the recording shows a SYSTEM_DEFINED cross-region profile list of at least 93 entries in which each profile's inferenceProfileName and description name the model as the foundation-model catalog names it (the full model name, including an 'Instruct' qualifier where the underlying model carries one), and in which createdAt and updatedAt are distinct per-profile instants (createdAt the date the profile was published, updatedAt a later, sub-second-precision instant), not a shared constant.

Emulator: for the same request it answers 200, but the summaries diverge from index 2 and across indices 6..92 of the recorded list. For the Meta Llama 3.1 8B cross-region profile (inferenceProfileId us.meta.llama3-1-8b-instruct-v1:0, whose ARN and models list DO match) it serves inferenceProfileName 'US Meta Llama 3.1 8B' where the recording requires 'US Meta Llama 3.1 8B Instruct', description 'Routes requests to Meta Llama 3.1 8B in us-east-1, us-east-2 and us-west-2.' where the recording requires 'Routes requests to Meta Llama 3.1 8B Instruct in us-east-1, us-east-2 and us-west-2.', and createdAt/updatedAt both '2024-08-26T00:00:00Z' where the recording requires createdAt '2024-11-01T00:00:00Z' and updatedAt '2024-11-04T18:02:21.191757959Z'. The same shape of divergence repeats for the remaining entries the replay reports ($.inferenceProfileSummaries[2] and [6] through [92]), i.e. the emulator's seeded profile list is not the recorded AWS list in either its per-entry metadata or its membership/ordering past index 1.

Note on wording: the fixed text of a profile description is the template 'Routes requests to <model name> in <region list>.' — the model name must be the catalog's full model name and the region list must be the profile's own member regions derived from its models list; the literal model name and regions above are the recorded example filling that template, not the requirement. Likewise the timestamps above are the recorded values for one profile: the requirement is that createdAt and updatedAt be the profile's own distinct publication and last-update instants, not one constant shared by every entry.

Why: ListInferenceProfiles is how a client discovers the cross-region inference profiles it may pass as a modelId to InvokeModel/Converse and how a console or inventory tool labels them. A client that enumerates profiles and matches or displays them by name sees names and descriptions that do not exist in AWS (the 'Instruct' qualifier is dropped), so name-based lookup fails and UI labels are wrong; a client that filters or sorts by createdAt/updatedAt — or that caches the catalog and refreshes on a changed updatedAt — sees one constant instant for every profile and can neither order nor invalidate its view. Where entries are absent or out of position altogether, a profile AWS publishes is simply undiscoverable through the emulator.

Fix: the bedrock control-plane crate's inference-profile catalog state — the seeded SYSTEM_DEFINED inference-profile table that the ListInferenceProfiles handler projects — must carry each profile's recorded name, description, createdAt and updatedAt rather than deriving the name/description from a shortened model label and stamping a shared timestamp, and must carry the full recorded profile set in the recorded order. The ListInferenceProfiles handler/serializer itself is not at fault for the entries it does project correctly (ARN, models, inferenceProfileId, status, type all match).

Traces: bedrock-catalog-list-inference-profiles#task#1 (ListInferenceProfiles, expected 200 / actual 200, body divergence on $.inferenceProfileSummaries[2] and [6]..[92]).

Related open issues (different operation, not covering this one): 20261116T011319Z-bedrock-listfoundationmodels-emits-inferenceapissupported-as-a-flat-string.md and 20261118T190920Z-bedrock-listfoundationmodels-serves-wrong-per-model-capability-members-and.md both concern ListFoundationModels only. The earlier bundle that did cover ListInferenceProfiles, 20261113T142522Z-bedrock-catalog-discovery-lists-diverge-from-the-recorded.md, is resolved and does not cover this divergence.

## Evidence
Replay of fixture bedrock-catalog-list-inference-profiles, step task#1 (service bedrock, operation ListInferenceProfiles), verdict mismatch, expected_status 200 / actual_status 200.

Expected (recorded AWS) $.inferenceProfileSummaries[6]:
  {'createdAt': '2024-11-01T00:00:00Z',
   'description': 'Routes requests to Meta Llama 3.1 8B Instruct in us-east-1, us-east-2 and us-west-2.',
   'inferenceProfileArn': 'arn:aws:bedrock:<region>:<account>:inference-profile/us.meta.llama3-1-8b-instruct-v1:0',
   'inferenceProfileId': 'us.meta.llama3-1-8b-instruct-v1:0',
   'inferenceProfileName': 'US Meta Llama 3.1 8B Instruct',
   'models': [3 entries: meta.llama3-1-8b-instruct-v1:0 in us-east-1, us-east-2, us-west-2],
   'status': 'ACTIVE', 'type': 'SYSTEM_DEFINED',
   'updatedAt': '2024-11-04T18:02:21.191757959Z'}

Actual (emulator) $.inferenceProfileSummaries[6]:
  {'createdAt': '2024-08-26T00:00:00Z',
   'description': 'Routes requests to Meta Llama 3.1 8B in us-east-1, us-east-2 and us-west-2.',
   'inferenceProfileArn': <matches>,
   'inferenceProfileId': 'us.meta.llama3-1-8b-instruct-v1:0',
   'inferenceProfileName': 'US Meta Llama 3.1 8B',
   'models': <matches>,
   'status': 'ACTIVE', 'type': 'SYSTEM_DEFINED',
   'updatedAt': '2024-08-26T00:00:00Z'}

Diverging member set reported by the replay (divergence_key): $.inferenceProfileSummaries[2] and $.inferenceProfileSummaries[6] through $.inferenceProfileSummaries[92] — i.e. 88 of the recorded list's entries. Only indices 0, 1, 3, 4 and 5 agree.

Smithy consulted: ListInferenceProfiles (GET /inference-profiles, 200, paginated items=inferenceProfileSummaries) and InferenceProfileSummary (inferenceProfileName required @length 1..64 @pattern ^([0-9a-zA-Z][ _-]?)+$; description @length 1..200 sensitive; createdAt/updatedAt Timestamp @timestampFormat date-time; status/type required enums).

Not grounded in the model: which SYSTEM_DEFINED profiles exist and their individual names/descriptions/timestamps — recorded behavior only.

## Steps to Reproduce
1. Boot the emulator from this checkout (bedrock control plane serving).
2. Replay wire fixture `bedrock-catalog-list-inference-profiles`, step `task#1` — an unfiltered ListInferenceProfiles read: GET /inference-profiles (no typeEquals, no maxResults).
3. Compare the 200 response body's `inferenceProfileSummaries` list element-by-element against the recording.
4. Observe that element 6 (inferenceProfileId `us.meta.llama3-1-8b-instruct-v1:0`) carries inferenceProfileName 'US Meta Llama 3.1 8B' and description 'Routes requests to Meta Llama 3.1 8B in ...' instead of the recorded 'US Meta Llama 3.1 8B Instruct' / 'Routes requests to Meta Llama 3.1 8B Instruct in ...', and that its createdAt and updatedAt are the same constant instant instead of the recorded distinct publication and last-update instants.
5. Observe the same divergence shape across $.inferenceProfileSummaries[2] and [6]..[92]; indices 0, 1, 3, 4 and 5 agree.
