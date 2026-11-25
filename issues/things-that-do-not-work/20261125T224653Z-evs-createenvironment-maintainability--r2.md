Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 3145e08a0e7597c7d02d6ec9279f15fb50aef399

# The "the environment's Availability Zone is the service access subnet's zone, else the Region's first zone" rule is written out twice in ec2_resources.rs

_Filed: 2026-11-25T22:46:53Z_
_Operation: CreateEnvironment | Class: maintainability_

> _Note: this issue was identified against the code base at commit `3145e08a0e7597c7d02d6ec9279f15fb50aef399`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ec2_resources.rs` resolves the one zone an environment's resources are placed in twice, in two functions of the same file: once inline in `launch_host_instance` (for the host's EC2 instance) and once in `environment_zone` (for the VLAN subnets). Both read the service access subnet's `availability_zone` and fall back to the Region's first zone, and both document the same rule.

The copies have already drifted on one branch: `environment_zone` rejects a zone name that is empty (`.filter(|(zone_name, _)| !zone_name.is_empty())`, so provisioning answers `ZoneUnknown` rather than storing a subnet of no zone), while the inline copy in `launch_host_instance` has no such guard and would store an instance whose `placement_az` is the empty string — the very thing its own comment says it refuses to do ("a Region name is not a zone name, and storing one would report a zone no `DescribeAvailabilityZones` lists").

Stating the rule once — `launch_host_instance` taking the zone name from `environment_zone`, which is the function that already owns it — leaves the answer identical for every input either path can reach today (an EVS-provisioned subnet always carries the non-empty zone `environment_zone` filtered, and EC2's own `CreateSubnet` stores a zone name), so no response and no existing test changes.

## Evidence

Copy 1 — src/state/ec2_resources.rs:546-561, inside `launch_host_instance`:
```
    // The Availability Zone the environment is deployed in: the zone of the service
    // access subnet the caller gave it ... An account whose store no longer holds that subnet falls back to
    // the Region's first zone, the way EC2's own placement does — and a Region with no
    // zone at all leaves nothing to place an instance in, so nothing is launched: a
    // Region name is not a zone name, and storing one would report a zone no
    // `DescribeAvailabilityZones` lists.
    let availability_zone = store
        .subnets
        .get(launch.service_access_subnet_id)
        .and_then(|subnet| subnet.availability_zone.clone())
        .or_else(|| {
            ec2::availability_zones_in_region(launch.region)
                .first()
                .map(|zone| zone.zone_name.clone())
        })?;
```
Copy 2 — src/state/ec2_resources.rs:1055-1088, `environment_zone`, which states the same rule and adds the empty-name guard:
```
/// "Amazon EVS requires that all VPC subnets exist in a single Availability Zone in a
/// Region where the service is available" (`vpcId` doc), and the service access subnet
/// is one of those subnets, so ITS zone is the environment's zone ... A service access subnet the store holds without a zone leaves
/// the Region's first zone, which is the zone `CreateSubnet` itself picks for a request
/// that names none; `None` when neither names one, which is no zone to put a subnet in.
fn environment_zone(...) -> Option<(String, String)> {
    let held = store.subnets.get(service_access_subnet_id).and_then(|subnet| { ... });
    held.or_else(|| {
        ec2::availability_zones_in_region(region).into_iter().next().map(|zone| (zone.zone_name, zone.zone_id))
    })
    .filter(|(zone_name, _)| !zone_name.is_empty())
}
```
Both are reached from one handler: src/ops/create_environment.rs:739 calls `provision_vlan_subnets` (which calls `environment_zone`, ec2_resources.rs:1142) and src/ops/create_environment.rs:993 (`add_requested_hosts`) calls `launch_host_instance`; src/ops/create_environment_host.rs:391 calls the latter too.

Tests covering both sides: tests/all/vlan_subnet_propagation.rs:19 and :475 ("`Vlan$availabilityZone` is the zone of that subnet, which is the zone of the …" and the assertion on `vlan["availabilityZone"]`), and tests/all/created_environment_hosts.rs:222 / tests/all/host_placement_references.rs:189, which seed the service access subnet with `("AvailabilityZone", ZONE)` for the host launch path.

## Steps to Reproduce

1. Read src/state/ec2_resources.rs:546-561 and src/state/ec2_resources.rs:1055-1088: both resolve the environment's Availability Zone from the service access subnet with a fallback to the Region's first zone.
2. Note that only the second applies `.filter(|(zone_name, _)| !zone_name.is_empty())`, so the two copies already disagree about a subnet whose stored zone name is empty.
3. Run the EVS suite (tests/all/vlan_subnet_propagation.rs, tests/all/created_environment_hosts.rs) before and after stating the rule once: every response is unchanged.
