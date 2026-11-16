Issue-Source: trace-sweep-divergence
Issue-Trace: connectparticipant-participant-session-lifecycle
Issue-Step: task#12
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectparticipant-participant-session-lifecycle
Issue-Shape-Id: com.amazonaws.connectparticipant#AmazonConnectParticipantServiceLambda
Issue-Key: connectparticipant|StartAttachmentUpload|4xx->2xx|$.message
Issue-Key: connectparticipant|StartAttachmentUpload|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: connectparticipant|StartAttachmentUpload|4xx->2xx|status
Issue-Commit: da177b6c885c0d0f980f9c5ee9dceb1aac434502

# StartAttachmentUpload fails open on an invalid connection token instead of 403 AccessDeniedException

> _Note: this issue was identified against the code base at commit `da177b6c885c0d0f980f9c5ee9dceb1aac434502`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartAttachmentUpload

## Description
One net-new cause, found by replaying wire trace set simparity-wire-aws-v1 against the emulator built from this checkout.

Cause 1 of 1: StartAttachmentUpload publishes a pre-signed upload target and HTTP 200 for a connection token AWS refuses, instead of denying the call.

AWS: Smithy model services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/smithy-model/com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda.json (sdkId ConnectParticipant, restJson1). StartAttachmentUpload (POST /participant/start-attachment-upload, success 200) takes ConnectionToken as the required X-Amz-Bearer header (ParticipantToken, @length 1..1000) and declares AccessDeniedException (errorKind client, httpError 403, member Message REQUIRED, doc "You do not have sufficient access to perform this action"). A restJson1 error response also carries the x-amzn-errortype header naming the error shape. The recorded response for this denial class is HTTP 403 with body member message = 'Access denied'; that text is entirely FIXED for this class — no part of it is taken from the request or from service state — and the x-amzn-errortype header is present (asserted as shaped, not by literal value). The requirement is therefore: a ConnectionToken that does not authenticate a currently connected participant of a live chat MUST be refused with 403 AccessDeniedException, body {"message": "Access denied"}, and an x-amzn-errortype header.

Emulator: HTTP 200 with a successful StartAttachmentUploadResponse (AttachmentId plus UploadMetadata with a live pre-signed URL). Replay reports "status: expected 403, got 200", "$.message: missing (expected 'Access denied', got None)" and "$headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)" — i.e. no error body member and no error-type header are emitted at all, because no error is produced. Hypothesis: the handler's own denial arm is correct in form (services/.../src/ops/start_attachment_upload.rs builds AccessDeniedException from crate::state::ACCESS_DENIED_MESSAGE when state.session_for_connection_token(..) resolves to nothing), so the token presented at this step is resolving to a session the emulator should not have — most plausibly a session CreateParticipantConnection minted for a participant token the service never issued (its None arm registers an arbitrary token and creates a ChatSession), which leaves the connection token derived from it fully authoritative for every later data-plane call. Whatever the route, the attachment path is the one observed answering 200 here, and the refusal is not reached.

Why: relative to AWS this is an authentication bypass on a data-plane write. A client holding a token AWS denies is handed a working, pre-signed upload target and can attach content to a contact, so no caller or test ever exercises the denial branch for attachments; code that distinguishes "denied" from "accepted" by status, by x-amzn-errortype, or by the message member sees a success and proceeds, and the emulator diverges from AWS exactly on the negative path this service's only credential has.

Fix: StartAttachmentUpload's authorization step — the connection-token resolution in the operation's handler (and the state lookup it delegates to) must deny a token that does not name a connected participant of a live chat, returning the modeled AccessDeniedException with AWS's fixed message text 'Access denied' through the restJson1 error serializer (status 403, lowercase body member `message`, x-amzn-errortype header) rather than continuing into minting an attachment record and signing an upload URL.

Traces: connectparticipant-participant-session-lifecycle#task#12 (StartAttachmentUpload, expected 403, actual 200).

Related open issues, neither of which carries this fixture step's replay key: 20261109T180950Z-connectparticipant-error-paths-miss-accessdenied-and-aws-message.md (same denial class on CreateParticipantConnection task#6 and SendMessage task#15) and 20261105T033734Z-connectparticipant-7-net-new-causes-connection-token-refusal.md (behavioral, no wire key). Neither names StartAttachmentUpload or task#12, so this divergence is filed with its own key and must be re-replayed in its own right.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up true, /__admin/health 200, data listener accepting): 26 steps total, 20 passed, 6 mismatched, 5 divergence clusters, score 76.9%.

Fixture connectparticipant-participant-session-lifecycle, step task#12, operation StartAttachmentUpload, verdict mismatch, expected_status 403, actual_status 200. Divergence detail, verbatim:
  "status: expected 403, got 200"
  "$.message: missing (expected 'Access denied', got None)"
  "$headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)"
divergence_key: service=connectparticipant|StartAttachmentUpload|status=403->200|keys=$.message,$headers.x-amzn-errortype,status

Expected (AWS): HTTP 403, AccessDeniedException, body member `message` with the fixed text 'Access denied' (no request- or state-derived parts), x-amzn-errortype header present naming the error shape.
Actual (emulator): HTTP 200 with AttachmentId and UploadMetadata (pre-signed PUT URL, expiry, headers); no `message` member, no x-amzn-errortype header.

Model citation: StartAttachmentUpload errors = AccessDeniedException(403) / InternalServerException(500) / ServiceQuotaExceededException(402) / ThrottlingException(429) / ValidationException(400); AccessDeniedException.Message is required.
Code citation for the arm that is not reached: services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/ops/start_attachment_upload.rs lines 205-212 (the `None` arm of state.session_for_connection_token), with the lookup itself at src/state.rs lines 313-325.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a ConnectParticipant client at its data listener.
2. Obtain a connection token the service should not honour for data-plane calls — e.g. call CreateParticipantConnection with a ParticipantToken this emulator never issued (it currently succeeds and returns ConnectionCredentials), or use a connection token whose participant has been disconnected.
3. Call StartAttachmentUpload with that ConnectionToken in the X-Amz-Bearer header and a valid AttachmentName / ContentType / AttachmentSizeInBytes / ClientToken.
4. Observe HTTP 200 with an AttachmentId and a live pre-signed UploadMetadata.Url, where AWS answers HTTP 403 AccessDeniedException with body {"message": "Access denied"} and an x-amzn-errortype header.
5. Re-replay fixture step connectparticipant-participant-session-lifecycle#task#12 to confirm a fix: it must report status 403 with the message member 'Access denied' and the error-type header present.
