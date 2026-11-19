Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 2bfe2a8a9c1ad5a493cf856532ec70b14ee46345

# The four environment-scoped list operations resolve an environment that has reached DELETED, although the state module names only GetEnvironment and ListEnvironments as deliberate exceptions

_Filed: 2026-11-19T11:29:59Z_
_Operation: ListEnvironmentVlans | Class: error-response_

> _Note: this issue was identified against the code base at commit `2bfe2a8a9c1ad5a493cf856532ec70b14ee46345`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate single-homes the "which environment does a request address" rule in `crate::state::live_environment_in_region(_mut)` / `live_environment_by_arn(_mut)`, whose own documentation enumerates the operations allowed to bypass the liveness half of that rule: exactly TWO readers, `GetEnvironment` and `ListEnvironments`.

In fact SIX readers bypass it. All four environment-scoped list handlers resolve through the non-live `environment_in_region`, so an environment that has reached `DELETED` is still addressable by them. The observable result is that one and the same deleted environment is simultaneously gone and present, depending on which read you issue:

* `GetDepotUrl`, `AssociateEipToVlan`, `DisassociateEipFromVlan`, `CreateEntitlement`, `DeleteEntitlement`, `CreateEnvironmentConnector`, `UpdateEnvironmentConnector`, `DeleteEnvironmentConnector`, `CreateEnvironmentHost`, `DeleteEnvironmentHost` and the three tagging operations all answer their not-found refusal for it (they call the `live_*` form);
* `ListEnvironmentVlans`, `ListEnvironmentHosts` and `ListEnvironmentConnectors` answer HTTP 200 and report its retained child records;
* `ListVmEntitlements` answers `ResourceNotFoundException` for the same environment anyway, but only incidentally — because the connector its `connectorId` names was cascade-deleted with the environment — so even inside the four-list family the answer is not consistent.

For `ListEnvironmentVlans` the 200 is self-contradicting as well: `DeleteEnvironment` RELEASES the ten VLAN subnets from the account's EC2 store on the call that accepts the deletion, so the page this handler serves afterwards reports ten `subnetId`s that the EC2 store no longer holds — exactly the kind of answer the addressing rule exists to prevent.

Either the state module's documented exception list is wrong about the code, or the four list handlers skip a check their thirteen siblings apply; the two cannot both be right.

Disclosure of the counter-evidence a reviewer should weigh: `state.rs:631-634` argues, about the *initializing* refusal only, that "a list against one of those is answered", and `tests/all/list_while_initializing.rs:59-63` asserts a `DELETED` environment answers a list while stating that whether it should instead be reported absent "is a separate question this suite deliberately does not decide — the handlers' lookup is untouched here". So the 200 is asserted by a test, but that test explicitly declines to decide the lookup question this issue is about.

## Evidence

The rule and its documented exception list — `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state.rs:3286-3298`:
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

The four handlers that are not on that list and resolve through the non-live form anyway:
* `src/ops/list_environment_vlans.rs:210-216`: `let environment = state\n    .environment_in_region(&region, &input.environment_id)` with the comment "Regional resource: the VLANs of an environment are only listable at the endpoint of the Region that environment was created in." — the Region half of the rule only, no liveness half.
* `src/ops/list_environment_hosts.rs:209-215`: `.environment_in_region(&region, &input.environment_id)`
* `src/ops/list_environment_connectors.rs:215-221`: `.environment_in_region(&region, &input.environment_id)`
* `src/ops/list_vm_entitlements.rs:226-232`: `.environment_in_region(&region, &input.environment_id)`

The sibling side of the same rule — `src/ops/list_tags_for_resource.rs:168-181`:
```
// The ARN is resolved through the rule every handler that acts on an environment
// shares (`live_environment_by_arn`): against the Region the request was signed
// for, and only to a record that has not reached DELETED — a resource of another
// Region's endpoint and one that is gone are both gone for addressing purposes.
let environment = state
    .live_environment_by_arn(&region, &input.resource_arn)
```
and `src/ops/get_depot_url.rs:204-210`, `src/ops/create_entitlement.rs:222-228`, `src/ops/delete_environment_host.rs:235-241`, which all use the `live_*` form.

The suite that pins the rule — `tests/all/environment_addressing_rule.rs:17-28`:
```
//! Every operation of this service that acts on an environment applies that one rule
//! through the state module's `live_environment_in_region(_mut)` /
//! `live_environment_by_arn(_mut)`, so the table below spans ALL of them ...
//! The two readers that deliberately report a finished environment, `GetEnvironment` and
//! `ListEnvironments`, are NOT part of this rule
```
Its table (`operation:` rows at lines 269-366) carries thirteen operations and none of the four lists.

Why the 200 is incoherent for VLANs — `src/ops/delete_environment.rs:314-322`:
```
// "Environment deletion also deletes the associated Amazon EVS VLAN subnets":
// the subnets this service provisioned in the account's own Amazon EC2 store are
// removed from it here ...
crate::state::ec2_resources::release_vlan_subnets(&*self.engine, &vlan_subnet_ids);
```

Counter-evidence, disclosed: `tests/all/list_while_initializing.rs:59-63` (`("DELETED", false)` row) — "whether a `DELETED` environment should instead be reported absent ... is a separate question this suite deliberately does not decide — the handlers' lookup is untouched here".

## Steps to Reproduce

1. CreateEnvironment with no `hosts` member (so the environment can be deleted) and let the create transition settle, so the environment is CREATED.
2. Call DeleteEnvironment for it and let the deletion transition settle, so `GetEnvironment` reports `environmentState` DELETED.
3. Call GetDepotUrl / TagResource / ListTagsForResource for the same environment: each answers its not-found refusal, because they resolve through `live_environment_in_region` / `live_environment_by_arn`.
4. Call ListEnvironmentVlans (and ListEnvironmentHosts, ListEnvironmentConnectors) for the same environment: each answers HTTP 200 and reports the retained child records — ListEnvironmentVlans reports ten `subnetId`s that `release_vlan_subnets` already removed from the EC2 store.
5. Compare with ListVmEntitlements on that same environment, which answers ResourceNotFoundException (its connector was cascade-deleted) — the four-list family does not even agree with itself.
