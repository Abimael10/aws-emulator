Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 05f543300edf7671f260beb5801de5b1b228112d

# CreateEnvironmentConnector mints its connectorId before resolving the environment, so a refused request consumes the store's id seed — the one thing CreateEnvironment goes out of its way not to do

_Filed: 2026-11-19T10:12:35Z_
_Operation: CreateEnvironmentConnector | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `05f543300edf7671f260beb5801de5b1b228112d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateEnvironmentConnector` calls `state.mint_connector_id()` BEFORE it resolves the environment the request addresses and before the credential-secret precondition runs. `mint_connector_id` mutates the store — `next_seed()` increments `next_id_seed` and seeds `id_salt` on first use — and that counter is shared by every id this service mints (environment ids, connector ids, fallback subnet ids, EIP association ids). So a `CreateEnvironmentConnector` that is refused `ResourceNotFoundException` (an unknown environment id, an environment of another Region, a DELETED one), refused `ValidationException` for a type already taken, or refused for a credential secret the account does not hold, still leaves the store changed: the next id this service mints is not the one it would have minted.

That is the exact rule `CreateEnvironment` states for itself and builds an extra read-only helper to preserve — `subnet_room_shortfall` exists only so the create can refuse on Amazon EC2's subnet ceiling before any id is consumed. Moving the two lines below the environment lookup and the precondition block costs nothing and changes no response: the connector id is opaque and is only reported for a create that succeeds.

## Evidence

The rule this crate states for its own minting — src/ops/create_environment.rs:694-701:
```
        // The ten VLAN subnets have to fit in the account's EC2 store, and that is
        // asked BEFORE anything of this service's own is minted: a request that cannot
        // be served must not consume an environment id.
        if let Some((held, limit)) =
            crate::state::ec2_resources::subnet_room_shortfall(&*self.engine, vlan_plan.len())
        {
            return Err(subnet_room_refusal(held, limit, vlan_plan.len()));
        }
```
and src/ops/create_environment.rs:703-708: "Bound the store before anything is minted ... This asks only whether there is room — a pure read, so a request refused further down leaves every stored record where it is".

The helper that exists only for that rule — src/state/ec2_resources.rs:1046-1048:
```
/// A READ, so a create can refuse on the EC2 subnet ceiling BEFORE it mints anything of
/// its own: [`provision_vlan_subnets`] enforces the same ceiling and writes nothing when
/// it is reached, but by then the create has already minted an environment id.
```
The outlier — src/ops/create_environment_connector.rs:273-287:
```
        let created_at_ms = crate::state::now_ms();
        let connector_id = state.mint_connector_id();

        // The environment this create addresses, resolved through the rule every handler
        // that acts on an environment shares (`live_environment_in_region_mut`): ...
        let environment = state
            .live_environment_in_region_mut(&region, &input.environment_id)
            .ok_or_else(|| {
                CreateEnvironmentConnectorErrorModeled::ResourceNotFoundException(
                    crate::state::environment_not_found(&input.environment_id),
                )
            })?;
```
The mint is a store mutation — src/state.rs:3042-3050:
```
    fn next_seed(&mut self) -> u64 {
        if self.id_salt == 0 {
            ...
            self.id_salt = (now_ms() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        }
        self.next_id_seed += 1;
        self.next_id_seed
    }
```
and src/state.rs:3064-3068 (`mint_connector_id`) draws from it, as `mint_environment_id` (3055-3059), `mint_subnet_id` (3078-3082) and `mint_eip_association_id` (3086-3090) all do.

Every other create of this crate mints after its refusals: `CreateEnvironment` at create_environment.rs:723 (after all preconditions, the EC2 subnet-room read and the store-room read), and `AssociateEipToVlan` calls `state.mint_eip_association_id()` only in its last branch (associate_eip_to_vlan.rs:390-394), after every refusal. `CreateEntitlement` mints no id at all.

Test coverage: tests/all/connectors.rs and tests/all/connector_create_while_deleting.rs exercise the refusal paths (unknown environment, type already taken) and assert only the response and that no connector record is stored; no test reads the store's id seed, so moving the mint leaves every assertion standing.

## Steps to Reproduce

1. Mount the EVS service on an in-memory engine with a fresh store.
2. Call CreateEnvironmentConnector with `environmentId: "env-0000000000"` (an id no environment exists for) — observe HTTP 404 ResourceNotFoundException.
3. Inspect the store (or call CreateEnvironment and look at the minted `environmentId`): the id seed has advanced, so the environment created next carries a different id than it would have without the refused call.
4. Expected (the rule create_environment.rs:694-696 states): a request that cannot be served consumes no id — mint the connectorId after the environment lookup and the credential-secret precondition.
