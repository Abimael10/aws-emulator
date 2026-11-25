Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.get-session-endpoint-malformed-id-rejected athena.session-endpoint-unavailable-before-session-ready
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena.get-session-endpoint-malformed-id-rejected|run#5|AssertionError|assert malformed_message == (
Issue-Key: athena.session-endpoint-unavailable-before-session-ready|_error_code#9|AssertionError|raise AssertionError(f"expected the call to be rejected while the session is not ready, but it SUCCEEDED and returned {result!r}")
Issue-Commit: a6e1f04ac97a18473409adbfc59bb5a800ec5308

# athena: 2 net-new GetSessionEndpoint causes — a malformed SessionId gets the unknown-session "Not authorized" refusal instead of the sessionId pattern-validation message, and a session that is not yet IDLE is served an endpoint/token instead of being refused

> _Note: this issue was identified against the code base at commit `a6e1f04ac97a18473409adbfc59bb5a800ec5308`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSessionEndpoint

## Description
Two distinct net-new root causes on Athena's GetSessionEndpoint. Neither failing check below is marked ALREADY FILED or OWNED in the replay report. Related but NOT covering these: open issue 20261117T160651Z-athena-9-net-new-causes-the-provisioned-dpu.md (its Cause 5 reports GetSessionEndpoint answering NotImplemented for every id); the operation now answers, so these are the behaviors it answers WRONGLY. Both causes must be closed before this issue is submitted.

Cause 1 of 2: GetSessionEndpoint does not pattern-validate SessionId, so a malformed id is answered with the unknown-session authorization refusal instead of AWS's framework validation message.
AWS: Smithy: GetSessionEndpoint.SessionId is @required with @length 1..256 and the operation's modeled client errors are InvalidRequestException (400) and ResourceNotFoundException (400). Beyond the modeled length, the service applies a character constraint on sessionId (recorded behavior only — the model carries no @pattern): a SessionId holding characters outside the session-id charset is refused with InvalidRequestException (400) and the framework validation message "1 validation error detected: Value at 'sessionId' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[0-9a-z-_]+$". In that message everything is fixed text except the member name ('sessionId') and the quoted pattern, which come from the constraint being violated; the rejected value itself is not interpolated. The unknown-but-well-formed-id refusal is a DIFFERENT class: InvalidRequestException with the fixed text "Not authorized to make this request."
Emulator: the malformed id "!! not-a-session-id ??" is refused with the right code (InvalidRequestException) but the wrong message — "Not authorized to make this request.", i.e. the request is resolved against session state as an unknown session and never constraint-checked.
Why: a client cannot distinguish "I built a bad session id" from "this session is not mine / does not exist". The emulator sends every malformed-input caller down an IAM-debugging path for what AWS reports as a client-side input-validation error, and any client (or test) matching on AWS's "N validation error(s) detected" prefix to retry/repair the argument never sees it.
Fix: GetSessionEndpoint's input validation — apply the sessionId character constraint before resolving the id against session state, and emit AWS's framework validation message for a violation, keeping "Not authorized to make this request." only for a well-formed id the account does not hold.
Traces: athena.get-session-endpoint-malformed-id-rejected

Cause 2 of 2: GetSessionEndpoint ignores the session's state and serves an endpoint/token for a session that has not reached IDLE.
AWS: Smithy: GetSessionEndpoint returns @required EndpointUrl, AuthToken and AuthTokenExpirationTime; its modeled client errors are InvalidRequestException (400) and ResourceNotFoundException (400). A session is only addressable once it is IDLE: called after StartSession while GetSessionStatus still reports a pre-IDLE state (CREATING/PROVISIONING), the call is refused with InvalidRequestException (400) and the message "Can not generate Session endpoint URL for Session in PROVISIONING state" — fixed text with the session's CURRENT state substituted as the single variable part. Once the session reaches IDLE, the same call returns 200 with the endpoint URL, auth token and expiration.
Emulator: the pre-IDLE call SUCCEEDS with HTTP 200 and a full GetSessionEndpoint body (EndpointUrl, AuthToken, AuthTokenExpirationTime) — no state gate exists, so no refusal is produced at any session state before IDLE.
Why: a client that uses the refusal as its readiness signal (retry until the endpoint is generable) proceeds immediately with an endpoint and token for a session whose Spark resources are not yet acquired; against real AWS that same code path is refused and retries, so the readiness handling it is exercising here is the opposite of AWS's. The emulator is overly permissive: a negative class AWS enforces is unobservable.
Fix: GetSessionEndpoint's handler must read the session's state from session state and refuse any non-IDLE session with InvalidRequestException carrying AWS's "Can not generate Session endpoint URL for Session in <STATE> state" wording, serving the endpoint/token triple only for IDLE.
Traces: athena.session-endpoint-unavailable-before-session-ready

## Evidence
Cause 1: expected InvalidRequestException with message "1 validation error detected: Value at 'sessionId' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[0-9a-z-_]+$" for SessionId="!! not-a-session-id ??"; actual InvalidRequestException with message 'Not authorized to make this request.' (AssertionError: unexpected GetSessionEndpoint message: 'Not authorized to make this request.').

Cause 2: expected the pre-IDLE call to be refused with InvalidRequestException and "Can not generate Session endpoint URL for Session in PROVISIONING state"; actual HTTP 200 carrying EndpointUrl, AuthToken and AuthTokenExpirationTime (AssertionError: "expected the call to be rejected while the session is not ready, but it SUCCEEDED and returned {'EndpointUrl': ..., 'AuthToken': ..., 'AuthTokenExpirationTime': ..., 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}"). The check first asserted GetSessionStatus reported a state other than IDLE, so the session was demonstrably not ready when the endpoint was served. (The host:port, token bytes, expiration timestamp and request id in that body are per-run values, not requirements — the divergence is the 200 itself.)

## Steps to Reproduce
Cause 1: athena.get_session_endpoint(SessionId="!! not-a-session-id ??") and read Error.Code/Error.Message — the code is InvalidRequestException but the message is the unknown-session "Not authorized to make this request." instead of the sessionId pattern-validation text. Compare with a well-formed but never-issued id (e.g. str(uuid4())), which must keep the "Not authorized to make this request." wording.

Cause 2: create an execution role and a Spark workgroup (Configuration.ExecutionRole=<role arn>, EngineVersion.SelectedEngineVersion="Apache Spark version 3.5"); athena.start_session(WorkGroup=<wg>, EngineConfiguration={"MaxConcurrentDpus": 2}, SessionIdleTimeoutInMinutes=60); immediately athena.get_session_status(SessionId=<id>) and confirm State != "IDLE"; then athena.get_session_endpoint(SessionId=<id>) — it returns 200 with EndpointUrl/AuthToken/AuthTokenExpirationTime instead of raising InvalidRequestException.
