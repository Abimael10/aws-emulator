Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connectparticipant.complete-attachment-upload-twice-conflict-rejected
Issue-Shape-Id: com.amazonaws.connectparticipant#AmazonConnectParticipantServiceLambda
Issue-Key: connectparticipant.complete-attachment-upload-twice-conflict-rejected|_transcript_count#14|Timeout|time.sleep(3)
Issue-Commit: 4ed63421c2a299f1b98968b719f9a71bb0543024

# connectparticipant attachment data path does not complete the duplicate-CompleteAttachmentUpload sequence inside its replay budget (check times out at 60.0s)

> _Note: this issue was identified against the code base at commit `4ed63421c2a299f1b98968b719f9a71bb0543024`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CompleteAttachmentUpload

## Description
Cause 1 of 1: the attachment round-trip that CompleteAttachmentUpload sits in never finishes while the behavioral check watches it — the only failing connectparticipant check is killed by the harness at 60.0 s with no response ever diverging, so every assertion after the upload is left ungraded (failing check: connectparticipant.complete-attachment-upload-twice-conflict-rejected).

AWS: Smithy model services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/smithy-model/com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda.json (sdkId ConnectParticipant, restJson1). StartAttachmentUpload (POST /participant/start-attachment-upload) answers 200 with AttachmentId + UploadMetadata{Url, UrlExpiry, HeadersToInclude}; the caller PUTs the bytes to that Url and the target MUST accept the body (the check's PUT carries a 30 s socket timeout and accepts 200/201/204). CompleteAttachmentUpload (POST /participant/complete-attachment-upload, AttachmentIds @length 1..1, ClientToken @idempotencyToken) answers 200 with an empty body, moves the attachment to ArtifactStatus APPROVED and puts it into the transcript exactly once; its documentation states "A conflict exception is thrown when an attachment with that identifier is already being uploaded" (ConflictException, httpError 409), and the check accepts either that refusal or an idempotent 200 for the second call. GetAttachment then answers 200 with a Url. Timing is recorded behavior only: on AWS each of these data-plane calls answers in well under a second and the presigned PUT returns immediately, so the whole sequence (two uploads, two completions, one duplicate completion, GetAttachment and the transcript reads) finishes inside the budget the suite allows.

Emulator: the check makes no graded request at all — the suite reports only "Error: Check timed out after 60.0s" for it, i.e. the sequence is still running when the budget expires. Nothing in the report shows a wrong status, code or body, so this is a liveness/latency divergence of the attachment path rather than a shape divergence. Hypothesis (the report carries no per-call timing, so a fixer must instrument the run to pick between these): (a) the pre-signed upload target StartAttachmentUpload publishes does not answer the PUT — the Url is signed against the emulator's own endpoint in src/ops/start_attachment_upload.rs and the file is received by the plane in crate::service, so if that plane never answers, each of the check's two PUTs burns its full 30 s socket timeout, which alone consumes the whole 60 s; or (b) the attachment never becomes visible as expected in the transcript/status reads the check polls (CompleteAttachmentUpload sets ARTIFACT_STATUS_APPROVED and appends an ATTACHMENT item in src/ops/complete_attachment_upload.rs, and GetTranscript reports it through transcript_item in src/ops/get_transcript.rs), so the check's bounded polls spin to their deadlines. Note for whoever picks this up: the check's own design contributes ~60 s of mandatory holds (its _transcript_count holds 30 s after an attachment first appears, and it calls it twice), so if instrumentation shows every emulator call answering promptly and correctly, the right outcome is to record that the check cannot fit a 60 s budget rather than to change behavior — that possibility is exactly why this is filed MEDIUM and not as a confirmed wrong answer.

Why: a client uploading a chat attachment through this emulator cannot be shown to complete the documented round trip — StartAttachmentUpload → PUT → CompleteAttachmentUpload → GetTranscript/GetAttachment. If the published upload target does not answer, every SDK upload hangs until the client's own timeout and the attachment is never completable; and because the run is cut short, the duplicate-completion rule (one attachment appears in the transcript exactly once, the second completion being refused 409 ConflictException) is unverified, so an over-permissive duplicate would go unnoticed.

Fix: the attachment data plane that serves the pre-signed upload/download URLs this service publishes (crate::service's attachment plane plus the URL minting in src/ops/start_attachment_upload.rs), and the attachment visibility the completion writes (src/ops/complete_attachment_upload.rs) as read back by src/ops/get_transcript.rs and GetAttachment. No handler response shape is implicated by the evidence.

Traces: connectparticipant.complete-attachment-upload-twice-conflict-rejected (the only failing connectparticipant check; 16/17 passed).

## Evidence
Behavioral suite, service connectparticipant: 16/17 passed (94%).

[mismatch] connectparticipant.complete-attachment-upload-twice-conflict-rejected
  Error: Check timed out after 60.0s

No response-level divergence was reported for the check: no status, error code or body mismatch — the check was killed mid-sequence, so every assertion after the first upload is ungraded.

What the check does, and therefore what must answer promptly: StartAttachmentUpload (200 + UploadMetadata.Url/HeadersToInclude) → HTTP PUT of the payload to that Url (30 s socket timeout, requires 200/201/204) → CompleteAttachmentUpload (200) → a second CompleteAttachmentUpload for the SAME AttachmentId with a fresh ClientToken (AWS: 409 ConflictException, fixed-class text naming the attachment id from the request; the check also accepts an idempotent 200) → GetTranscript polls asserting the attachment appears exactly once → a second, distinct attachment is uploaded, completed, observed APPROVED in the transcript and fetched with GetAttachment (non-empty Url).

Code read in this checkout for grounding (citations only): services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/ops/start_attachment_upload.rs lines 249-292 mint the pre-signed PUT URL against the emulator's own endpoint and store ARTIFACT_STATUS_IN_PROGRESS; src/ops/complete_attachment_upload.rs lines 215-265 refuse an already-APPROVED attachment with ConflictException and otherwise set APPROVED and append the ATTACHMENT transcript item; src/ops/get_transcript.rs lines 540-567 report an item's Attachments (AttachmentId/Status) from the stored record. The connect side sets the session's attachments_shared from the instance's storage association in services/service-com-amazonaws-connect-amazonconnectservice/src/ops/start_chat_contact.rs line 460, so the check's setup path is wired.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Create a Connect instance, publish a contact flow, associate an ATTACHMENTS storage config and UpdateAttachedFilesConfiguration, then StartChatContact and CreateParticipantConnection(Type=["CONNECTION_CREDENTIALS"]) to obtain a ConnectionToken.
3. Call StartAttachmentUpload(ContentType="text/plain", AttachmentSizeInBytes=<len>, AttachmentName="x.txt", ClientToken=<uuid>, ConnectionToken=<token>) and time the HTTP PUT of the payload to UploadMetadata.Url with the returned HeadersToInclude — record whether that target answers 200/201/204 and how long it takes.
4. Call CompleteAttachmentUpload(AttachmentIds=[<id>], ClientToken=<uuid>, ConnectionToken=<token>), then call it again for the SAME AttachmentId with a fresh ClientToken; record the status/code of both and time each.
5. Poll GetTranscript(SortOrder="ASCENDING", MaxResults=100) and record how long it takes for the attachment to appear with Status APPROVED, and that it appears in exactly one item; then GetAttachment(AttachmentId=<id>) and record its latency and that Url is non-empty.
6. Re-replay connectparticipant.complete-attachment-upload-twice-conflict-rejected to confirm the fix: it must complete within the suite's budget with the duplicate completion refused 409 ConflictException (or accepted idempotently) and the attachment listed exactly once. If step 3-5 timings show every emulator call answering promptly and correctly, report that the check's own ~60 s of mandatory polling holds exceeds the 60 s budget instead of changing behavior.
