Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connectparticipant.authentication-operations-invalid-session-id-rejected
Issue-Shape-Id: com.amazonaws.connectparticipant#AmazonConnectParticipantServiceLambda
Issue-Key: connectparticipant.authentication-operations-invalid-session-id-rejected|run#6|AssertionError|assert url_err == ("ValidationException", _INVALID_PARAMS), f"got {url_err!r}"
Issue-Commit: f788da56fca8042d5f8354f4ce9bfd6169341434

# Implement connectparticipant GetAuthenticationUrl: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `f788da56fca8042d5f8354f4ce9bfd6169341434`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAuthenticationUrl

## Description
`GetAuthenticationUrl` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/ops/get_authentication_url.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `GetAuthenticationUrl` call, so each one unblocks only when `GetAuthenticationUrl` works: `connectparticipant.authentication-operations-invalid-session-id-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/ops/get_authentication_url.rs`. Replaying the corpus, the emulator refused the `GetAuthenticationUrl` call: File "run.py", line 348, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_connectparticipant_authentication_operations_invalid_session_id_rejected.py", line 37, in run
    assert url_err == ("ValidationException", _INVALID_PARAMS), f"got {url_err!r}"
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: got ('NotImplemented', 'operation GetAuthenticationUrl is scaffolded but not implemented in this emulator')

## Steps to Reproduce
Boot the connectparticipant emulator
Call GetAuthenticationUrl and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal

## Resolution

`GetAuthenticationUrl` and `CancelParticipantAuthentication` are implemented over a stored
customer-authentication session (`src/state/authentication.rs`): the sign-in URL is composed
from the session's Cognito authorize endpoint, app client and scopes plus the request's
`RedirectUri` and `SessionId`; the retrieval writes nothing, the cancel closes the session,
and a `SessionId` naming no current session of the caller's own chat is refused HTTP 400
`ValidationException`, a connection token that authenticates nobody HTTP 403
`AccessDeniedException`. Both scaffold census lines are gone and the suites in
`tests/all/chat_participant_slice.rs` cover the class over the wire.

Residual divergence, left open on purpose: the recorded unit asserts the
`ValidationException` MESSAGE against a constant the report does not quote, so the exact
text AWS returns for that refusal class is not recoverable from this evidence. The status
(400) and the code (`ValidationException`) match the recording; the message is worded the
way this service words a rejected input. A later run with the recorded text in hand should
replace that one string.
