Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sts.get-web-identity-token-duration-escalation-rejected sts.get-web-identity-token-session-duration-escalation-rejected sts.get-web-identity-token-outbound-federation-disabled-blocked sts.get-web-identity-token-outbound-federation-disabled-rejected sts.get-web-identity-token-payload-size-exceeded-rejected
Issue-Shape-Id: com.amazonaws.sts#AWSSecurityTokenServiceV20110615
Issue-Key: sts.get-web-identity-token-duration-escalation-rejected|run#12|AssertionError|assert message == expected_message, (
Issue-Key: sts.get-web-identity-token-session-duration-escalation-rejected|run#22|AssertionError|assert message == expected_message, (
Issue-Key: sts.get-web-identity-token-outbound-federation-disabled-blocked|run#15|AssertionError|assert message == expected_message, (
Issue-Key: sts.get-web-identity-token-outbound-federation-disabled-rejected|run#11|AssertionError|assert message == expected_message, (
Issue-Key: sts.get-web-identity-token-payload-size-exceeded-rejected|run#12|AssertionError|assert message == expected_message, (
Issue-Commit: 9faa0412eef4ef0c0869e5f9ba25231d4242bf3e

# STS GetWebIdentityToken: all three modelled refusals serve the Smithy doc paragraph as the error Message instead of AWS's wire message text (3 net-new causes)

