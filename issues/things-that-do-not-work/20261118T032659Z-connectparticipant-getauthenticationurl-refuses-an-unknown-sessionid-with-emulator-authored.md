Issue-Source: trace-sweep-divergence
Issue-Trace: connectparticipant-participant-session-lifecycle
Issue-Step: task#13
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectparticipant-participant-session-lifecycle
Issue-Shape-Id: com.amazonaws.connectparticipant#AmazonConnectParticipantServiceLambda
Issue-Key: connectparticipant|GetAuthenticationUrl|4xx->4xx|$.message
Issue-Commit: 027d9e9288dfd341f60d75b6ca76c3164fcd4263

# GetAuthenticationUrl refuses an unknown SessionId with emulator-authored message text instead of AWS's fixed ValidationException sentence

> _Note: this issue was identified against the code base at commit `027d9e9288dfd341f60d75b6ca76c3164fcd4263`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAuthenticationUrl

## Description
One net-new root cause in connectparticipant, found by replaying the wire fixture corpus (simparity-wire-aws-v1) against the emulator built from this checkout. 22/26 steps passed; the other three diverging steps are already owned by open issues (GetTranscript MessageMetadata/NextToken, StartAttachmentUpload fail-open) and are not re-filed here.

Cause 1 of 1: GetAuthenticationUrl correctly refuses a SessionId that names no live authentication session with HTTP 400 ValidationException, but words the refusal with its own state-revealing sentence instead of AWS's fixed parameter-validation text.

AWS: Smithy (services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/smithy-model/com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda.json, sdkId ConnectParticipant, restJson1) models GetAuthenticationUrl as POST /participant/authentication-url with required SessionId (@length min 36 / max 36), required RedirectUri (@length 1..1024) and required ConnectionToken bound to the X-Amz-Bearer header; its modelled errors are AccessDeniedException (403), ValidationException (client, httpError 400), ThrottlingException (429) and InternalServerException (500). Recorded behavior: a SessionId that satisfies the modelled 36-character length but does not name a current authentication session for the contact is refused by AWS with HTTP 400, error type ValidationException, and the message "Specified request parameter(s) are invalid." That whole sentence is FIXED text: no part of it is taken from the request or from service state. In particular AWS does NOT echo the submitted SessionId and does NOT state whether such a session exists for the contact — it collapses every bad-SessionId reason (unknown id, another contact's session, already cancelled, expired) into the one sentence.

Emulator: the status (400) and the error class match, but the message body diverges: $.message is "Authentication session with id <SessionId> does not exist for this contact" — the fixed prefix "Authentication session with id ", the SessionId copied verbatim out of the request, and the fixed suffix " does not exist for this contact". (The id string in the replay detail is this run's request value and is not part of the requirement.)

Why: the error message is part of the refusal contract. A client that branches on, logs or surfaces the reason a customer sign-in URL could not be retrieved never sees AWS's wording, so retry/branch logic and user-facing text keyed on "Specified request parameter(s) are invalid." silently takes the wrong path. Worse, because the emulator's sentence asserts that no such session exists "for this contact" and echoes the submitted id, a caller can distinguish "unknown id" from the other causes AWS deliberately collapses — an existence oracle the real API does not give, so code (or a security review) developed against the emulator sees information the real service withholds.

Fix: the GetAuthenticationUrl handler's SessionId validation path and the shared refusal-message helper it calls — the ValidationException arm must emit AWS's fixed parameter text rather than a per-request, state-revealing sentence. File/line citations under Traces.

Related open issue (does NOT cover this): 20261117T160640Z-connectparticipant-authentication-session-refusals-are-emulator-authored-getauthenticationurl-cancelparticipantauthentication.md describes the same wording defect from the BEHAVIORAL corpus; it carries only a behavioral check id (connectparticipant.authentication-operations-invalid-session-id-rejected) and no wire replay key, so this wire fixture step was reported unowned by the replay and is filed here so a fixer re-replays it.

Traces: wire fixture connectparticipant-participant-session-lifecycle, step task#13 (operation GetAuthenticationUrl, verdict mismatch). Code: services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/state/authentication.rs lines 39-51 (fn unknown_authentication_session_message) and its caller services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/ops/get_authentication_url.rs lines 195-205.

## Evidence
Wire corpus replay (simparity-wire-aws-v1): steps_total 26, steps_passed 22, steps_mismatched 4, score 84.6%.

Divergence cluster (fixture connectparticipant-participant-session-lifecycle, step task#13, service connectparticipant, operation GetAuthenticationUrl, verdict mismatch, expected_status 400, actual_status 400):

  $.message: value_mismatch (expected 'Specified request parameter(s) are invalid.', got 'Authentication session with id sim2real-partauth-1a6cc2a4dd01e666fe does not exist for this contact')

Expected (AWS): HTTP 400, error type ValidationException, message exactly "Specified request parameter(s) are invalid." — entirely fixed text, no request- or state-derived parts.
Actual (emulator): HTTP 400, ValidationException class, message "Authentication session with id <SessionId from the request> does not exist for this contact" — fixed prefix "Authentication session with id " + the SessionId value interpolated from the request + fixed suffix " does not exist for this contact". The literal id in the replay line above is the fixture's per-run request value, not a requirement.

Smithy evidence cited: GetAuthenticationUrl input SessionId required @length min 36/max 36, RedirectUri required @length 1..1024, ConnectionToken required httpHeader X-Amz-Bearer; modelled error ValidationException errorKind client httpError 400; http binding POST /participant/authentication-url, success code 200.

All other steps of the same fixture (CreateParticipantConnection, SendMessage, GetTranscript positive paths) pass, so the session and connection token used by this step are healthy — only the refusal wording diverges.

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it; point a ConnectParticipant client at its data listener.
2. Replay wire fixture connectparticipant-participant-session-lifecycle: it starts a chat contact, calls CreateParticipantConnection to obtain a ConnectionToken, and exercises the participant session.
3. At step task#13 the fixture calls GetAuthenticationUrl with a SessionId that names no live authentication session for the contact, RedirectUri=<https callback>, and the ConnectionToken in the X-Amz-Bearer header.
4. Inspect the raw 400 response body: $.message is "Authentication session with id <submitted SessionId> does not exist for this contact", where AWS returns the fixed "Specified request parameter(s) are invalid.".
5. Re-replay the wire corpus to confirm a fix: fixture connectparticipant-participant-session-lifecycle step task#13 must pass, with the 400 ValidationException body carrying AWS's fixed message and no request-derived or state-derived text.
