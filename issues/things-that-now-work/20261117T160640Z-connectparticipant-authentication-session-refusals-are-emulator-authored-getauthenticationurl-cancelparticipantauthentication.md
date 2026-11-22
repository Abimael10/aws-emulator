Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connectparticipant.authentication-operations-invalid-session-id-rejected
Issue-Shape-Id: com.amazonaws.connectparticipant#AmazonConnectParticipantServiceLambda
Issue-Key: connectparticipant.authentication-operations-invalid-session-id-rejected|run#6|AssertionError|assert url_err == ("ValidationException", _INVALID_PARAMS), f"got {url_err!r}"
Issue-Commit: fbb16d7ef183300468a2464d60b6797b413232a5

# Authentication-session refusals are emulator-authored: GetAuthenticationUrl/CancelParticipantAuthentication word a malformed SessionId as "Authentication session with id <id> does not exist for this contact" instead of AWS's "Specified request parameter(s) are invalid."

> _Note: this issue was identified against the code base at commit `fbb16d7ef183300468a2464d60b6797b413232a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAuthenticationUrl

## Description
One net-new root cause in connectparticipant's two customer-authentication operations, found by replaying the behavioral suite against the emulator.

Cause 1 of 1: a SessionId that satisfies the modelled length but names no authentication session is refused with the right class and status but with the emulator's own sentence instead of AWS's message, and that sentence discloses state AWS's refusal does not.

AWS: Smithy (services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/smithy-model/com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda.json, sdkId ConnectParticipant, restJson1) models SessionId as a string with @length min 36 / max 36, required on both GetAuthenticationUrlRequest (POST /participant/authentication-url) and CancelParticipantAuthenticationRequest (POST /participant/cancel-authentication); both operations declare ValidationException (errorKind client, httpError 400, member Message required, target Reason len 1..2000, doc "The input fails to satisfy the constraints specified by Amazon Connect"). Recorded behavior: a 36-character value that is not a real session id (so botocore's own min=max=36 check does not intercept it) is refused by AWS with HTTP 400 ValidationException whose Message is the fixed text "Specified request parameter(s) are invalid." — the SAME text for both operations. That whole sentence is FIXED: no part of it is taken from the request or from service state, and in particular AWS does NOT name the rejected SessionId and does NOT say whether such a session exists for the contact.

Emulator: the error code and 400 status match, but the Message is the emulator's own reference-refusal sentence — ValidationException "Authentication session with id zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz does not exist for this contact", i.e. the fixed prefix "Authentication session with id ", the SessionId copied verbatim out of the request, and the fixed suffix " does not exist for this contact". Both operations produce it from the same shared helper, so the divergence is identical on GetAuthenticationUrl and on CancelParticipantAuthentication (the check asserts GetAuthenticationUrl first and stops there; the CancelParticipantAuthentication assertion in the same check asserts the same expected pair and is served by the same helper).

Why: the message is part of the refusal contract. A client (or a test) that branches on, logs or surfaces the reason a sign-in URL could not be retrieved never sees AWS's wording, so retry/branching logic keyed on that text silently takes the wrong path; and because the emulator's sentence states that no such session exists "for this contact" and echoes the submitted id, a caller can distinguish "unknown id" from the other reasons AWS collapses into one fixed sentence (another contact's session, already cancelled, past the one-minute window) — an existence oracle AWS deliberately does not give. Note the related open issue 20261109T180950Z-connectparticipant-error-paths-miss-accessdenied-and-aws-message.md: it covers emulator-authored wording on CreateParticipantConnection/SendMessage error paths only, and carries no key for either authentication operation, so this cause is filed separately. The fixed issue 20261105T033734Z-connectparticipant-getauthenticationurl-stub.md stood the operation up; the operation now answers, so this is a wording defect of the implemented handler, not a re-refusal.

Fix: the shared refusal-message helper and the two handlers' SessionId validation path — the message a non-current/malformed SessionId is refused with must be AWS's fixed parameter text rather than a per-request, state-revealing sentence. Citations under Traces.

Traces: behavioral check connectparticipant.authentication-operations-invalid-session-id-rejected. Code: services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/state/authentication.rs lines 39-51 (fn unknown_authentication_session_message), and its callers services/.../src/ops/get_authentication_url.rs lines 195-205 and services/.../src/ops/cancel_participant_authentication.rs lines 190-202.

## Evidence
Behavioral suite replay (15/17 passed): check connectparticipant.authentication-operations-invalid-session-id-rejected, verdict mismatch.

  AssertionError: got ('ValidationException', 'Authentication session with id zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz does not exist for this contact')
  assert url_err == ("ValidationException", "Specified request parameter(s) are invalid.")

Expected (AWS, both operations): HTTP 400, Error.Code ValidationException, Error.Message exactly "Specified request parameter(s) are invalid." — entirely fixed text.
Actual (emulator): HTTP 400, Error.Code ValidationException, Error.Message "Authentication session with id <SessionId from the request> does not exist for this contact" — fixed prefix/suffix with the submitted SessionId interpolated (the 36 'z' characters in the replay are this run's request value, not part of the requirement).

Source: services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/state/authentication.rs:49
  pub fn unknown_authentication_session_message(session_id: &str) -> String {
      format!("Authentication session with id {session_id} does not exist for this contact")
  }
returned from the ValidationException arms of GetAuthenticationUrl::handle (ops/get_authentication_url.rs:197-204) and CancelParticipantAuthentication::handle (ops/cancel_participant_authentication.rs:193-201).

The positive control in the same check (SendMessage on the same connection token) is unaffected: the session itself is healthy, only the refusal text diverges.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a ConnectParticipant client at its data listener.
2. Start a chat and obtain a ConnectionToken: connect.start_chat_contact(...) then connectparticipant.create_participant_connection(ParticipantToken=<token>, Type=["CONNECTION_CREDENTIALS"]).
3. Call get_authentication_url(SessionId="z"*36, RedirectUri="https://example.com/callback", ConnectionToken=<token>) — a 36-character value passes botocore's client-side length check and reaches the server as a malformed SessionId.
4. Inspect the raw 400 body: Error.Message is "Authentication session with id zzzz...z does not exist for this contact" where AWS returns the fixed "Specified request parameter(s) are invalid."
5. Call cancel_participant_authentication(SessionId="z"*36, ConnectionToken=<token>) — same divergence from the same helper.
6. Re-replay behavioral check connectparticipant.authentication-operations-invalid-session-id-rejected to confirm a fix; it must pass (its positive-control SendMessage at the end must still succeed).