> _Note: this issue was identified against the code base at commit `9faa0412eef4ef0c0869e5f9ba25231d4242bf3e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetWebIdentityToken

## Description
GetWebIdentityToken refuses three conditions with the right error Code and HTTP status, but the `Message` of every one of them is the shape's DOCUMENTATION paragraph from the Smithy model rather than the short message real STS puts on the wire. Three distinct messages are wrong, each fixed in its own helper, so this is three causes bundled for one service.

Shared mechanism (same for all three): `services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/src/state/outbound_web_identity.rs` defines one `*_message()` helper per refusal and each returns the model's `doc` prose verbatim; `src/ops/get_web_identity_token.rs` plugs that string into the error's `message` member. The model's `doc` text is API-reference prose, not the wire `Message`.

---
Cause 1 of 3: SessionDurationEscalationException message.
AWS: Smithy: GetWebIdentityToken error `SessionDurationEscalationException`, errorKind client, httpError 403, awsQueryError code `SessionDurationEscalationException`. The model carries no wire message text (its `doc` is reference prose), so for the message itself: recorded behavior only — when a session-credentialed caller asks for a JWT whose expiry is after the calling session's expiry, STS answers HTTP 403, Code `SessionDurationEscalationException`, Message exactly "Requested token expiry time must be before the original session's expiry time." That message is entirely FIXED text: it interpolates nothing from the request or from state (no duration, no timestamp, no session name).
Emulator: same status and Code, but Message is 'The requested token duration would extend the session beyond its original expiration time. You cannot use this operation to extend the lifetime of a session beyond what was granted when the session was originally created.' — the model's doc paragraph.
Why: a client (or a test, or a log-matching runbook) that keys on STS's refusal text for a duration-escalation attempt never matches, and an operator reading the emulator's reply is told to re-create the session rather than that the requested expiry must precede the session's expiry. The refusal is correctly classified but not recognisable as AWS's.
Fix: the message helper in the operation's state module — `session_duration_escalation_message()` — must return AWS's fixed refusal text; the handler's refusal branch stays as is.
Traces: sts.get-web-identity-token-duration-escalation-rejected, sts.get-web-identity-token-session-duration-escalation-rejected.

---
Cause 2 of 3: OutboundWebIdentityFederationDisabledException message.
AWS: Smithy: GetWebIdentityToken error `OutboundWebIdentityFederationDisabledException`, errorKind client, httpError 403, awsQueryError code `OutboundWebIdentityFederationDisabledException`. Message text: recorded behavior only — with the account-level outbound web-identity federation feature OFF, STS answers HTTP 403, Code `OutboundWebIdentityFederationDisabledException`, Message exactly "OutboundWebIdentityFederation is disabled." All fixed text; nothing is taken from the request or from state (no account id, no issuer URL).
Emulator: same status and Code, Message 'The outbound web identity federation feature is not enabled for this account. To use this feature, you must first enable it through the Amazon Web Services Management Console or API.' — again the model's doc paragraph.
Why: callers that distinguish "feature off" from other 403s by message text, and the harnesses that poll until the feature-disabled refusal stops, see a message AWS never sends.
Fix: the message helper in the operation's state module — `outbound_web_identity_federation_disabled_message()`.
Traces: sts.get-web-identity-token-outbound-federation-disabled-blocked, sts.get-web-identity-token-outbound-federation-disabled-rejected.

---
Cause 3 of 3: JWTPayloadSizeExceededException message.
AWS: Smithy: GetWebIdentityToken error `JWTPayloadSizeExceededException`, errorKind client, httpError 400, awsQueryError code `JWTPayloadSizeExceededException`; input member `Tags` is `tagListType` with @length 0..50, the member whose size drives the payload. Message text: recorded behavior only — when the claim set built from the request's tags exceeds the token payload budget, STS answers HTTP 400, Code `JWTPayloadSizeExceededException`, Message exactly "JWT payload exceeds maximum allowed size" (fixed text, no trailing period, no interpolated byte count or limit).
Emulator: same status and Code, Message 'The requested token payload size exceeds the maximum allowed size. Reduce the number of request tags included in the GetWebIdentityToken API call to reduce the token payload size.' — the model's doc paragraph.
Why: the size-limit refusal is not matchable by text, so clients that classify oversized-token requests by message fall through to generic 400 handling.
Fix: the message helper in the operation's state module — `jwt_payload_size_exceeded_message()`.
Traces: sts.get-web-identity-token-payload-size-exceeded-rejected.

---
Note on scope: the status codes, error Codes and response shapes of all three refusals already match AWS; only the Message strings diverge. A general fix is to stop sourcing wire `Message` text from the model's `doc` prose for these three shapes.

## Evidence
Cause 1 — sts.get-web-identity-token-duration-escalation-rejected (and ...-session-duration-escalation-rejected):
  AssertionError: expected the refusal message "Requested token expiry time must be before the original session's expiry time.", got 'The requested token duration would extend the session beyond its original expiration time. You cannot use this operation to extend the lifetime of a session beyond what was granted when the session was originally created.'
  (Code asserted and matched: SessionDurationEscalationException; HTTP 403.)

Cause 2 — sts.get-web-identity-token-outbound-federation-disabled-blocked (and ...-disabled-rejected):
  AssertionError: expected the refusal message 'OutboundWebIdentityFederation is disabled.', got 'The outbound web identity federation feature is not enabled for this account. To use this feature, you must first enable it through the Amazon Web Services Management Console or API.'
  (Code asserted and matched: OutboundWebIdentityFederationDisabledException; HTTP 403.)

Cause 3 — sts.get-web-identity-token-payload-size-exceeded-rejected:
  AssertionError: expected the refusal message 'JWT payload exceeds maximum allowed size', got 'The requested token payload size exceeds the maximum allowed size. Reduce the number of request tags included in the GetWebIdentityToken API call to reduce the token payload size.'
  (Code asserted and matched: JWTPayloadSizeExceededException; HTTP 400.)

Source of the divergence in the checkout:
  services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/src/state/outbound_web_identity.rs:132 jwt_payload_size_exceeded_message()
  services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/src/state/outbound_web_identity.rs:141 session_duration_escalation_message()
  services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/src/state/outbound_web_identity.rs:151 outbound_web_identity_federation_disabled_message()
  consumed at services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/src/ops/get_web_identity_token.rs:254, :288, :309 — each returns the corresponding Smithy shape `doc` paragraph (compare smithy_query shape=JWTPayloadSizeExceededException / SessionDurationEscalationException / OutboundWebIdentityFederationDisabledException, whose doc strings are byte-for-byte what the emulator sends).

## Steps to Reproduce
Cause 1 (SessionDurationEscalationException):
1. iam:EnableOutboundWebIdentityFederation for the account (if off).
2. Create a role trusting the caller with sts:GetWebIdentityToken allowed; sts:AssumeRole it with DurationSeconds=900.
3. With the session credentials, call sts:GetWebIdentityToken(Audience=["https://example.com"], SigningAlgorithm="RS256", DurationSeconds=3600) — a duration outliving the 900s session.
4. Observe HTTP 403 / Code SessionDurationEscalationException with the model's doc paragraph as Message instead of "Requested token expiry time must be before the original session's expiry time."

Cause 2 (OutboundWebIdentityFederationDisabledException):
1. Ensure outbound web-identity federation is DISABLED (iam:DisableOutboundWebIdentityFederation).
2. Call sts:GetWebIdentityToken(Audience=["https://example.com"], SigningAlgorithm="RS256", DurationSeconds=60).
3. Observe HTTP 403 / Code OutboundWebIdentityFederationDisabledException with the doc paragraph as Message instead of "OutboundWebIdentityFederation is disabled."

Cause 3 (JWTPayloadSizeExceededException):
1. Enable outbound web-identity federation.
2. Call sts:GetWebIdentityToken with 50 maximally-sized Tags so the claim set exceeds the token payload budget.
3. Observe HTTP 400 / Code JWTPayloadSizeExceededException with the doc paragraph as Message instead of "JWT payload exceeds maximum allowed size".

Re-replay: the five checks listed in trace_ids.
