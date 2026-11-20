Issue-Source: trace-sweep-divergence
Issue-Trace: connectcampaigns-error-list-tags-nonexistent-campaign
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcampaigns-error-list-tags-nonexistent-campaign
Issue-Shape-Id: com.amazonaws.connectcampaigns#AmazonConnectCampaignService
Issue-Key: connect-campaigns|ListTagsForResource|4xx->4xx|$headers.content-type
Issue-Commit: c2a51bbc379e00e6c722f42efee8d77cde344066

# ConnectCampaigns v1 replies carry Content-Type application/x-amz-json-1.1 instead of application/json

> _Note: this issue was identified against the code base at commit `c2a51bbc379e00e6c722f42efee8d77cde344066`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
ONE cause, found on the ConnectCampaigns **v1** service (Smithy: AmazonConnectCampaignService, version 2021-01-30, sdkId ConnectCampaigns, sigv4Name/arnNamespace 'connect-campaigns', protocol aws.protocols#restJson1). The service token the replay reports for it is 'connect-campaigns', the signing name it shares with ConnectCampaignsV2.

=== Cause 1 of 1: the response Content-Type header on a ConnectCampaigns v1 reply is 'application/x-amz-json-1.1' where the service answers 'application/json' ===

AWS: Smithy: AmazonConnectCampaignService declares protocol restJson1, whose response media type is `application/json`; ListTagsForResource is `GET /tags/{arn}` (input member `arn`, @required, @httpLabel, @length min 20 max 500, @pattern ^arn:.*$) with modeled errors AccessDeniedException(403), ValidationException(400), ResourceNotFoundException(404), ThrottlingException(429), InternalServerException(500). The model states nothing further about the media type, so for the exact header value this is recorded behavior only: the recorded AWS response for a ListTagsForResource call naming a campaign ARN that does not exist is HTTP 404 with a JSON error envelope labelled `Content-Type: application/json`. That header value is a fixed literal with no variable part; the recorded status (404) and the error envelope the recording carries are the requirement for this error class, and the envelope's message text is the service's fixed wording with the offending resource ARN taken from the request's `arn` label - never a literal from one recording.

Emulator: the reply is labelled `Content-Type: application/x-amz-json-1.1`. The replayed step diverges on that header alone - `$headers.content-type: value_mismatch (expected 'application/json', got 'application/x-amz-json-1.1')` - with expected_status 404 and actual_status 404 and no body key reported, so the status line and the JSON error body are produced correctly and only the media type is wrong. This is the awsJson1_1 media type being applied to a service whose protocol is restJson1. Note the direction: it is the INVERSE of the already-open v2 issue '20261029T183744Z-connect-campaigns-connectcampaignsv2-responses-carry-the-wrong-content-type-on' (ConnectCampaignsV2, which does answer `application/x-amz-json-1.1`, is served `application/json`). That issue's Issue-Key list covers v2 operations only and does not carry this fixture's replay key, so it does not cover this step; a change that makes the x-amz-json-1.1 literal unconditional for everything signing as 'connect-campaigns' satisfies v2 and breaks v1, which is what this step is showing. The open router issue '20261022T145747Z-connectcampaigns-is-entirely-unreachable-router-resolves-services-by' concerns v1 requests being refused with AmbiguousService and likewise does not own this header.

Why: the Content-Type is how a client, proxy or recorded-cassette layer decides how to decode a body and which protocol the peer speaks. A caller relying on AWS's behavior sees a ConnectCampaigns v1 response advertised as an awsJson1_1 payload, so content negotiation and any media-type assertion classify a byte-correct restJson1 error envelope as the wrong type, and the emulator becomes indistinguishable at the header level from an awsJson service. It affects refusals and successes alike, since the header comes from the serializer, not the handler.

Fix: the response serializer / protocol content-type selection used by the ConnectCampaigns v1 service crate (the restJson1 response writer, applied uniformly to success and error envelopes). The media type must be chosen per service from that service's own protocol trait rather than from the shared 'connect-campaigns' signing name, so v1 emits `application/json` while v2 keeps the `application/x-amz-json-1.1` its recordings require. No ListTagsForResource handler logic is involved.

Traces: connectcampaigns-error-list-tags-nonexistent-campaign#task#1 (ListTagsForResource, 404 -> 404, header-only divergence).

## Evidence
Replayed step connectcampaigns-error-list-tags-nonexistent-campaign#task#1, operation ListTagsForResource, verdict mismatch:
  $headers.content-type: value_mismatch (expected 'application/json', got 'application/x-amz-json-1.1')

Expected (AWS, ConnectCampaigns v1 / restJson1): HTTP 404, JSON error envelope, Content-Type: application/json.
Actual (emulator): HTTP 404, JSON error envelope, Content-Type: application/x-amz-json-1.1.

Status (404 expected, 404 actual) and the response body matched - the cluster reports only the `$headers.content-type` key, so this is purely the serializer's media type. divergence_key: service=connect-campaigns|ListTagsForResource|status=404->404|keys=$headers.content-type. affected_count: 1.

Model evidence: services/service-com-amazonaws-connectcampaigns-amazonconnectcampaignservice/smithy-model - AmazonConnectCampaignService, version 2021-01-30, protocols [restJson1], sdkId ConnectCampaigns, sigv4Name 'connect-campaigns'; ListTagsForResource http {GET /tags/{arn}, code 200}, errors incl. ResourceNotFoundException httpError 404.

## Steps to Reproduce
1. Boot the emulator from this checkout (both connectcampaigns and connectcampaignsv2 crates are loaded in the default build).
2. Point a SigV4-signed ConnectCampaigns **v1** client (signing name 'connect-campaigns') at the data listener.
3. Call ListTagsForResource with the ARN of a campaign that does not exist, i.e. `GET /tags/arn:aws:connect-campaigns:<region>:<account>:campaign/<unknown-uuid>`.
4. Inspect the raw response headers: the 404 error envelope is labelled `Content-Type: application/x-amz-json-1.1`, where ConnectCampaigns v1 answers `application/json`.
5. Replay fixture connectcampaigns-error-list-tags-nonexistent-campaign step task#1 and confirm it reports `$headers.content-type: value_mismatch (expected 'application/json', got 'application/x-amz-json-1.1')`; after the fix it must pass while the ConnectCampaignsV2 steps keep expecting `application/x-amz-json-1.1`.
