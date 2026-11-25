Issue-Source: trace-sweep-divergence
Issue-Trace: connectcampaigns-error-create-campaign-validation
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcampaigns-error-create-campaign-validation connectcampaigns-error-onboarding-missing-instance connectcampaigns-error-operations-on-missing-campaign
Issue-Shape-Id: com.amazonaws.connectcampaigns#AmazonConnectCampaignService
Issue-Key: connect-campaigns|CreateCampaign|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|StartInstanceOnboardingJob|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|GetInstanceOnboardingJobStatus|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|GetConnectInstanceConfig|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|DeleteInstanceOnboardingJob|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|DescribeCampaign|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|GetCampaignState|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|PauseCampaign|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|ResumeCampaign|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|StartCampaign|4xx->4xx|$headers.content-type
Issue-Commit: b346fe5639d35cd9791aa731a7b2fb5c4cb110f2

# ConnectCampaigns v1 create-campaign and instance-onboarding replies are labelled application/json instead of application/x-amz-json-1.1

> _Note: this issue was identified against the code base at commit `b346fe5639d35cd9791aa731a7b2fb5c4cb110f2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCampaign

## Description
TEN causes, all on the ConnectCampaigns v1 service (Smithy: services/service-com-amazonaws-connectcampaigns-amazonconnectcampaignservice/smithy-model — AmazonConnectCampaignService, version 2021-01-30, sdkId ConnectCampaigns, arnNamespace/sigv4Name 'connect-campaigns'). Every cause is the same defect — the response Content-Type the emulator's serializer stamps on a ConnectCampaigns reply — observed on a different operation, and each cause is its own replayed step that must be made to pass. A fixer MUST close all ten (and the six in the sibling bundle 'Campaign lifecycle and ListCampaigns replies carry Content-Type application/json where the recording requires application/x-amz-json-1.1') before submitting.

Shared shape of the defect
AWS: the recorded AWS responses for these ConnectCampaigns calls are labelled 'Content-Type: application/x-amz-json-1.1'. The model states only the protocol (restJson1) and says nothing about the media type literal, so for the exact header value this is recorded behavior only: the recording is the requirement. The header value is fixed text with no variable part; the status line and JSON envelope of each step already match, so the media type is the only requirement being missed.
Emulator: every one of these replies is labelled 'Content-Type: application/json'. Each step reports exactly one divergence key: $headers.content-type: value_mismatch (expected 'application/x-amz-json-1.1', got 'application/json'), with expected_status == actual_status and no body key reported.
Why: Content-Type is how a client, proxy or cassette layer decides how to decode the body and which protocol the peer speaks. A caller relying on AWS's behavior sees a byte-correct envelope advertised with the wrong media type, so content negotiation and any media-type assertion misclassify the response; because the header comes from the serializer and not the handler, it is wrong on successes and refusals alike, across the whole service.
Fix: the response-serializer / protocol content-type selection in the ConnectCampaigns v1 service crate (the single place the response writer chooses the media type for both success and error envelopes). It must emit the media type these recordings require for this service rather than the generic 'application/json' default. Note the direction: open issue 20261115T223048Z-connect-campaigns-connectcampaigns-v1-replies-carry-content-type-application-x.md reports the INVERSE on a different fixture/step (connectcampaigns-error-list-tags-nonexistent-campaign#task#1, ListTagsForResource, expecting 'application/json' and getting 'application/x-amz-json-1.1'). It does not carry any of this bundle's replay keys, so it does not cover these steps, and a change that flips the literal unconditionally will satisfy one of the two and break the other. Hypothesis: the media type has to be derived per response path from the recorded expectations of each ConnectCampaigns operation rather than from one global constant for the signing name 'connect-campaigns'; confirm by replaying both this bundle and that issue's step together.

Cause 1 of 10: CreateCampaign (POST /campaigns) validation refusal — expected 400 with Content-Type application/x-amz-json-1.1, served 400 with application/json. Fixture connectcampaigns-error-create-campaign-validation step task#1.
Cause 2 of 10: StartInstanceOnboardingJob on an instance that is not onboarded — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-onboarding-missing-instance step task#1.
Cause 3 of 10: GetInstanceOnboardingJobStatus on the same unknown instance — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-onboarding-missing-instance step task#2.
Cause 4 of 10: GetConnectInstanceConfig on the same unknown instance — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-onboarding-missing-instance step task#3.
Cause 5 of 10: DeleteInstanceOnboardingJob on the same unknown instance — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-onboarding-missing-instance step task#4.
Cause 6 of 10: DescribeCampaign (GET /campaigns/{id}, ResourceNotFoundException httpError 404) on a missing campaign — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#1.
Cause 7 of 10: GetCampaignState on a missing campaign — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#2.
Cause 8 of 10: PauseCampaign on a missing campaign — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#3.
Cause 9 of 10: ResumeCampaign on a missing campaign — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#4.
Cause 10 of 10: StartCampaign on a missing campaign — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#5.

Traces (examples of the defect, one per cause): connectcampaigns-error-create-campaign-validation#task#1; connectcampaigns-error-onboarding-missing-instance#task#1, #task#2, #task#3, #task#4; connectcampaigns-error-operations-on-missing-campaign#task#1, #task#2, #task#3, #task#4, #task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200). Ten mismatch clusters, each with the single key $headers.content-type and no stub_path and no owned_by:

1. divergence_key service=connect-campaigns|CreateCampaign|status=400->400|keys=$headers.content-type — connectcampaigns-error-create-campaign-validation#task#1: expected 'application/x-amz-json-1.1', got 'application/json' (status 400 -> 400).
2. service=connect-campaigns|StartInstanceOnboardingJob|status=404->404|keys=$headers.content-type — connectcampaigns-error-onboarding-missing-instance#task#1.
3. service=connect-campaigns|GetInstanceOnboardingJobStatus|status=404->404|keys=$headers.content-type — connectcampaigns-error-onboarding-missing-instance#task#2.
4. service=connect-campaigns|GetConnectInstanceConfig|status=404->404|keys=$headers.content-type — connectcampaigns-error-onboarding-missing-instance#task#3.
5. service=connect-campaigns|DeleteInstanceOnboardingJob|status=404->404|keys=$headers.content-type — connectcampaigns-error-onboarding-missing-instance#task#4.
6. service=connect-campaigns|DescribeCampaign|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#1.
7. service=connect-campaigns|GetCampaignState|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#2.
8. service=connect-campaigns|PauseCampaign|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#3.
9. service=connect-campaigns|ResumeCampaign|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#4.
10. service=connect-campaigns|StartCampaign|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#5.

In every cluster the detail line is identical: "$headers.content-type: value_mismatch (expected 'application/x-amz-json-1.1', got 'application/json')". Expected status equals actual status in all ten and no body key is reported, so the status line and JSON envelope are correct and only the serializer's media type diverges.

Model evidence: AmazonConnectCampaignService 2021-01-30, protocols [restJson1], sigv4Name 'connect-campaigns'; DescribeCampaign http {GET /campaigns/{id}, code 200} with ResourceNotFoundException httpError 404; DeleteConnectInstanceConfig http {DELETE /connect-instance/{connectInstanceId}/config}. The model carries no media-type trait, so the recorded header is the only statement of the requirement.

## Steps to Reproduce
1. Clone the repo and boot the emulator from this checkout (the connectcampaigns v1 crate is loaded in the default build).
2. Replay the wire corpus for trace set simparity-wire-aws-v1.
3. Replay fixture connectcampaigns-error-create-campaign-validation step task#1 (CreateCampaign with an invalid request body) and inspect the raw response headers: the 400 envelope is labelled 'Content-Type: application/json' where the recording carries 'application/x-amz-json-1.1'.
4. Repeat for connectcampaigns-error-onboarding-missing-instance steps task#1..task#4 (StartInstanceOnboardingJob, GetInstanceOnboardingJobStatus, GetConnectInstanceConfig, DeleteInstanceOnboardingJob against an instance id that was never onboarded) and connectcampaigns-error-operations-on-missing-campaign steps task#1..task#5 (DescribeCampaign, GetCampaignState, PauseCampaign, ResumeCampaign, StartCampaign against a campaign id that does not exist): each answers the correct status and body with the wrong media type.
5. After the fix, all ten steps must report no $headers.content-type divergence, and the step named by open issue 20261115T223048Z (connectcampaigns-error-list-tags-nonexistent-campaign#task#1) must still pass its own expectation.
