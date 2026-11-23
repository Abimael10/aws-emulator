Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 61290e09beb771b33e9a8583fb4b2bda13d2dc30

# A request whose body the codec cannot read skips the channel-ARN-first rule and the access record: it is answered ValidationException from the codec instead of InvalidChannelARN, and nothing is recorded

_Filed: 2026-11-23T23:14:00Z_
_Operation: PutAuditEvents | Class: error-response_

> _Note: this issue was identified against the code base at commit `61290e09beb771b33e9a8583fb4b2bda13d2dc30`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The pre-handler layer this service states its refusals with (`validation::StatingSubmittedValues`) decides everything from the violation list the shared collecting walk returns. That walk returns an EMPTY list for a request whose body the codec cannot read at all (a type mismatch, e.g. `auditEvents` sent as a JSON string, or a document the JSON grammar cannot hold) — only `MissingMember` is turned into a violation; every other codec error becomes `Refusing::default()`. With no violations, `refusal_of` returns `None`, the wrapper neither refuses nor records, and the request is handed to the generated operation, which fails the SAME deserialize and is answered by the framework's own renderer.

Two rules this crate states for itself break on that path:

1. The channel reference is no longer answered first. The crate says a value outside `ChannelArn`'s modelled `@pattern` is answered under `InvalidChannelARN` "whatever else the request carried", and that a refusal carrying other violations too is still answered as the channel-ARN fault. But for `POST /PutAuditEvents?channelArn=not-an-arn` with an unreadable body, the pattern violation is never produced (the pattern is only checked by `constraints::validate` AFTER a successful deserialize), so the caller is answered 400 `ValidationException` carrying the codec's own message — a third spelling of "not a channel ARN", under a code this service's model does not declare for it — instead of `InvalidChannelARN` with the sentence the model documents. The service's `override_refusal` mapping cannot save it either: the framework only consults `override_refusal` for refusals it can describe structurally, and a non-`MissingMember` codec error is not one of them (`refusal_of` returns `None`).

2. No access record is emitted. The crate states that every request this service ANSWERS — served or refused, whichever layer answered it — emits exactly one `record_access`. On this path the authorization gate records nothing (it allows the call), the wrapper records nothing (it refuses nothing) and the handler never runs, so the request is answered with no record at all.

This is a different path from the already-FIXED issue "The pre-handler refusal layer answers a request without emitting the access record…": there the wrapper answered the request and failed to record it; here the wrapper does not answer the request at all, so both the record and the channel-ARN precedence are skipped.

## Evidence

The rule the crate states for precedence — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/service/validation.rs:181-185:

    /// A channel reference comes FIRST, whatever else the request carried: this service
    /// resolves every call against the channel the request names before it reads the payload,
    /// so a value that is not a channel reference is answered under the error its model
    /// declares for that fault rather than as one constraint violation among several (see
    /// [`super::refuses_channel_arn`]).

and src/service.rs:76-84:

    /// A refusal that ALSO carries other violations is still answered as the channel-ARN
    /// fault: the channel reference is what this service resolves the call against before it
    /// reads anything else, so that is the fault it reports.

The rule the crate states for the access record — src/ops/put_audit_events.rs:288-291:

    /// Reached by every layer that ANSWERS a request of this service — the authorization gate
    /// in front of this operation ([`crate::service::authorization`]), the wrapper that states
    /// a refusal found before this handler runs ([`crate::service`]'s `validation`), and the
    /// handler below — so every request this service answers, served or refused and whichever
    /// layer refused it, is recorded once and in one shape.

What the wrapper actually does with an empty violation list — src/service/validation.rs:131-158:

            let collected: Vec<(Violation, SubmittedEcho)> =
                ::service_common::validation::refusing_violations_with_declared_echoes_rendering_up_to::<Op>(
                    svc.definition().protocol(),
                    req,
                    MAX_ECHO_POSITIONS,
                    MAX_QUOTED_BATCH,
                );
            if let Some(PreHandlerRefusal { refused, detail }) = refusal_of(&collected) {
                … record_access(…) … return svc.definition().refusal_response(…);
            }
            self.operation.run(svc, req, ctx).await

and src/service/validation.rs:194-197:

    fn refusal_of(collected: &[(Violation, SubmittedEcho)]) -> Option<PreHandlerRefusal> {
        if collected.is_empty() {
            return None;
        }

Why the list is empty for an unreadable body — services/common/src/validation.rs:1050 and :1064 (the arms for the `deserialize` result):

        Err(SerdeError::MissingMember { member_name }) => Refusing { … }
        Err(_) => Refusing::default(),

(the `@pattern` check that would have produced the channel-ARN violation only runs on the Ok arm, services/common/src/validation.rs:831: `let violations = constraints::validate(&input, schema);`).

What answers the request instead — vendor/smithy-server-runtime/src/operation.rs:477-478:

        let input = <O::Input as crate::DeserializableStruct>::deserialize(&mut *deser)
            .map_err(aws_smithy_schema_server::ProtocolError::Serde)?;

rendered at vendor/smithy-server-runtime/src/pre_dispatch.rs:447 with no service override consulted (`chosen` is `None` because vendor/smithy-server-runtime/src/pre_dispatch.rs:198-212 `refusal_of` maps only `MissingMember`, `Invalid` and `Unauthenticated`, `_ => None`):

        ProtocolError::Serde(e) => (400, "ValidationException", serde_message(&e)),

Test coverage: the crate's rule-0 table pins the record and the code for every OTHER layer — tests/all/constraint_refusals.rs:503-513 ("RULE: every request this service answers emits exactly one structured access record — served or refused, and whichever of its layers answered it") with rows for a non-ARN channel reference (:530-539, expecting `InvalidChannelARN` and outcome `InvalidChannelARN`), an over-long batch, an element constraint and the external id (:540-579) — but no row carries a body the codec cannot read, so this path is untested.

## Steps to Reproduce

1. Mount the service on the shared engine (as tests/all/put_audit_events.rs `Harness::new()` does) and sign with a credential the authorization gate allows (e.g. the default `AKIAEMULATORTEST`).
2. Control: send `POST /PutAuditEvents?channelArn=not-an-arn` with the well-formed body `{"auditEvents":[{"id":"evt-1","eventData":"{}"}]}`. The answer is 400 `InvalidChannelARN` with "The specified channel ARN is not a valid channel ARN." and one `PutAuditEvents access record` is emitted (what tests/all/constraint_refusals.rs:530-539 pins).
3. Now send the same URI with a body the codec cannot read, e.g. `{"auditEvents":"evt-1"}` (a JSON string where `auditEvents` is modelled as a list) or `{"auditEvents":[{"id":7,"eventData":"{}"}]}`.
4. Observe the answer is 400 with `__type` `ValidationException` and the codec's own message — not `InvalidChannelARN`, although the request's `channelArn` is outside the member's modelled `^arn:.*$` and the crate states that fault is answered first "whatever else the request carried".
5. Observe the tracing output for that request: no `PutAuditEvents access record` line is emitted, although the crate states every request this service answers is recorded once (src/ops/put_audit_events.rs:288-291).
6. Repeat step 3 with a REGISTERED, well-formed `channelArn`: still 400 `ValidationException` with no access record, so the hole is not specific to the bad channel reference.
