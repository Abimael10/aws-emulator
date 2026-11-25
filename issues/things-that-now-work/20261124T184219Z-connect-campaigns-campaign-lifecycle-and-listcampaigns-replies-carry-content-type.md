Issue-Source: trace-sweep-divergence
Issue-Trace: connectcampaigns-error-operations-on-missing-campaign
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcampaigns-error-operations-on-missing-campaign connectcampaigns-error-validation-boundaries
Issue-Shape-Id: com.amazonaws.connectcampaigns#AmazonConnectCampaignService
Issue-Key: connect-campaigns|StopCampaign|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|UpdateCampaignName|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|UpdateCampaignOutboundCallConfig|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|DeleteCampaign|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|ListCampaigns|2xx->2xx|$headers.content-type
Issue-Key: connect-campaigns|ListCampaigns|4xx->4xx|$headers.content-type
Issue-Commit: b346fe5639d35cd9791aa731a7b2fb5c4cb110f2

# Campaign lifecycle and ListCampaigns replies carry Content-Type application/json where the recording requires application/x-amz-json-1.1

> _Note: this issue was identified against the code base at commit `b346fe5639d35cd9791aa731a7b2fb5c4cb110f2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopCampaign

## Description
SIX causes, all on the ConnectCampaigns v1 service (Smithy: services/service-com-amazonaws-connectcampaigns-amazonconnectcampaignservice/smithy-model — AmazonConnectCampaignService, version 2021-01-30, sdkId ConnectCampaigns, sigv4Name 'connect-campaigns'). This is the second half of the same serializer defect; the first ten causes are filed as 'ConnectCampaigns v1 create-campaign and instance-onboarding replies are labelled application/json instead of application/x-amz-json-1.1'. Each cause here is its own replayed step and must be closed individually — six steps, not one.

Shared shape of the defect
AWS: the recorded AWS responses for these ConnectCampaigns calls are labelled 'Content-Type: application/x-amz-json-1.1'. The model declares the protocol (restJson1) and nothing about the media-type literal, so for the exact header value this is recorded behavior only and the recording is the requirement. The value is fixed text with no variable part. It is wrong on 2xx successes as well as 4xx refusals, which the ListCampaigns pair below shows directly.
Emulator: each of these replies is labelled 'Content-Type: application/json'; every cluster reports the single key $headers.content-type with expected_status == actual_status and no body key.
Why: a client, proxy or recorded-cassette layer uses Content-Type to choose a decoder and to decide which protocol the peer speaks, so a correct body advertised with the wrong media type is misclassified; any media-type assertion fails against an otherwise byte-correct response. Because the header is emitted by the serializer rather than a handler, it is wrong uniformly for the whole service.
Fix: the response-serializer / protocol content-type selection in the ConnectCampaigns v1 service crate — the one place the response writer stamps the media type for success and error envelopes alike. No per-operation handler logic is involved. Do not flip the literal globally for the signing name: open issue 20261115T223048Z-connect-campaigns-connectcampaigns-v1-replies-carry-content-type-application-x.md records the inverse expectation on connectcampaigns-error-list-tags-nonexistent-campaign#task#1 and carries none of this bundle's replay keys, so both must hold after the change.

Cause 1 of 6: StopCampaign on a campaign that does not exist — expected 404 with Content-Type application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#6.
Cause 2 of 6: UpdateCampaignName on a campaign that does not exist — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#7.
Cause 3 of 6: UpdateCampaignOutboundCallConfig on a campaign that does not exist — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#9.
Cause 4 of 6: DeleteCampaign on a campaign that does not exist — expected 404 with application/x-amz-json-1.1, served 404 with application/json. Fixture connectcampaigns-error-operations-on-missing-campaign step task#10.
Cause 5 of 6: ListCampaigns (POST /campaigns-summary, maxResults @range 1..50) on a SUCCESSFUL page — expected 200 with application/x-amz-json-1.1, served 200 with application/json, showing the defect is not confined to error envelopes. Fixture connectcampaigns-error-validation-boundaries step task#1.
Cause 6 of 6: ListCampaigns with an out-of-range maxResults — expected 400 with application/x-amz-json-1.1, served 400 with application/json (the ValidationException status and body are correct). Fixture connectcampaigns-error-validation-boundaries step task#2.

Traces (one per cause): connectcampaigns-error-operations-on-missing-campaign#task#6, #task#7, #task#9, #task#10; connectcampaigns-error-validation-boundaries#task#1, #task#2.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up). Six mismatch clusters, each with the single key $headers.content-type, empty stub_path and empty owned_by; every detail line is identical: "$headers.content-type: value_mismatch (expected 'application/x-amz-json-1.1', got 'application/json')".

1. service=connect-campaigns|StopCampaign|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#6 (404 -> 404).
2. service=connect-campaigns|UpdateCampaignName|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#7.
3. service=connect-campaigns|UpdateCampaignOutboundCallConfig|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#9.
4. service=connect-campaigns|DeleteCampaign|status=404->404|keys=$headers.content-type — connectcampaigns-error-operations-on-missing-campaign#task#10.
5. service=connect-campaigns|ListCampaigns|status=200->200|keys=$headers.content-type — connectcampaigns-error-validation-boundaries#task#1 (200 -> 200, a success page).
6. service=connect-campaigns|ListCampaigns|status=400->400|keys=$headers.content-type — connectcampaigns-error-validation-boundaries#task#2 (400 -> 400).

Expected status equals actual status in all six and no body key is reported, so the status line and the JSON payload are produced correctly and only the serializer's media type diverges — on a 200 as well as on 400/404.

Model evidence: AmazonConnectCampaignService 2021-01-30, protocols [restJson1]; ListCampaigns http {POST /campaigns-summary, code 200}, input maxResults @range min 1 max 50, errors AccessDeniedException(403), InternalServerException(500), ValidationException(400). No media-type trait exists in the model, so the recorded header is the sole statement of the requirement.

## Steps to Reproduce
1. Clone the repo and boot the emulator from this checkout.
2. Replay the wire corpus for trace set simparity-wire-aws-v1.
3. Replay connectcampaigns-error-operations-on-missing-campaign steps task#6, task#7, task#9, task#10 (StopCampaign, UpdateCampaignName, UpdateCampaignOutboundCallConfig, DeleteCampaign against a campaign id that does not exist) and inspect the raw headers: each 404 envelope is labelled 'Content-Type: application/json' where the recording carries 'application/x-amz-json-1.1'.
4. Replay connectcampaigns-error-validation-boundaries step task#1 (ListCampaigns returning a page, HTTP 200) and step task#2 (ListCampaigns with maxResults outside 1..50, HTTP 400): both bodies and statuses match, both are labelled 'application/json'.
5. After the fix, all six steps must report no $headers.content-type divergence while the step in open issue 20261115T223048Z keeps passing its own expectation.
