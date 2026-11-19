Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 4e953d0ceb021d373a4669be7884bbadfda507a7

# A pre-handler refusal bypasses the authorization gate, so the two spellings of "not a channel ARN" reach an unentitled caller as two different errors

_Filed: 2026-11-19T04:10:49Z_
_Operation: PutAuditEvents | Class: error-response_

> _Note: this issue was identified against the code base at commit `4e953d0ceb021d373a4669be7884bbadfda507a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states two rules about the order in which `PutAuditEvents` refuses a call, and the code cannot honour both.

Rule 1 (the handler): authorization is decided first, for every request — `src/ops/put_audit_events.rs:333-338` says "AUTHORIZATION FIRST, as IAM decides a signed call ahead of the service ... A principal no statement allows is refused before anything of this service is read".

Rule 2 (the same handler, about the channel reference): a value that is not a channel ARN is answered as ONE error whichever of its two spellings the caller sends — `src/ops/put_audit_events.rs:375-379` says a value that does not start with `arn:` "fails the member's modelled `@pattern` at the boundary and is answered as this same refusal there (see `crate::service`'s refusal override), so the two spellings of \"not a channel ARN\" reach the caller as one error".

The boundary spelling is answered by the wrapper in `src/service/validation.rs`, which returns the refusal response WITHOUT ever calling the operation (`self.operation.run(..)` is reached only when there are no violations), and the authorization gate lives inside the operation's `handle`. So the gate runs for one spelling and not for the other, and for a principal whose identity-based policies allow nothing the same fault class comes back as two different errors:

* `channelArn=not-an-arn` (no `arn:` prefix) → HTTP 400 `InvalidChannelARN`, decided in the wrapper with NO authorization decision at all;
* `channelArn=arn:aws:s3:::a-bucket-name` (an `arn:` value that is not a channel ARN) → HTTP 403 `AccessDeniedException`, because the handler's gate runs before `parse_channel_ref`.

The same divergence covers every other pre-handler refusal: an unentitled caller that also breaks `auditEvents`' `@length` or `AuditEvent$id`'s `@pattern` is answered 400 `ValidationException` whose message QUOTES the batch the request carried (`src/service/validation.rs:134-168`, `tests/all/clause_value_slot.rs`), although the handler's comment says such a principal is "refused before anything of this service is read". Which of the two rules the fixer keeps is their choice; what is filed here is that the crate currently breaks one of them, and that the error class a caller sees for one fault depends on the spelling of the value.

## Evidence

Rule 1 — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/ops/put_audit_events.rs:333-338:
```
        // AUTHORIZATION FIRST, as IAM decides a signed call ahead of the service: the
        // calling principal's identity-based policies are evaluated for this operation's
        // own action on the channel reference the request carried ...
        // A principal no statement allows is refused
        // before anything of this service is read, so the answer reports neither whether
        // that channel exists nor any per-event outcome.
```
and the gate it introduces, put_audit_events.rs:339-352 (`if let Some(denial) = crate::service::authorization::refusal(...) { ... return Err(crate::scaffolded::Scaffolded::access_denied(denial)); }`) — inside `handle`.

Rule 2 — the same file, put_audit_events.rs:375-379:
```
        // `channelArn` is "The ARN or ID (the ARN suffix) of a channel"; a value that
        // names neither is not a channel ARN. A value that does not even start with
        // `arn:` fails the member's modelled `@pattern` at the boundary and is answered
        // as this same refusal there (see `crate::service`'s refusal override), so the
        // two spellings of "not a channel ARN" reach the caller as one error.
```

The other side — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/service/validation.rs:114-123, which answers the boundary spelling and returns BEFORE the operation (and therefore before the gate) ever runs:
```
            if let Some(refused) = refusal_of(&collected) {
                let violations: Vec<Violation> = ...;
                return svc.definition().refusal_response(
                    aws_smithy_schema_server::ProtocolError::Invalid(violations),
                    Some(refused),
                    req,
                );
            }
            self.operation.run(svc, req, ctx).await
```
and validation.rs:147-159, which maps a `^arn:.*$` pattern violation to `Refused { code: INVALID_CHANNEL_ARN_CODE, message: INVALID_CHANNEL_ARN_MESSAGE }`. This wrapper is what serves the operation (src/service.rs:103-109, src/service.rs:135-147).

Test coverage: no test pins the unentitled-caller × pre-handler-refusal combination, so nothing in the crate states a reason for the divergence. tests/all/channel_arn_refusals.rs:245-290 ("a_channel_reference_outside_the_modelled_pattern_is_the_services_own_refusal", rows including "a bare word", "an empty value") and tests/all/constraint_refusals.rs all sign with `DEFAULT_ACCESS_KEY_ID`, documented in tests/all/put_audit_events.rs:26-28 as "a key neither STS nor IAM holds a record for, which is the position the account's own published credential is in" — i.e. the one caller authorization never refuses (src/service/authorization.rs:54-59: "Such a request is allowed."). Conversely every row of tests/all/identity_authorization.rs uses a well-formed channel ARN (`channel_arn(..)`), so the unentitled caller is only ever exercised on the handler side of the split.

## Steps to Reproduce

1. Build the harness of tests/all/put_audit_events.rs on a fresh engine and seed an assumed-role session whose role policy allows only `cloudtrail:*` — exactly `given_session(harness.engine())` + `given_role_policy(harness.engine(), Some(&allow(json!("cloudtrail:*"))))` from tests/all/identity_authorization.rs:459-461.
2. Call `harness.put_as(SESSION_KEY_ID, "arn:aws:s3:::a-bucket-name", None, &[ev("evt-1", GOOD_EVENT)])` — an `arn:` value that is not a channel ARN. Observe HTTP 403 `AccessDeniedException`: the handler's gate ran first.
3. Call `harness.put_as(SESSION_KEY_ID, "not-an-arn", None, &[ev("evt-1", GOOD_EVENT)])` — the other spelling of the same fault. Observe HTTP 400 `InvalidChannelARN` with "The specified channel ARN is not a valid channel ARN.": the wrapper in src/service/validation.rs answered it and the authorization gate never ran.
4. Repeat step 3 with a well-formed registered channel ARN but 101 events (past `auditEvents`' `@length` maximum): observe HTTP 400 `ValidationException` whose message quotes the whole submitted batch back to a principal no statement allows, although put_audit_events.rs:337 says such a principal is "refused before anything of this service is read".
