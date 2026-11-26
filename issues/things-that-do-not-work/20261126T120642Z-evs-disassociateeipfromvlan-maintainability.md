Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: f7078ec9d24b4839050651ec5b14c25693a01eca

# The "take an Elastic IP association off a VLAN" rule is implemented twice — in DisassociateEipFromVlan and in DeleteEnvironment — instead of once on StoredVlan

_Filed: 2026-11-26T12:06:42Z_
_Operation: DisassociateEipFromVlan | Class: maintainability_

> _Note: this issue was identified against the code base at commit `f7078ec9d24b4839050651ec5b14c25693a01eca`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Ending an Elastic IP association of a VLAN is ONE rule with three inseparable steps in this crate: (1) drop the `EipAssociation` from `StoredVlan::eip_associations`, (2) remember the removal through `StoredVlan::note_eip_association_removed`, so an `@idempotent` repeat of `DisassociateEipFromVlan` resolves to `EipAssociationHandle::Removed` rather than to the missing resource, and (3) collect the `(allocationId, associationId)` pair so `ec2_resources::release_elastic_ips` gives the allocation back to the account's EC2 `Address` record.

The crate single-homes every other rule of this area on `StoredVlan` / `StoredEnvironment` — `resolve_eip_association`, `note_eip_association_removed` (with its own retention bound) and the three-step `addressed_public_hcx_vlan` gate that both Elastic IP operations share — but this one is written out twice, in two different shapes: once as a `retain` closure in `DisassociateEipFromVlan` (remove the ONE named association) and once as a `std::mem::take` loop in `DeleteEnvironment` (remove ALL of them). The two copies can drift: a change to the pairing of step (2) and step (3) — the invariant that no pair is handed to EC2 without being noted as removed, and none is noted without being released — has to be made in two handlers, and a copy that forgets one half either leaks a consumed EC2 allocation or makes a later disassociate report the missing resource for an association this service really did remove.

A fix single-homes the per-association step beside the two helpers it already calls (e.g. a `StoredVlan` method that removes the named association, or all of them, notes each removal and answers the `(allocation, association)` pairs to release), and both handlers call it. No response changes: both paths keep answering exactly what they answer today, and the EC2 release calls stay where they are, outside the per-association step.

## Evidence

Copy 1 — `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/ops/disassociate_eip_from_vlan.rs:265-289`:
```
        let before = vlan.eip_associations.len();
        let mut released: Vec<(String, String)> = Vec::new();
        vlan.eip_associations.retain(|association| {
            let named =
                association.association_id.as_deref() == Some(input.association_id.as_str());
            if named {
                if let (Some(allocation_id), Some(association_id)) = (
                    association.allocation_id.clone(),
                    association.association_id.clone(),
                ) {
                    released.push((allocation_id, association_id));
                }
            }
            !named
        });
        let removed = vlan.eip_associations.len() != before;
        if removed {
            // Remembered as a removal of THIS VLAN, so the idempotent repeat of this
            // call resolves to it instead of to no association at all.
            vlan.note_eip_association_removed(&input.association_id);
```

Copy 2 — `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/ops/delete_environment.rs:274-286`:
```
                for association in std::mem::take(&mut vlan.eip_associations) {
                    // The association this deletion ends is one this VLAN HELD, so a
                    // disassociate addressed to it afterwards resolves to a removal this
                    // service already carried out rather than to no association at all.
                    if let Some(association_id) = association.association_id.as_deref() {
                        vlan.note_eip_association_removed(association_id);
                    }
                    if let (Some(allocation_id), Some(association_id)) =
                        (association.allocation_id, association.association_id)
                    {
                        released_eips.push((allocation_id, association_id));
                    }
                }
```

That the two are ONE rule rather than two, stated by the crate's own suite — `tests/all/eip_ec2_propagation.rs:25-28`:
```
//! * every path that ends the association (`DisassociateEipFromVlan`, and
//!   `DeleteEnvironment`, which deletes the VLAN subnets the address was bound to)
//!   clears it on the EC2 record while the allocation STAYS allocated to the account, so
//!   it can be associated again.
```

That this crate single-homes rules of exactly this kind on the VLAN record — `src/state.rs:1491-1497` (`note_eip_association_removed`, "Record that this VLAN's association `association_id` has been removed, so a repeat of the call that removed it still resolves to the removal rather than to no resource at all") and `src/state.rs:1862-1867`:
```
    /// Where in [`Self::vlans`] the VLAN an Elastic IP request addresses by `vlan_name`
    /// sits, or the `ValidationException` that request is refused with.
    ///
    /// The one gate both Elastic IP operations apply, so a change to any of its three
    /// rules is made once and both operations agree:
```

Test coverage that pins the behaviour a fix must leave unchanged: `tests/all/eip_ec2_propagation.rs` (both ending paths against a router carrying EVS and EC2 on one engine), `tests/all/eip_release_on_environment_deletion.rs:3-30` ("the deletion releases every association they held — on the account's own Amazon EC2 `Address` records and on the VLAN records this service reports", including "the `@idempotent` repeat, which must not report an association that no longer exists"), `tests/all/eip_association_existence.rs:3-16` (what an `associationId` may name on `DisassociateEipFromVlan`), `tests/all/hcx_eip_associations.rs`, and `tests/all/record_stamp_monotonicity.rs:77` / `tests/all/environment_child_change_stamps.rs:82` (the stamps each removal writes).

## Steps to Reproduce

1. Open `src/ops/disassociate_eip_from_vlan.rs` and read lines 265-289: the handler removes the named `EipAssociation` from `vlan.eip_associations`, calls `vlan.note_eip_association_removed(..)`, and collects the `(allocationId, associationId)` pair it later hands to `crate::state::ec2_resources::release_elastic_ips`.
2. Open `src/ops/delete_environment.rs` and read lines 274-286: the same three steps again, in a `std::mem::take` loop over the same field, with the same two follow-ups (`note_eip_association_removed`, then `release_elastic_ips` at line 333).
3. Grep the crate for the helpers the two copies lean on (`grep -n 'note_eip_association_removed\\|resolve_eip_association' src`): both live once on `StoredVlan` in `src/state.rs`, while the removal-and-release step that must accompany them lives in two handlers.
4. Note that no `StoredVlan` method removes an association: `src/state.rs:1471-1515` exposes only `resolve_eip_association` and `note_eip_association_removed`, so each handler re-derives the removal and the EC2 release pairs itself.
