Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 5f29791fc25a509777c1b865e6b33ff5033fb67b

# DeleteEnvironment resolves an environment that has reached DELETED, although the state module names only GetEnvironment and ListEnvironments as deliberate exceptions to the addressing rule

_Filed: 2026-11-21T09:47:25Z_
_Operation: DeleteEnvironment | Class: error-response_

> _Note: this issue was identified against the code base at commit `5f29791fc25a509777c1b865e6b33ff5033fb67b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate single-homes "which environment does a request address" in `live_environment_in_region(_mut)` / `live_environment_by_arn(_mut)`, and that function's own documentation enumerates the operations allowed to bypass the liveness half: exactly TWO readers, `GetEnvironment` and `ListEnvironments`. `DeleteEnvironment` is neither of those — it is a mutating operation — yet it resolves through the non-live `environment_in_region_mut`, so an environment whose state has already settled on `DELETED` is still addressable by it and answered HTTP 200 with the `DELETED` record, where its thirteen siblings answer their not-found refusal for the same record.

The handler's own comment justifies this for the `DELETING` case (`@idempotent`: a repeat must not restart the transition), and the suite pins exactly that case — a repeat issued while the record is still `DELETING`. Nothing pins the `DELETED` case, and for it the two statements in the crate cannot both be true: either the state module's documented exception list is wrong about the code, or `DeleteEnvironment` skips the liveness half of the rule its siblings apply. The observable result is that one and the same deleted environment is simultaneously gone (GetDepotUrl, the three tagging operations, both Elastic IP operations, both entitlement operations, all three connector operations, both host operations) and present (a delete that reports it as successfully addressed).

## Evidence

The rule and its documented exception list — `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state.rs:3286-3299`:
```
    /// The environment `environment_id` ADDRESSES: the record this service holds IN
    /// `region` that is still one a request can act on.
    ...
    /// The two readers that deliberately report a finished environment —
    /// `GetEnvironment`, which describes the record whatever state it has reached, and
    /// `ListEnvironments`, which filters by `EnvironmentState` on request — resolve
    /// through [`Self::environment_in_region`] instead.
    pub fn live_environment_in_region(
```
and the liveness half it applies — `src/state.rs:1651-1663`:
```
    /// A `DELETED` environment is gone, so a request that names it resolves to nothing
    /// ("The resource might not be specified correctly, or it may have a `state` of
    /// `DELETED`", `ResourceNotFoundException` doc). ...
    pub fn is_live(&self) -> bool {
        self.state != STATE_DELETED
    }
```

The handler that bypasses it — `src/ops/delete_environment.rs:194-208`:
```
        // Regional resource: an environment can only be deleted at the endpoint of the
        // Region it was created in.
        let environment = state
            .environment_in_region_mut(&region, &input.environment_id)
            .ok_or_else(|| {
                DeleteEnvironmentErrorModeled::ResourceNotFoundException(
                    crate::state::environment_not_found(&input.environment_id),
                )
            })?;

        // `@idempotent`: an environment that is already DELETING or DELETED is
        // reported as it stands, so a repeated call neither restarts the transition
        // nor moves `modifiedAt`.
```
— the Region half of the rule only, no liveness half.

The sibling side of the same rule — `src/ops/delete_environment_host.rs:230-241` (`.live_environment_in_region_mut(&region, &input.environment_id)` with the comment "the rule every handler that acts on an environment shares … an environment that has reached DELETED is gone"), and identically `src/ops/get_depot_url.rs:204-210`, `src/ops/create_entitlement.rs:222-228`, `src/ops/delete_entitlement.rs:230-236`, `src/ops/create_environment_connector.rs:281-287`, `src/ops/update_environment_connector.rs:252-258`, `src/ops/delete_environment_connector.rs:236-242`, `src/ops/associate_eip_to_vlan.rs:241-247`, `src/ops/disassociate_eip_from_vlan.rs:219-225`, `src/ops/create_environment_host.rs:274-280`, `src/ops/tag_resource.rs:218-219`, `src/ops/untag_resource.rs:192-193`, `src/ops/list_tags_for_resource.rs:172-173`.

The suite that claims to span the whole rule — `tests/all/environment_addressing_rule.rs:17-28`:
```
//! Every operation of this service that acts on an environment applies that one rule
//! through the state module's `live_environment_in_region(_mut)` /
//! `live_environment_by_arn(_mut)`, so the table below spans ALL of them — the
//! environment-id-addressed ones and the three ARN-addressed tagging ones — rather than
//! one. ...
//! The two readers that deliberately report a finished environment, `GetEnvironment` and
//! `ListEnvironments`, are NOT part of this rule and are pinned by
//! `tests/all/environment_lifecycle.rs`.
```
Its `rows(&fixture)` table (`tests/all/environment_addressing_rule.rs:267-370`) carries thirteen operations and `DeleteEnvironment` is not among them: that file uses DeleteEnvironment only to drive the fixture into `DELETING`/`DELETED` (lines 469-480, 547-558).

Counter-evidence, disclosed: `tests/all/environment_lifecycle.rs:880-893` pins the idempotent repeat, but issues it BEFORE the transition settles — the record is `DELETING` there (asserted at lines 866-871), not `DELETED`:
```
    // @idempotent: deleting it again reports the same record and restarts nothing.
```
Also disclosed: open issue `20261119T112959Z-evs-listenvironmentvlans-error-response` reports the same bypass for the four environment-scoped LIST operations; it enumerates the handlers that use the `live_*` form and does not mention `DeleteEnvironment`, so this handler is a seventh bypass named by neither the state module's list nor that issue.

## Steps to Reproduce

1. CreateEnvironment with no `hosts` member (so the environment can be deleted) and wait for the create transition, so GetEnvironment reports `CREATED`.
2. Call DeleteEnvironment for it and wait for the deletion transition, so GetEnvironment reports `environmentState` `DELETED`.
3. Call GetDepotUrl / TagResource / AssociateEipToVlan for the same environment: each answers its not-found refusal, because each resolves through `live_environment_in_region(_mut)` / `live_environment_by_arn_mut`.
4. Call DeleteEnvironment again for the same environment: it answers HTTP 200 with `environment.environmentState` `DELETED`, having resolved a record the crate's own addressing rule says is gone for every request that acts on an environment.
