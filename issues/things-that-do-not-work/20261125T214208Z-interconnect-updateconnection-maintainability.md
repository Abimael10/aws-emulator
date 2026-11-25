Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 2c036246c52324f8f473f1b5e9b5314460be2843

# UpdateConnection hand-rolls the region-scoped mutable connection lookup the store already exposes as connection_in_mut

_Filed: 2026-11-25T21:42:08Z_
_Operation: UpdateConnection | Class: maintainability_

> _Note: this issue was identified against the code base at commit `2c036246c52324f8f473f1b5e9b5314460be2843`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states exactly one rule for resolving a connection a MUTATING operation is about: look the identifier up in `connections` and keep it only when the record belongs to the Region the request was signed for. That rule lives once in the state module, as `ComAmazonawsInterconnectInterconnectState::connection_in_mut`, and the two sibling writers that need it call it (`DeleteConnection`, `AcceptConnectionProposal`).

`UpdateConnection` is the one writer that does not: it spells the same two steps out inline — `connections.get_mut(identifier)` followed by `.filter(|stored| visible_in(stored, region))` — so the one resolution rule now exists as two copies in this crate's own code and they can drift apart (a later change to `connection_in_mut`, e.g. the `exists_as_resource` guard its ARN-addressed sibling `connection_in_for_arn_mut` already adds, would silently not reach the update path).

The inline copy is byte-for-byte the helper's body, so calling `store.connection_in_mut(&region, &input.identifier)` instead produces the identical `Option<&mut StoredConnection>`: every response and every existing test stays exactly as it is. The comment above the inline copy already admits it is restating the shared predicate ("spelled with the store's own predicate so this field borrow keeps one rule"), and nothing after the lookup needs the store again — the only later call, `crate::state::environment_in_region(&region, &environment_id)`, is a free function that touches no store — so the borrow reason the comment gives does not require the duplication.

## Evidence

The helper, with its doc naming the operations it exists for — services/service-com-amazonaws-interconnect-interconnect/src/state.rs:514-524:

    /// [`Self::connection_in`] for the operations that MUTATE the record they
    /// resolve, scoping the store by the same predicate.
    pub fn connection_in_mut(
        &mut self,
        region: &str,
        identifier: &str,
    ) -> Option<&mut StoredConnection> {
        self.connections
            .get_mut(identifier)
            .filter(|stored| visible_in(stored, region))
    }

The second copy of that same body, in the handler — services/service-com-amazonaws-interconnect-interconnect/src/ops/update_connection.rs:169-177:

        // The same Region scope every connection lookup applies, spelled with the
        // store's own predicate so this field borrow keeps one rule: a
        // connection created through another Region's endpoint is not one this
        // endpoint can update.
        let Some(stored) = store
            .connections
            .get_mut(&input.identifier)
            .filter(|stored| crate::state::visible_in(stored, &region))
        else {

The siblings that call the helper rather than restating it:
- services/service-com-amazonaws-interconnect-interconnect/src/ops/delete_connection.rs:155 — `let Some(stored) = store.connection_in_mut(&region, &input.identifier) else {`
- services/service-com-amazonaws-interconnect-interconnect/src/ops/accept_connection_proposal.rs:515 — `let Some(stored) = store.connection_in_mut(&region, &connection_id) else {`

The one predicate both copies end in — services/service-com-amazonaws-interconnect-interconnect/src/state.rs:287-296: "ONE predicate, so every resolution path below scopes the store the same way." / `pub fn visible_in(stored: &StoredConnection, region: &str) -> bool { stored.region == region }`

Nothing after the lookup needs the store, so the borrow the comment cites is not what forces the copy — update_connection.rs:216-230 uses only `stored.connection.environment_id` and the free function `crate::state::environment_in_region(&region, &environment_id)`.

Test coverage that keeps the behaviour pinned across the change:
- services/service-com-amazonaws-interconnect-interconnect/tests/all/region_scoped_connections.rs:296-306 — drives `UpdateConnection` through every other Region's endpoint and asserts the refusal is `connection_absent_message(&id)`, the same answer an identifier that was never allocated gets.
- services/service-com-amazonaws-interconnect-interconnect/tests/all/update_connection_idempotent_retry.rs and tests/all/update_settles_back_into_the_state_it_came_from.rs — the in-Region success paths of the same lookup.

## Steps to Reproduce

1. Open services/service-com-amazonaws-interconnect-interconnect/src/state.rs and read `ComAmazonawsInterconnectInterconnectState::connection_in_mut` at lines 514-524: `connections.get_mut(identifier).filter(|stored| visible_in(stored, region))`, documented as the resolution "for the operations that MUTATE the record they resolve".
2. Open services/service-com-amazonaws-interconnect-interconnect/src/ops/update_connection.rs and read lines 173-177: the same `connections.get_mut(&input.identifier).filter(|stored| crate::state::visible_in(stored, &region))` written out again in the handler.
3. Compare with the two sibling writers that call the helper instead: delete_connection.rs:155 and accept_connection_proposal.rs:515.
4. Observe that the rule now has two copies in this crate, so a change made in `connection_in_mut` (as `connection_in_for_arn_mut` at state.rs:568-576 already layers `exists_as_resource` on top of it) would not reach `UpdateConnection`.
5. Replace the inline lookup with `store.connection_in_mut(&region, &input.identifier)` and re-run the crate's tests: region_scoped_connections.rs, update_connection_idempotent_retry.rs and update_settles_back_into_the_state_it_came_from.rs must pass unchanged, and no response byte changes.
