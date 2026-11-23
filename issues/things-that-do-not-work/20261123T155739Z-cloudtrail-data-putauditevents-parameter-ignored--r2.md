Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: a5a3c6bd63789eb42e1283a455a4e127273ca01a

# The authorization gate drops the bound channelArn (judging "*") whenever the query parameter NAME carries a percent-escape, so the gate and the handler disagree about the resource

_Filed: 2026-11-23T15:57:39Z_
_Operation: PutAuditEvents | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `a5a3c6bd63789eb42e1283a455a4e127273ca01a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The IAM gate in front of PutAuditEvents reads the channel reference off the raw query string with `service_common::query_parameter`, which compares the parameter NAME byte-for-byte as written. The protocol deserializer that later binds the same `@httpQuery("channelArn")` member percent-decodes the parameter name before looking it up. So for a request whose parameter name carries any percent-escape — e.g. `POST /PutAuditEvents?channelAr%6E=arn:aws:cloudtrail:<region>:<account>:channel/<uuid>` (`%6E` = `n`) — the two layers read the request differently:

* the gate finds no `channelArn` key, falls back to `ANY_CHANNEL` (`"*"`) and evaluates the caller's identity policies for `cloudtrail-data:PutAuditEvents` on the resource `*`;
* the handler receives `channel_arn` bound to the real ARN and ingests into that channel.

The authorization decision is therefore taken against a resource the request does not name. Both directions diverge: a principal whose grant is scoped to that channel ARN is refused 403 (the shared evaluator globs the statement's `Resource` pattern against the request resource string, so a statement naming `arn:aws:cloudtrail:...:channel/<uuid>` does not cover the literal `*`), while an explicit `Deny` scoped to that one channel is never consulted and the event is ingested. This contradicts the gate's own stated rule, which claims parity with the deserializer for every spelling of the value.

The crate's own unit table for this rule exercises percent-encoded VALUES and repeated parameters but never an encoded NAME, so nothing pins the current behaviour.

## Evidence

The claimed rule, services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/service/authorization.rs:128-131:

    /// The reference is the value of the query parameter the model binds it to
    /// ([`CHANNEL_ARN_PARAMETER`]), percent-decoded the way the protocol's own deserializer
    /// decodes a query-bound member, so the gate judges the very value a handler would see —
    /// whatever spelling it takes, including one the member's modelled `@pattern` rejects.

What the gate actually calls, authorization.rs:146-151:

    fn channel_reference_named(uri: &str) -> Option<String> {
        let (_, query) = uri.split_once('?')?;
        let reference = ::service_common::query_parameter(query, CHANNEL_ARN_PARAMETER)
            .map(::smithy_server_runtime::percent_decode)?;

and the fallback, authorization.rs:137-139:

    pub fn judged_resource(req: &RtRequest) -> String {
        channel_reference_named(req.uri()).unwrap_or_else(|| ANY_CHANNEL.to_string())
    }

(`const ANY_CHANNEL: &str = "*";` authorization.rs:103).

The raw-key comparison, services/common/src/lib.rs:1284-1293:

    /// Parameter NAMES in a SigV4 query are spelled literally, so the name is compared as
    /// written; ...
    pub fn query_parameter<'q>(query: &'q str, name: &str) -> Option<&'q str> {
        query
            .split('&')
            .filter_map(|item| item.split_once('='))
            .find_map(|(key, value)| (key == name).then_some(value))
    }

The deserializer the comment claims parity with decodes the NAME too, vendor/smithy-schema-server/src/http_binding.rs:104-110:

            for pair in query_str.split('&') {
                let mut it = pair.splitn(2, '=');
                let k = percent_decode_query(it.next().unwrap_or(""));
                let v = percent_decode_query(it.next().unwrap_or(""));
                query.entry(k.clone()).or_insert_with(|| v.clone());

The handler then serves whatever the deserializer bound, src/ops/put_audit_events.rs:382 (`crate::state::parse_channel_ref(&input.channel_arn)`) and :426 (`guard.channel_mut(&channel_ref)`), while the gate already allowed or refused the call on `"*"` (authorization.rs:196-197: `let resource = judged_resource(req); if let Some(denial) = refusal(&*self.engine, ctx, self.operation, &resource)`).

Resource matching is a glob of the statement pattern against the request resource — services/common/src/iam_authorization.rs:1421: `Some(pattern) => hit = hit || pattern.matches(request.resource, true),` — so a channel-scoped statement does not cover the request resource `"*"`, which the crate's own test asserts for the inverse case (authorization.rs:692-727, `a_grant_is_read_for_the_channel_the_request_named`).

Test coverage: authorization.rs:561-634 `the_resource_judged_is_the_bound_channel_reference` covers "percent-encoded, as an SDK writes a query value" (:572-576), "the first of two spellings" (:587-591) and "bound as a bare key" (:619-623) — no row carries an escaped parameter NAME, so this divergence is untested.

## Steps to Reproduce

1. Register a channel in the emulator account: `Channel::from_arn("arn:aws:cloudtrail:us-east-1:000000000000:channel/<uuid>", "Custom")`.
2. Create an IAM role/user whose identity policy allows `cloudtrail-data:PutAuditEvents` ONLY on that channel ARN (Resource = the channel ARN), and sign with its credentials.
3. Send `POST /PutAuditEvents?channelArn=arn:aws:cloudtrail:us-east-1:000000000000:channel/<uuid>` with one valid event — the gate judges the ARN, the grant covers it, the batch is ingested (control).
4. Re-send the byte-identical request with the parameter name escaped: `POST /PutAuditEvents?channelAr%6E=arn:aws:cloudtrail:us-east-1:000000000000:channel/<uuid>`. The deserializer still binds `channelArn`, but `judged_resource` returns `"*"`, the channel-scoped grant does not cover it, and the call is refused 403 AccessDeniedException although the handler would have served it.
5. Inverse: give the principal `Allow cloudtrail-data:* on *` plus `Deny cloudtrail-data:PutAuditEvents on <that channel ARN>`. The plain spelling is denied; the escaped-name spelling is allowed and the events are appended to the denied channel's ingest log (`Channel::events`).
