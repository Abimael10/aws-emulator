Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 5f29791fc25a509777c1b865e6b33ff5033fb67b

# The "this connector is already going away" predicate is spelled out in DeleteEnvironmentConnector instead of the StoredConnector::is_live the crate single-homes it in

_Filed: 2026-11-21T09:47:25Z_
_Operation: DeleteEnvironmentConnector | Class: maintainability_

> _Note: this issue was identified against the code base at commit `5f29791fc25a509777c1b865e6b33ff5033fb67b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StoredConnector::is_live()` IS the predicate "the teardown has not started": its body is `state != CONNECTOR_DELETING && state != CONNECTOR_DELETED`, and its doc calls it "The one predicate behind [`live_connector`] and [`live_connector_mut`], so a reader and a writer resolve exactly the same records". `DeleteEnvironmentConnector` re-derives the same two-state test inline as `already_going_away`, which is exactly `!connector.is_live()`. The two copies can drift: a `ConnectorState` added to the "going away" side of the line (the model already declares `CREATE_FAILED` and `UPDATE_FAILED` beside the five this crate moves a connector through) would have to be added in both places, and a change made in only one would let the delete re-stamp and re-schedule a record `live_connector` already resolves to nothing — or refuse the precondition a second time for a teardown already accepted.

The fix is substituting the existing helper (`let already_going_away = !connector.is_live();`), which evaluates to the same boolean for every stored state, so every response and every existing test stays byte-identical.

## Evidence

The single-homed predicate — `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state.rs:888-903`:
```
    /// Whether this record is still a connector a request can USE: one whose teardown
    /// has not started.
    ///
    /// A `DELETING` connector's connection to the appliance is being torn down and a
    /// `DELETED` one is gone, so neither is a connector further work can be tracked
    /// through ...
    ///
    /// The one predicate behind [`live_connector`] and [`live_connector_mut`], so a
    /// reader and a writer resolve exactly the same records.
    pub fn is_live(&self) -> bool {
        self.state != CONNECTOR_DELETING && self.state != CONNECTOR_DELETED
    }
```

The second copy — `src/ops/delete_environment_connector.rs:262-267`:
```
        // `@idempotent`: a connector that is already DELETING or DELETED is reported as
        // it stands, so a repeated call neither restarts the teardown nor moves
        // `modifiedAt` — and its precondition is not re-judged, having been judged when
        // the delete that started the teardown was accepted.
        let already_going_away = connector.state == crate::state::CONNECTOR_DELETING
            || connector.state == crate::state::CONNECTOR_DELETED;
```

That `already_going_away` gates the three things the delete does (`src/ops/delete_environment_connector.rs:269` the blocking-entitlement precondition, `:292-302` the state change, re-stamp and pending transition), so the two copies govern one rule.

Test coverage that must stay unchanged: `tests/all/connector_deletion.rs:379-410` (the repeated delete of a `DELETING` and of a settled `DELETED` record reports it as it stands and restarts nothing), `tests/all/connector_create_while_deleting.rs:415-525` (which states on which side of the line each `ConnectorState` falls), `tests/all/environment_addressing_rule.rs:327-334` (the `DeleteEnvironmentConnector` row), and `tests/all/environment_child_change_stamps.rs:375-384` ("a connector removal repeated" moves no stamp).

## Steps to Reproduce

1. Read `src/state.rs:901-903` — `StoredConnector::is_live()` is `self.state != CONNECTOR_DELETING && self.state != CONNECTOR_DELETED`, documented as the one predicate behind `live_connector` / `live_connector_mut`.
2. Read `src/ops/delete_environment_connector.rs:266-267` — the same disjunction over the same two constants is written again as `already_going_away`.
3. Note that `already_going_away` is the negation of `is_live()` for every stored state, so the handler holds a second copy of a rule the state module owns; adding a state to either side of the line requires editing both.
