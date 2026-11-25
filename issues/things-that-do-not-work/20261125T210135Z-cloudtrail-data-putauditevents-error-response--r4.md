Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# A cross-account channel ARN is refused with the unmodelled AccessDeniedException instead of the modelled ChannelInsufficientPermission

_Filed: 2026-11-25T21:01:35Z_
_Operation: PutAuditEvents | Class: error-response_

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`PutAuditEvents` compares the account segment of the submitted `channelArn` against the emulator's own account and, when they differ, answers HTTP 403 with the error code `AccessDeniedException` — a code this service's model does not declare (its common_errors are exactly the six modelled shapes). That is precisely the condition the model documents for the modelled `ChannelInsufficientPermission` (HTTP 400): "The caller's account ID must be the same as the channel owner's account ID."

The crate contradicts itself on this point twice:

1. The SAME handler answers the SAME fault — a channel owned by another account — with the modelled `ChannelInsufficientPermission` when the reference arrived as a bare channel id (the registration's `owner_account_id` is compared there instead), so one fault reaches callers as two different error classes and two different statuses depending on the spelling of the reference.
2. The crate's own error-channel documentation states the rule that is broken here: `Scaffolded::access_denied` is to be reached for "where AUTHORIZATION refuses the call … A refusal the service's own model declares travels as [`Modeled`](Self::Modeled) instead." This refusal IS one the model declares.

The existing tests pin the 403 with a stated reason, but that reason is about WHEN the refusal is taken ("before the channel is looked up — so the answer is the same whether or not such a channel is registered here"), not about which code it carries: the modelled `ChannelInsufficientPermission` can be answered at exactly the same point, from the ARN's account segment alone, and discloses nothing about what the registry holds either. A client generated from this model has no handler for `AccessDeniedException` at all — the very argument `src/service.rs:189-190` makes about `ValidationException` ("`ValidationException` is not modelled anywhere in this service, so a generated client has no handler for it at all").

## Evidence

Smithy (smithy_query query="shape" shape="ChannelInsufficientPermission"): `"doc": "<p>The caller's account ID must be the same as the channel owner's account ID.</p>"`, `"errorKind": "client", "httpError": 400`, and it is declared on the operation (smithy_query query="operation" operation="PutAuditEvents": errors include `{"shape": "ChannelInsufficientPermission", … "httpError": 400}`). service_info lists `common_errors: [ChannelInsufficientPermission, ChannelNotFound, ChannelUnsupportedSchema, DuplicatedAuditEventId, InvalidChannelARN, UnsupportedOperationException]` — `AccessDeniedException` is not among them.

Handler, the ARN path — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/ops/put_audit_events.rs:409-422:
```
        if let crate::state::ChannelRef::Arn(arn) = &channel_ref {
            if arn.account_id != ::engine::emulator_account_id() {
                record_access(… crate::scaffolded::ACCESS_DENIED_CODE, "the channel ARN names another account");
                return Err(crate::scaffolded::Scaffolded::access_denied(…));
```

The same handler, the bare-id path for the same fault — put_audit_events.rs:450-464:
```
        if channel.owner_account_id != ::engine::emulator_account_id() {
            record_access(… "ChannelInsufficientPermission", "the channel is owned by another account");
            return Err(PutAuditEventsErrorModeled::ChannelInsufficientPermission(
                    ChannelInsufficientPermission { message: Some(CHANNEL_NOT_OWNED_MESSAGE.to_string()) },
```
with CHANNEL_NOT_OWNED_MESSAGE at put_audit_events.rs:172-173 being the model's own sentence.

The crate's stated rule it breaks — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/scaffolded.rs:321-325:
```
    /// Reach for this where AUTHORIZATION refuses the call — the caller may not act on
    /// the resource the request names — rather than for a malformed request
    /// ([`validation`](Self::validation)) or an unwritten operation
    /// ([`not_implemented`](Self::not_implemented)). A refusal the service's own model
    /// declares travels as [`Modeled`](Self::Modeled) instead.
```
and src/service.rs:189-190: "`ValidationException` is not modelled anywhere in this service, so a generated client has no handler for it at all."

Test coverage that pins the current 403 (and the reason it states, which is about ordering only): tests/all/channel_arn_refusals.rs:301-306 ("A channel ARN in another account is refused by AUTHORIZATION — HTTP 403, `AccessDeniedException` … taken before the channel is resolved, so the answer cannot report whether that channel exists") and tests/all/put_audit_events.rs:761-774 (rows expecting `403, "AccessDeniedException"`).

## Steps to Reproduce

1. Register a channel owned by another account, e.g. `arn:aws:cloudtrail:us-east-1:111122223333:channel/25364758-697a-4b8c-8d9e-0f1021324354` (tests/all/put_audit_events.rs:725-728 does exactly this).
2. POST /PutAuditEvents?channelArn=<that ARN> with one valid event.
3. Observe HTTP 403 with `__type: AccessDeniedException` — a code this service's model never declares — where the model declares `ChannelInsufficientPermission` (HTTP 400) for "The caller's account ID must be the same as the channel owner's account ID."
4. Register a channel under another account's ARN and call the same operation through the state API with the BARE channel id (state::parse_channel_ref of the suffix): the same fault is answered with the modelled `ChannelInsufficientPermission` instead, so one fault has two answers.
