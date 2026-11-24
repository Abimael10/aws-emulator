Issue-Source: trace-sweep-divergence
Issue-Trace: connectcampaigns-error-onboarding-missing-instance
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcampaigns-error-onboarding-missing-instance connectcampaigns-error-validation-boundaries
Issue-Shape-Id: com.amazonaws.connectcampaigns#AmazonConnectCampaignService
Issue-Key: connect-campaigns|DeleteConnectInstanceConfig|4xx->4xx|$.message
Issue-Key: connect-campaigns|DeleteConnectInstanceConfig|4xx->4xx|$headers.content-type
Issue-Key: connect-campaigns|GetCampaignStateBatch|2xx->2xx|$headers.content-type
Issue-Related: 20261117T041857Z-connect-campaigns-getcampaignstatebatch-failedrequests-entries-omit-the-failurereason-text.md
Issue-Commit: b346fe5639d35cd9791aa731a7b2fb5c4cb110f2

# ConnectCampaigns v1 error text and batch failure reasons are emulator-authored, not AWS's

> _Note: this issue was identified against the code base at commit `b346fe5639d35cd9791aa731a7b2fb5c4cb110f2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteConnectInstanceConfig

## Description
TWO causes on the ConnectCampaigns v1 service (Smithy: services/service-com-amazonaws-connectcampaigns-amazonconnectcampaignservice/smithy-model — AmazonConnectCampaignService, version 2021-01-30, sigv4Name 'connect-campaigns'). They share one fault: the explanatory text the emulator returns for a failure is its own prose instead of the text AWS words for that failure class. Both must be closed. Both clusters ALSO diverge on $headers.content-type; that header is the serializer defect filed separately in the two 'application/x-amz-json-1.1' bundles for this service and is not re-filed here.

Cause 1 of 2: DeleteConnectInstanceConfig on an instance that was never onboarded returns the wrong ResourceNotFoundException message.
AWS: DeleteConnectInstanceConfig is DELETE /connect-instance/{connectInstanceId}/config (input connectInstanceId: @required @httpLabel, @length 0..256, @pattern ^[a-zA-Z0-9_\-.]*$), errors AccessDeniedException(403), InternalServerException(500), InvalidStateException(409), ResourceNotFoundException(404 — members message @required, x-amzn-ErrorType header), ThrottlingException(429), ValidationException(400). The model does not state message text, so the wording is recorded behavior only: the recorded AWS response is HTTP 404 with $.message = 'Instance is not onboarded'. That string is entirely FIXED text — it interpolates nothing from the request or from state, and in particular it does not name the instance id.
Emulator: answers the correct status (404) but with $.message = 'ConnectInstanceConfig not found <connectInstanceId>.' — emulator-authored prose built from a different template, whose fixed parts are 'ConnectInstanceConfig not found ' and the trailing '.' and whose variable part is the connectInstanceId echoed from the request's httpLabel (the uuid in the recording is one run's request value, not part of any template).
Why: the error message is part of the contract for an error class. A client that matches, logs or surfaces the refusal reason sees an internal type name ('ConnectInstanceConfig') and a resource id where AWS states a condition about the instance's onboarding state, so operator-facing diagnostics and any message assertion diverge; the emulator also leaks an internal state-store noun that AWS never exposes.
Fix: the DeleteConnectInstanceConfig handler's not-found refusal path for connect-campaigns (where the ResourceNotFoundException is constructed) must emit the service's fixed wording for a not-onboarded instance rather than a generic '<StateType> not found <id>.' formatter. Note the open issue 20261118T105449Z-connectcampaigns-deleteconnectinstanceconfig-stub.md (unimplemented-scaffold stub for this operation): the replay shows the operation now answering 404 with a body, this step carries no stub_path and no owned_by, and that issue does not carry this replay key, so it does not cover this cause.
Traces: connectcampaigns-error-onboarding-missing-instance#task#5.

Cause 2 of 2: GetCampaignStateBatch partial-failure entries omit the failure reason text.
AWS: GetCampaignStateBatch is POST /campaigns-state (input campaignIds: CampaignIdList @required @length min 1), output GetCampaignStateBatchResponse with successfulRequests / failedRequests (@length 0..25). The model's FailedCampaignStateResponse declares only campaignId (@length 0..256, @pattern ^[\S]*$) and failureCode, so the reason member is recorded behavior only: the recorded AWS response is HTTP 200 and each entry for a campaign id that does not exist carries campaignId, failureCode 'ResourceNotFound' AND a reason string. That string is a template whose fixed text is 'Campaign: ' and " doesn't exist", with the single variable part being the campaign id echoed from the corresponding element of the request's campaignIds list; the uuid in the recording merely fills that template.
Emulator: answers HTTP 200 with failedRequests of the right length and order, but each element is only {"campaignId": ..., "failureCode": "ResourceNotFound"} — the reason member is absent, which the replay reports as the element being both missing and extra.
Why: a batch caller relies on the per-id reason to explain and triage a partial failure; with the member absent, SDK consumers that display or match the reason get nothing and cannot distinguish failure causes behind one failureCode.
Fix: the GetCampaignStateBatch handler's response construction for connect-campaigns — the failed-request element struct needs the reason field and the serializer must emit it, formatted from the requested campaign id. Open issue 20261117T041857Z-connect-campaigns-getcampaignstatebatch-failedrequests-entries-omit-the-failurereason-text.md describes the same shape of defect but is keyed to a DIFFERENT fixture/step (connectcampaignsv2-campaign-outbound-requests#task#2) and carries none of this step's replay key, so it does not cover this v1 step; fixing one does not demonstrate the other.
Traces: connectcampaigns-error-validation-boundaries#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up). Both clusters have empty stub_path and empty owned_by.

Cause 1 — divergence_key service=connect-campaigns|DeleteConnectInstanceConfig|status=404->404|keys=$.message,$headers.content-type, fixture connectcampaigns-error-onboarding-missing-instance step task#5:
  $.message: value_mismatch (expected 'Instance is not onboarded', got 'ConnectInstanceConfig not found 37c20524-6029-4761-a057-589232f43782.')
  $headers.content-type: value_mismatch (expected 'application/x-amz-json-1.1', got 'application/json')
Expected (recorded AWS): HTTP 404, $.message = 'Instance is not onboarded' — wholly fixed text, no request or state values interpolated.
Actual (emulator): HTTP 404, $.message = 'ConnectInstanceConfig not found <connectInstanceId>.' — fixed parts 'ConnectInstanceConfig not found ' + '.', variable part = connectInstanceId from the request's httpLabel (the uuid shown is that one run's value).
Status matches 404 -> 404; the divergence is the message text (plus the separately filed media type).

Cause 2 — divergence_key service=connect-campaigns|GetCampaignStateBatch|status=200->200|keys=$.failedRequests[0],$headers.content-type, fixture connectcampaigns-error-validation-boundaries step task#3:
  $.failedRequests[0]: missing/extra — expected {'campaignId': '<id>', 'failureCode': 'ResourceNotFound', 'failureReason': "Campaign: <id> doesn't exist"}, got {'campaignId': '<id>', 'failureCode': 'ResourceNotFound'}
  $headers.content-type: value_mismatch (expected 'application/x-amz-json-1.1', got 'application/json')
Expected: HTTP 200 with the reason member present on every failed entry, text = 'Campaign: ' + <campaign id echoed from the request> + " doesn't exist".
Actual: HTTP 200, reason member absent from the entry. Status matches 200 -> 200.

Model evidence: DeleteConnectInstanceConfig http {DELETE /connect-instance/{connectInstanceId}/config}, ResourceNotFoundException httpError 404 with required 'message'; InvalidStateException httpError 409 (not used here). GetCampaignStateBatch http {POST /campaigns-state, code 200}; FailedCampaignStateResponse members = campaignId, failureCode only — hence 'recorded behavior only' for the reason text.

## Steps to Reproduce
1. Clone the repo and boot the emulator from this checkout.
2. Replay the wire corpus for trace set simparity-wire-aws-v1.
3. Cause 1: replay fixture connectcampaigns-error-onboarding-missing-instance step task#5 — DELETE /connect-instance/<an instance id that was never onboarded>/config (DeleteConnectInstanceConfig). The reply is 404 but $.message reads 'ConnectInstanceConfig not found <that id>.' instead of AWS's fixed 'Instance is not onboarded'.
4. Cause 2: replay fixture connectcampaigns-error-validation-boundaries step task#3 — POST /campaigns-state (GetCampaignStateBatch) with campaignIds naming campaigns that do not exist. The 200 body's failedRequests entries carry campaignId and failureCode 'ResourceNotFound' but no reason member, where AWS also returns "Campaign: <that campaign id> doesn't exist".
5. After the fix both steps must replay with no $.message / $.failedRequests divergence (the $headers.content-type key on both steps is owned by the separately filed ConnectCampaigns content-type bundles).
