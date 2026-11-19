Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: a46c7b329d14c33881a40e10b99591f6626f6ae1

# A host's placementGroupId / dedicatedHostId never reach the EC2 instance the host IS, so EC2 reports the Dedicated Host as unoccupied

_Filed: 2026-11-19T09:36:27Z_
_Operation: CreateEnvironmentHost | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `a46c7b329d14c33881a40e10b99591f6626f6ae1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateEnvironmentHost` (and `CreateEnvironment`, through `add_requested_hosts`) resolves `host.placementGroupId` and `host.dedicatedHostId` against the account's own Amazon EC2 store and then reports both members back on the `Host`, but the instance it launches into that same store records NEITHER placement: `HostLaunch` has no field for them, and the instance is written with `detail: ec2::InstanceDetail::default()`, whose `placement_group_name` and `host_id` are `None`.

Consequences, all inside the one store this crate declares authoritative for the instance:

* EC2 `DescribeInstances` on the host's own `ec2InstanceId` reports `Placement.HostId` absent and `Placement.GroupName` empty, while the EVS `Host` reports `dedicatedHostId` / `placementGroupId` — the two stores disagree about where the same ESX host runs.
* EC2 recomputes a Dedicated Host's `Instances` from `instance.detail.host_id`, so `DescribeHosts` reports a Dedicated Host an EVS host was placed on as running nothing, and `dedicated_host_is_occupied` answers false for it. EC2's `ReleaseHosts` reads exactly that list, so it will release a Dedicated Host that is carrying a running ESX host instance.

This is NOT the already-fixed "CreateEnvironmentHost launches no EC2 bare metal instance" gap (the instance is launched) and not the open ENI gap (the two `eni-` ids): it is the placement of that instance.

## Evidence

The premise the writer states, services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state/ec2_resources.rs:466-487 — the doc, and the field list that carries no placement id:

    /// What a host launch needs to know about the host and the environment it joins.
    ///
    /// Every field is a fact the request or the stored environment already carries: nothing
    /// here is invented, so the instance EC2 ends up holding describes the same host Amazon
    /// EVS reports.
    pub struct HostLaunch<'a> {
        pub region: &'a str,
        pub instance_type: &'a str,
        pub key_name: &'a str,
        pub management_ip: &'a str,
        pub management_subnet_id: &'a str,
        pub service_access_subnet_id: &'a str,
    }

The placement is left at the default, src/state/ec2_resources.rs:632-635:

        // Amazon EVS configures none of the option sets an EC2 instance reports (its
        // metadata options, enclave, tenancy and the rest), so every one of them is at its
        // documented default.
        detail: ec2::InstanceDetail::default(),

…although the very same function had just resolved both ids in that store, src/state/ec2_resources.rs:377-385:

        if let Some(dedicated_host_id) = host
            .dedicated_host_id
            .filter(|id| !store.dedicated_hosts.contains_key(*id))
        {
            failures.push(invalid_dedicated_host_clause(

EC2's own record says those two fields are what a LAUNCH records, services/service-com-amazonaws-ec2-amazonec2/src/state/instance_detail.rs:209-222:

    /// The NAME of the placement group the launch put the instance in
    /// (`Placement$GroupName`), or `None` for an instance in none. …
    pub placement_group_name: Option<String>,
    /// The DEDICATED HOST the launch placed the instance on (`Placement$HostId`), or `None`
    /// for an instance on no host of the account. Already resolved against the host store by
    /// the launch, … and it is what makes the host's own `Instances` report that instance
    /// ([`ComAmazonawsEc2Amazonec2State::resync_dedicated_host_instances`]).
    pub host_id: Option<String>,

and EC2 states the dependency that list carries, services/service-com-amazonaws-ec2-amazonec2/src/state/dedicated_host_instances.rs:7-17:

    //! FOLLOWS from where the instances of the account were launched. The launch that places an
    //! instance on a host records that host on the instance
    //! ([`super::InstanceDetail::host_id`]), and the host's own member is RECOMPUTED from the
    //! instance store here …
    //! That is what makes the dependency a release has to honour real: "you must stop or terminate
    //! all instances on a host before it can be released" (`ReleaseHosts`) reads this one list, so
    //! a host carrying a running instance is reported in `Unsuccessful` instead of released under
    //! it.

The promise the EVS record makes, src/state.rs:1245-1251: "`ec2InstanceId` and `networkInterfaces` are reported from the EC2 resources the host's creation launched … so a caller can look the host's instance up in EC2 with the id reported here."

Smithy, `CreateEnvironmentHost` doc (smithy_query): "You can use the <code>dedicatedHostId</code> parameter to specify an Amazon EC2 Dedicated Host for ESX host creation." and "You can use the <code>placementGroupId</code> parameter to specify a cluster or partition placement group to launch EC2 instances into." — `HostInfoForCreate$placementGroupId`: "The unique ID of the placement group where the host is placed."

Test coverage: nothing pins the current behaviour. tests/all/created_environment_hosts.rs:545-560 is the only assertion about the launched instance ("The instance the account's own EC2 store holds for the first host describes that host: the launch is real, not a reported id") and checks `instanceType`, `keyName`, `privateIpAddress` and `subnetId` only — and it describes host index 0, while the placement row puts the id on index 1 (created_environment_hosts.rs:415-431). tests/all/host_placement_references.rs only asserts that an unheld id is refused; tests/all/host_ec2_instances.rs:556-568 only asserts that naming BOTH mechanisms is refused.

## Steps to Reproduce

1. Mount EVS and EC2 on one engine; allocate a Dedicated Host in EC2 (`AllocateHosts`) for `i4i.metal` and note its `h-…` id.
2. CreateEnvironment with a VCF version Amazon EVS installs, wait for `environmentState` CREATED (or use SELF_DEPLOYED).
3. CreateEnvironmentHost with `host.hostName=esx-1`, `host.instanceType=i4i.metal` and `host.dedicatedHostId=<the allocated id>`; note the returned `host.ec2InstanceId` (the EVS response echoes `dedicatedHostId`).
4. Call EC2 DescribeInstances with that instance id: `Placement.HostId` is absent (instance_detail.rs:931 reports `detail.host_id`, which is `None`).
5. Call EC2 DescribeHosts for the `h-…` id: `Instances` is empty, so `ReleaseHosts` releases a Dedicated Host that is running the ESX host instance (dedicated_host_instances.rs:14-17).
6. Repeat with `placementGroupId`: DescribeInstances reports `Placement.GroupName` as the empty string instead of the group the host was placed in.
