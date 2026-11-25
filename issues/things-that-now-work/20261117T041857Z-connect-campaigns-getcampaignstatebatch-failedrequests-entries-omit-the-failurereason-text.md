Issue-Source: trace-sweep-divergence
Issue-Trace: connectcampaignsv2-campaign-outbound-requests
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcampaignsv2-campaign-outbound-requests
Issue-Shape-Id: com.amazonaws.connectcampaignsv2#AmazonConnectCampaignServiceV2
Issue-Key: connect-campaigns|GetCampaignStateBatch|2xx->2xx|$.failedRequests[*]
Issue-Commit: e974643efe5559269bccc30718b5c19c98ab99de

# GetCampaignStateBatch failedRequests entries omit the failureReason text

> _Note: this issue was identified against the code base at commit `e974643efe5559269bccc30718b5c19c98ab99de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCampaignStateBatch

## Description
One cause.

Cause 1 of 1: GetCampaignStateBatch returns partial-failure entries in `failedRequests` without the human-readable `failureReason` member, so every failed campaign id in a batch loses its explanation.

AWS: Smithy (connectcampaignsv2, GetCampaignStateBatch, POST /v2/campaigns-state, output GetCampaignStateBatchResponse -> failedRequests: FailedCampaignStateResponseList) models FailedCampaignStateResponse with members `campaignId` (CampaignId, @length 1..256, @pattern ^[-:/a-zA-Z0-9]+$) and `failureCode` (GetCampaignStateBatchFailureCode, values ResourceNotFound | UnknownError). The model does NOT declare a `failureReason` member, so for that member this is recorded behavior only: the recorded AWS response for this operation returns HTTP 200 and, for each campaign id that does not exist, a failedRequests entry carrying all three of `campaignId`, `failureCode` = "ResourceNotFound" and `failureReason`. The reason text is a template: the fixed text is "Campaign: " and " doesn't exist", and the only variable part is the campaign id, which is echoed from the corresponding element of the request's `campaignIds` list — i.e. `Campaign: <campaignId from request> doesn't exist`. The ids in the recording (e.g. 74fd8c00-4fac-416a-a76c-4827dbab7fe7, 2ee0e574-5143-4eb7-a426-494004d34e54) are request values filling that template, not part of it. The operation stays a 200 partial-success response; no modeled error (AccessDeniedException 403, ValidationException 400, ThrottlingException 429, InternalServerException 500) is involved.

Emulator: replies HTTP 200 (status matches) with a failedRequests list of the right length and order, but each entry contains only {"campaignId": ..., "failureCode": "ResourceNotFound"} — the `failureReason` member is absent from every element. The replay reports each element as both missing and extra because the whole object differs by that one key.

Why: a client that surfaces or logs the per-id explanation gets nothing to show. Callers of a batch API rely on failureReason to distinguish "the campaign id was never created" from other non-existence causes, and SDK consumers that string-match or display the reason see an empty/undefined value, so partial-failure handling and diagnostics silently degrade. Because the reason is also absent for ids where the campaign is missing for a different reason, the emulator cannot express any variation at all.

Fix: the GetCampaignStateBatch handler's response construction for connect-campaigns (the batch-state operation handler and the serializer/struct it builds failedRequests from) must populate the reason member alongside campaignId/failureCode, formatting the text from the requested campaign id rather than a constant string. The response struct for the failed-request element needs the extra field so the serializer emits it.

Traces: fixture connectcampaignsv2-campaign-outbound-requests step task#2 (both failedRequests[0] and failedRequests[1] diverge in the same way).

Related open issues, neither of which covers this: 20261022T145747Z-connectcampaigns-is-entirely-unreachable-router-resolves-services-by.md (routing by SigV4 signing name) and 20261115T223048Z-connect-campaigns-connectcampaigns-v1-replies-carry-content-type-application-x.md (Content-Type on v1 replies, which owns the other divergence in this corpus).

## Evidence
Fixture connectcampaignsv2-campaign-outbound-requests, step task#2, GetCampaignStateBatch.
Expected (recorded AWS, HTTP 200): $.failedRequests[0] = {"campaignId": "<id1>", "failureCode": "ResourceNotFound", "failureReason": "Campaign: <id1> doesn't exist"}; $.failedRequests[1] = {"campaignId": "<id2>", "failureCode": "ResourceNotFound", "failureReason": "Campaign: <id2> doesn't exist"} — fixed text "Campaign: " + " doesn't exist", variable part = the campaign id echoed from the request's campaignIds.
Actual (emulator, HTTP 200): $.failedRequests[0] = {"campaignId": "<id1>", "failureCode": "ResourceNotFound"}; $.failedRequests[1] = {"campaignId": "<id2>", "failureCode": "ResourceNotFound"} — failureReason absent on every element.
Status matches (200 -> 200); the divergence is the missing body member only. Divergence key: service=connect-campaigns|GetCampaignStateBatch|status=200->200|keys=$.failedRequests[0],$.failedRequests[1].

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture connectcampaignsv2-campaign-outbound-requests, step task#2: POST /v2/campaigns-state (GetCampaignStateBatch) with a campaignIds list of ids that do not exist in emulator state.
3. Inspect the 200 response body: each $.failedRequests[*] entry has campaignId and failureCode but no failureReason, whereas the recorded AWS response includes failureReason = "Campaign: <that campaign id> doesn't exist".
