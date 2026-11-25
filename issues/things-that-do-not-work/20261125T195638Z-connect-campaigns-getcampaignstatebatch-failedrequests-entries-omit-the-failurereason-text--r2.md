Issue-Source: trace-sweep-divergence
Issue-Trace: connectcampaigns-error-validation-boundaries
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcampaigns-error-validation-boundaries
Issue-Shape-Id: com.amazonaws.connectcampaigns#AmazonConnectCampaignService
Issue-Key: connect-campaigns|GetCampaignStateBatch|2xx->2xx|$.failedRequests[*]
Issue-Commit: 24511e4ce5aef4d33e503a00bb0e57c4ce26e21d

# GetCampaignStateBatch failedRequests entries omit the failureReason text

> _Note: this issue was identified against the code base at commit `24511e4ce5aef4d33e503a00bb0e57c4ce26e21d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCampaignStateBatch

## Description
ONE cause on the ConnectCampaigns v1 service (Smithy: services/service-com-amazonaws-connectcampaigns-amazonconnectcampaignservice/smithy-model — AmazonConnectCampaignService, sigv4Name 'connect-campaigns').

Cause 1 of 1: GetCampaignStateBatch partial-failure entries carry no reason text.
AWS: GetCampaignStateBatch is POST /campaigns-state, http code 200 (Smithy: input GetCampaignStateBatchRequest.campaignIds -> CampaignIdList @required @length min 1; output GetCampaignStateBatchResponse.successfulRequests / .failedRequests -> @length 0..25; errors AccessDeniedException(403), InternalServerException(500), ThrottlingException(429), ValidationException(400)). The op reports per-id failures INSIDE a 200 body rather than as an error. The modeled FailedCampaignStateResponse declares only campaignId (CampaignId, @length 0..256, @pattern ^[\S]*$) and failureCode (GetCampaignStateBatchFailureCode), so the reason member is recorded behavior only: the recording shows HTTP 200 where each entry for a campaign id that does not exist carries campaignId, failureCode 'ResourceNotFound' AND a failureReason string. That string is a template — its FIXED text is 'Campaign: ' and " doesn't exist", and its single VARIABLE part is the campaign id echoed from the corresponding element of the request's campaignIds list. The uuid in the recording merely fills that template and is not itself a requirement.
Emulator: answers HTTP 200 with failedRequests of the right length and element order, but each element is only {"campaignId": <id>, "failureCode": "ResourceNotFound"} — the failureReason member is absent entirely, which the replay reports as the element being simultaneously 'missing' and 'extra' at $.failedRequests[0].
Why: a batch caller relies on the per-id reason to explain and triage a partial failure, because the whole batch still returns 200. With the member absent, SDK consumers that surface, log or match the reason get nothing behind a single opaque failureCode, and cannot tell a nonexistent campaign apart from other ResourceNotFound causes.
Fix: the GetCampaignStateBatch handler's response construction for connect-campaigns — the failed-request element struct needs the failureReason field and the response serializer must emit it, formatted from the campaign id that was requested (not from state).
Traces: connectcampaigns-error-validation-boundaries#task#3.

Related open issues that do NOT cover this cause: 20261124T184219Z-connect-campaigns-connectcampaigns-v1-error-text-and-batch-failure-reasons.md describes this shape of defect but its replay keys are connect-campaigns|DeleteConnectInstanceConfig|4xx->4xx|$.message, |$headers.content-type and connect-campaigns|GetCampaignStateBatch|2xx->2xx|$headers.content-type — none of which is this cluster's key ($.failedRequests[0]); the replay therefore reports this step with an EMPTY owned_by, so closing that issue would not gate on or re-run this divergence. 20261117T041857Z-connect-campaigns-getcampaignstatebatch-failedrequests-entries-omit-the-failurereason-text.md is keyed to a different fixture/step (connectcampaignsv2-campaign-outbound-requests#task#2) and likewise carries none of this step's replay key.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator_up=true, 20 steps, 18 passed, 2 mismatched, score 90%). This cluster: stub_path empty, owned_by empty, affected_count 1.

divergence_key: service=connect-campaigns|GetCampaignStateBatch|status=200->200|keys=$.failedRequests[0]
fixture connectcampaigns-error-validation-boundaries, step task#3, verdict mismatch.

Expected (recorded AWS): HTTP 200, $.failedRequests[0] = {'campaignId': <requested campaign id>, 'failureCode': 'ResourceNotFound', 'failureReason': "Campaign: <that same requested campaign id> doesn't exist"}. Fixed text of the message: 'Campaign: ' + " doesn't exist"; variable part: the campaign id taken from the request's campaignIds element.
Actual (emulator): HTTP 200, $.failedRequests[0] = {'campaignId': <requested campaign id>, 'failureCode': 'ResourceNotFound'} — failureReason absent. Reported as:
  $.failedRequests[0]: missing (expected {...'failureReason': "Campaign: ... doesn't exist"}, got {...no failureReason})
  $.failedRequests[0]: extra (same pair)
Status matches 200 -> 200 and failureCode matches 'ResourceNotFound'; the sole divergence is the omitted failureReason member.

Model evidence: GetCampaignStateBatch http {POST /campaigns-state, code 200, codeDefaulted}; FailedCampaignStateResponse members = campaignId, failureCode only — hence the reason text is 'recorded behavior only'.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from this checkout.
2. Replay the wire corpus for trace set simparity-wire-aws-v1.
3. Replay fixture connectcampaigns-error-validation-boundaries, step task#3: POST /campaigns-state (GetCampaignStateBatch) with campaignIds naming one or more campaigns that do not exist in the emulator's state.
4. Observe the 200 body: each failedRequests entry carries campaignId and failureCode 'ResourceNotFound' but no failureReason, where AWS also returns failureReason = 'Campaign: ' + <the requested campaign id> + " doesn't exist".
5. After the fix the step must replay with no $.failedRequests divergence (the $headers.content-type key on this step is owned by the separately filed ConnectCampaigns content-type bundle).
