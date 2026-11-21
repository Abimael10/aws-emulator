Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: b61d91764c5b64113b88eda248364bbad90afe06

# Re-recording an idempotency outcome keeps the key's original FIFO position, breaking the documented "never drops a newer outcome" eviction rule

_Filed: 2026-11-21T14:55:03Z_
_Operation: UpdateConnectionRecordingPreferences | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `b61d91764c5b64113b88eda248364bbad90afe06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::ComAmazonawsSsmguiconnectSsmguiconnectState::record_outcome` is the single place both mutating operations persist an idempotency outcome (`put_preferences` for Update, `remove_preferences` for Delete). Its eviction queue `idempotency_order` is pushed ONLY when the key is new: `if self.idempotency_tokens.insert(key.clone(), outcome).is_none() { push_back(key) }`. So when an outcome is re-recorded under a `ClientToken` that is already in the map, the fresh outcome inherits the queue position of the token's FIRST insertion and is evicted ahead of outcomes recorded strictly later under newer tokens.

That contradicts the invariant the state module states for this very queue: eviction is claimed to be ordered such that "it never drops a newer outcome to keep an older one". Re-recording an existing key is a REACHABLE path, not a theoretical one: both handlers fall through to the write when `replayable_outcome` says the token's recorded outcome is no longer current (the resource changed out from under it), and the crate's own test `a_replay_whose_resource_changed_is_not_answered_from_the_first_outcome` drives exactly that for Update and for Delete. Those fall-through writes call `put_preferences` / `remove_preferences` with the SAME `ClientToken` as before, so `record_outcome` overwrites the map entry while leaving the stale queue position untouched.

The response-visible blast radius is small (for a Delete outcome dropped while still current, the call re-runs for real and answers the same 200 + token, differing only in the audit outcome token `preferences-already-absent` vs `idempotent-replay`), which is why this is filed LOW — but the code under the comment does not implement the rule the comment states, and the two will keep drifting as long as the queue tracks key insertion while the doc (and the test comment) talk about outcome recency.

## Evidence

The stated rule — `services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state.rs:54-55`:
```
/// had expired. Eviction is FIFO — strictly by insertion order, tracked in
/// `idempotency_order` — so it never drops a newer outcome to keep an older one.
```
and the queue's own field doc, `src/state.rs:89-92`:
```
/// Insertion order of `idempotency_tokens`' keys, which is what makes the
/// eviction above FIFO. Holds exactly the map's keys, so the two are inserted
/// and removed together.
idempotency_order: VecDeque<IdempotencyKey>,
```

The code that breaks it — `src/state.rs:216-221`:
```
/// Record `outcome` under `key`, evicting the oldest entries until the store is
/// back inside [`MAX_IDEMPOTENCY_OUTCOMES`].
fn record_outcome(&mut self, key: IdempotencyKey, outcome: StoredResponse) {
    if self.idempotency_tokens.insert(key.clone(), outcome).is_none() {
        self.idempotency_order.push_back(key);
    }
```
The `is_none()` guard means an overwrite (a NEW outcome under an OLD key) never refreshes the key's position, so the queue orders by key age, not by outcome age.

The re-record path is reachable from the handlers:
- `src/ops/update_connection_recording_preferences.rs:238` `if let Some(StoredResponse::Updated(record)) = store.replayable_outcome(&key)` — when this does NOT match, line 254 `store.put_preferences(record.clone());` re-records under the same caller token (`src/state.rs:169-172`: `let key = (IdempotentOperation::Update, record.client_token.clone()); ... self.record_outcome(key, StoredResponse::Updated(record));`).
- `src/ops/delete_connection_recording_preferences.rs:195-209` — same shape: a non-current `Deleted` outcome falls through to `store.remove_preferences(&client_token)`, which re-records `(IdempotentOperation::Delete, client_token)` (`src/state.rs:184-188`).

Test coverage of the two halves, neither of which exercises a REUSED token at the cap: `tests/all/connection_recording_preferences.rs:683` `a_replay_whose_resource_changed_is_not_answered_from_the_first_outcome` (proves the re-record path runs) and `tests/all/connection_recording_preferences.rs:769` `the_idempotency_store_stays_bounded`, which only ever sends distinct tokens (`"ClientToken": format!("token-{n}")`, line 785) and whose own comment restates the broken rule at lines 815-816:
```
// Eviction is FIFO, so the most recent token is still replayable and answers with
// the record it wrote rather than writing a second time.
```

## Steps to Reproduce

1. Read `services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state.rs:54-55`: eviction "never drops a newer outcome to keep an older one".
2. Read `record_outcome` at `src/state.rs:218-221`: `idempotency_order.push_back(key)` happens only when `insert(...).is_none()`, i.e. only for a key seen for the first time.
3. Drive the re-record path the crate already tests: enrolled account, `UpdateConnectionRecordingPreferences` with `ClientToken="reused"`, then `DeleteConnectionRecordingPreferences`, then `UpdateConnectionRecordingPreferences` with `ClientToken="reused"` again — the replay check at `src/ops/update_connection_recording_preferences.rs:238` fails (the record is no longer current) and line 254 calls `put_preferences`, re-recording `(Update, "reused")`.
4. Observe that `idempotency_order` still holds `"reused"` at the position of its FIRST insertion, so its newly recorded outcome is now the oldest queue entry even though it is the most recently recorded one.
5. Keep issuing mutating calls with fresh tokens until the store passes `MAX_IDEMPOTENCY_OUTCOMES`: the first eviction pops `"reused"` — the newest outcome — while up to 1023 outcomes recorded strictly earlier are retained, the opposite of the quoted invariant. With `DeleteConnectionRecordingPreferences` tokens (whose `Deleted` outcomes all stay current while nothing is configured) the dropped entry was still replayable, so the next replay of that token re-runs for real and is audited `preferences-already-absent` instead of `idempotent-replay`.
