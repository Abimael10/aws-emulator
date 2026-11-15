Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: f41cd6914a3022b28f4b3ca74f33b5eb7d339de6

# An idempotent AcceptConnectionProposal retry is refused 400 after an UpdateConnection/UntagResource, because the replay is judged against the live record instead of the acceptance that was made

_Filed: 2026-11-15T21:49:09Z_
_Operation: AcceptConnectionProposal | Class: error-response_

> _Note: this issue was identified against the code base at commit `f41cd6914a3022b28f4b3ca74f33b5eb7d339de6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`AcceptConnectionProposal` is modelled `@idempotent` with an `@idempotencyToken` `clientToken`, and the handler explicitly declines to keep a replay index, claiming the operation is "idempotent by construction". It decides whether a repeat is the SAME acceptance by `crate::state::acceptance_already_applied`, which compares the request's `description` and `tags` against the values the CONNECTION RECORD currently holds — not against what the acceptance asked for.

The record is mutable by other operations after the acceptance: `UpdateConnection` assigns `stored.connection.description`, and `UntagResource` removes keys from `stored.connection.tags`. Once either has run, a byte-for-byte replay of the acceptance that already succeeded no longer "matches" the record, so the handler falls into the already-accepted refusal and answers HTTP 400 `InterconnectClientException` instead of replaying the connection. The refusal is additionally worded as an attach-point change ("whose attach point cannot be changed") even though the replay carries the identical attach point and asks to change nothing.

The crate already states the opposite rule for its sibling idempotency judgement, and states it for exactly this hazard: `IdempotentCreate` deliberately keeps the request's own members "rather than read back off the stored connection, because `UpdateConnection` may since have changed the record's description or bandwidth: idempotency is about the request that was made, not about the resource's current shape." `CreateConnection` therefore survives an intervening `UpdateConnection`; `AcceptConnectionProposal`, which writes the same two caller-owned members, does the very thing that doc says is wrong.

The existing suite pins only "a SECOND acceptance asking for another description/tag value is refused" (no intervening write), so recording what the acceptance asked for — the way `IdempotentCreate` does — keeps that assertion green while making the replay-after-update case the 200 replay the `@idempotent` trait asks for.

## Evidence

The model marks the operation idempotent and its token an idempotency token — `smithy_query(query="operation",operation="AcceptConnectionProposal")`:
```
"clientToken": {"target": "String", "idempotencyToken": true}, ... "idempotent": true
```

The handler's claim that no replay index is needed — services/service-com-amazonaws-interconnect-interconnect/src/ops/accept_connection_proposal.rs:413-416:
```
    /// `clientToken` needs no replay index of its own: this acceptance is idempotent
    /// by construction, because it makes no new resource — it accepts the one proposal
    /// the key names, and a repeat asking for what that connection already holds is
    /// answered with that same connection.
```

The judgement it relies on, and the refusal it falls into — src/ops/accept_connection_proposal.rs:337-353:
```
    if stored.proposal_accepted {
        if crate::state::acceptance_already_applied(
            connection,
            &input.attach_point,
            input.description.as_ref(),
            input.tags.as_ref(),
        ) {
            return Ok(StoreVerdict::AlreadyAccepted(Box::new(connection.clone())));
        }
        return Err(
            AcceptConnectionProposalErrorModeled::InterconnectClientException(
                InterconnectClientException {
                    message: crate::state::proposal_already_accepted_message(&connection.id),
                },
            )
            .into(),
        );
```

The comparison is against the LIVE record — src/state.rs:1762-1779:
```
pub fn acceptance_already_applied(
    connection: &Connection,
    attach_point: &AttachPoint,
    description: Option<&String>,
    tags: Option<&std::collections::HashMap<String, String>>,
) -> bool {
    attach_point_eq(&connection.attach_point, attach_point)
        && description.is_none_or(|supplied| *supplied == connection.description)
        && tags.is_none_or(|supplied| {
            supplied.iter().all(|(key, value)| {
                connection
                    .tags
                    .as_ref()
                    .and_then(|held| held.get(key))
                    .is_some_and(|held| held == value)
            })
        })
}
```

The SAME crate states the contrary rule for its other idempotency index, naming this exact hazard — src/state.rs:255-260:
```
/// The request-owned members are kept here rather than read back off the stored
/// connection, because `UpdateConnection` may since have changed the record's
/// description or bandwidth: idempotency is about the request that was made, not
/// about the resource's current shape. (`remoteAccount` is not on the resource at
/// all — `Connection` models no such member — so the request is the only place it
/// could be compared from.)
```

The writers that move the record out from under the replay — src/ops/update_connection.rs:248-250:
```
        if let Some(description) = input.description.clone() {
            stored.connection.description = description;
        }
```
and src/ops/untag_resource.rs:242:
```
        crate::state::untag_tags(&mut stored.connection.tags, &input.tag_keys);
```
while the acceptance is the writer of those same two members — src/state.rs:1718-1725:
```
    stored.connection.attach_point = attach_point.clone();
    if let Some(description) = description {
        stored.connection.description = description.clone();
    }
    ...
    merge_tags(&mut stored.connection.tags, tags);
```

The refusal's wording, which names a member the replay never changed — src/state.rs:1786-1791:
```
pub fn proposal_already_accepted_message(connection_id: &str) -> String {
    format!(
        "{REFUSAL_ENVELOPE}The connection proposal for the supplied activationKey has already \
         been accepted as connection {connection_id}, whose attach point cannot be changed"
    )
}
```

Test coverage: tests/all/accept_connection_proposal.rs:723-831 pins a repeated acceptance reporting the same connection and pins the 400 for a second acceptance that differs in attach point, description or tag value — but every one of those rows sends the differing request directly, with NO intervening `UpdateConnection`/`UntagResource` (tests/all/accept_connection_proposal.rs:775-803), so the replay-after-write case is unpinned. tests/all/update_connection_idempotent_retry.rs covers only `UpdateConnection`'s own retry.

## Steps to Reproduce

1. Boot the emulator, point an Interconnect client at one region, and create a Direct Connect gateway so the account holds a valid attach point.
2. Call ListEnvironments to get the called Region's environmentId and an available bandwidth.
3. CreateConnection on that environment; note the returned connection's `id` and `activationKey`.
4. Build the acceptance request R = AcceptConnectionProposal(activationKey=<that key>, attachPoint={"directConnectGateway": <gateway id>}, description="my conn", clientToken="t-1") and send it. Observe HTTP 200 and a connection whose description is "my conn".
5. Send R again immediately. Observe HTTP 200 and the same connection — the replay works while nothing else has written the record.
6. Wait ~300 ms so the clock carries the connection from `pending` to `available` (src/state.rs:154-164), then call UpdateConnection(identifier=<id>, description="renamed"). Observe HTTP 200 and description "renamed".
7. Send the UNCHANGED request R a third time — a byte-for-byte retry of the acceptance that already succeeded. Observed: HTTP 400 `InterconnectClientException` with "...has already been accepted as connection <id>, whose attach point cannot be changed", even though R's attachPoint is identical to the one stored.
8. Repeat the tag variant: accept with tags={"owner":"me"}, then UntagResource(arn=<connection arn>, tagKeys=["owner"]), then resend the same acceptance — also HTTP 400.
9. Contrast the sibling: CreateConnection(clientToken="t-2", ...), then UpdateConnection(description="renamed"), then resend the identical CreateConnection — it still replays (src/state.rs:316-323 compares the request, not the record).
