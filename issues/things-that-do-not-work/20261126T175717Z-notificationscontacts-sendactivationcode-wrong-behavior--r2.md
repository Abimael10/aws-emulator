Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: e7528d3e52a8616792d558e251041b8f9fac92e1

# A refused SendActivationCode still consumes a recorded instant, advancing the store's monotonic clock

_Filed: 2026-11-26T17:57:17Z_
_Operation: SendActivationCode | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `e7528d3e52a8616792d558e251041b8f9fac92e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`SendActivationCode` draws the store's RECORDED instant (`next_instant_millis`, which WRITES `last_instant_millis`) before it has resolved the contact, so the 404 path (an ARN the store does not hold) and the 409 path (an already-active contact) both mutate the store's instant counter although they persist nothing.

Every sibling writer in this crate is careful to draw a recorded instant ONLY on the path that actually records a change: `CreateEmailContact` draws its id and instant after the quota refusal, and `ActivateEmailContact` draws one only when the submitted code matches (`code_matches.then(|| store.next_instant_millis())`), explicitly so that "`updateTime` moves only on the call that actually changes the status". `crate::state` draws the same distinction between the two accessors: `next_instant_millis` is for the instant an operation RECORDS, while `current_instant_millis` exists for reading the clock "without recording a change of its own".

Because `next_instant_millis` is `now.max(last + 1)`, every refused send advances the stored clock by at least a millisecond beyond the wall clock. A caller repeating a refused send therefore pushes `last_instant_millis` into the FUTURE, and the `creationTime`/`updateTime` of every contact created or activated afterwards (and the issue instant every later token's documented 12-hour lifetime is measured from) is stamped ahead of the real clock by the number of refusals served. The refusal paths are state-changing where the crate's own rule says only a recording path may be.

## Evidence

The handler draws code AND recorded instant before it knows the contact exists — `src/ops/send_activation_code.rs:165-171`:
```
        let code = store.fresh_activation_code();
        let issued_at = store.next_instant_millis();
        let contact = store.contact_mut(&input.arn).ok_or_else(|| {
            SendActivationCodeErrorModeled::ResourceNotFoundException(
                crate::state::contact_not_found(&input.arn),
            )
        })?;
```
(the 409 for an active contact is returned further down, at `src/ops/send_activation_code.rs:175-186`, also after the draw).

The accessor it calls MUTATES the store — `src/state.rs:587-592`:
```
    /// Now, in milliseconds since the Unix epoch, and strictly later than every
    /// instant this store has already handed out.
    pub fn next_instant_millis(&mut self) -> i64 {
        self.last_instant_millis = now_epoch_millis().max(self.last_instant_millis + 1);
        self.last_instant_millis
    }
```
and the crate states the other accessor is the one for a path that records nothing — `src/state.rs:594-603`:
```
    /// Now, in milliseconds since the Unix epoch, on the same timeline the instants
    /// this store hands out lie on — never earlier than one of them — and RECORDING
    /// nothing.
```

Sibling that draws a recorded instant only when it records — `src/ops/activate_email_contact.rs:232-235`:
```
        // Activating an already-active contact is a no-op (handled above), so
        // `updateTime` moves only on the call that actually changes the status; a
        // matching code reaching here therefore always belongs to an inactive contact.
        let activated_at = code_matches.then(|| store.next_instant_millis());
```
Sibling that refuses BEFORE drawing — `src/ops/create_email_contact.rs:192-203`:
```
        if held >= crate::state::MAX_EMAIL_CONTACTS {
            return Err(CreateEmailContactErrorModeled::ServiceQuotaExceededException(
```
... followed by `let id = store.fresh_contact_id(); ... let created_at = store.next_instant_millis();`.

The crate's own rule for recorded instants — `tests/all/recorded_instants_are_monotonic.rs:5-9`:
```
//! RULE: an operation that records a change stamps the instant it persists from the
//! store's own instant accessor, so every recorded instant is strictly later than every
//! instant the store has already handed out
```
and that suite deliberately never drives the refusal paths, so this is unpinned — `tests/all/recorded_instants_are_monotonic.rs:239-241`:
```
            // Each contact, with whether it has been activated: a send is refused for an
            // active contact and an activation of one records nothing, so neither is driven
            // here — their own suites own those refusals.
```

## Steps to Reproduce

1. Start the emulator with the notificationscontacts service mounted.
2. Call `POST /2022-10-31/emailcontacts/arn:aws:notifications-contacts::000000000000:emailcontact/aaaaaaaaaaaaaaaaaaaaaaaaaaa/activate/send` (a well-formed ARN no contact uses) a few thousand times in a tight loop; every call answers the modelled 404 and persists nothing.
3. Now call `POST /2022-09-19/emailcontacts` to create a contact and `GET /emailcontacts/{arn}`.
4. Observe `creationTime`/`updateTime` stamped ahead of the real clock by roughly one millisecond per refused send: each refusal advanced `last_instant_millis` (src/state.rs:590) although the refusal recorded nothing, where `ActivateEmailContact` (src/ops/activate_email_contact.rs:235) and `CreateEmailContact` (src/ops/create_email_contact.rs:199-203) draw a recorded instant only on the path that records a change.
