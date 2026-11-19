Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 6e2d7185f30747a2356e601c1d412ac8e0be11c4

# host.keyName is the one EC2 reference accepted unresolved, on the premise that EC2 holds no key pairs — a premise the EC2 crate now contradicts

_Filed: 2026-11-19T03:34:01Z_
_Operation: CreateEnvironmentHost | Class: validation_

> _Note: this issue was identified against the code base at commit `6e2d7185f30747a2356e601c1d412ac8e0be11c4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other Amazon EC2 resource a host or an environment names is resolved against the account's own EC2 store before the request is accepted: `vpcId`, `serviceAccessSubnetId`, `serviceAccessSecurityGroups`, `initialVlans.hcxNetworkAclId`, `connectivityInfo.privateRouteServerPeerings`, `host.placementGroupId`, `host.dedicatedHostId` and `allocationId` each draw a refusal when the readable store holds no such record. `host.keyName` — "The name of the SSH key that is used to access the host" — is accepted whatever it names, and the handler doc justifies that by asserting the account's EC2 store holds no key pairs at all because every `…KeyPair` operation of EC2 is still a scaffold stub. That premise is false: EC2 holds a `key_pairs` store and both `CreateKeyPair` and `ImportKeyPair` are implemented writers into it, while `DescribeKeyPairs` refuses a name the store does not hold.

So a host is created, and an EC2 instance is launched carrying `KeyName` = that string, for a key pair the account provably does not hold — a host nothing can SSH into, reported as if its key existed. The same string reaches the launch through `CreateEnvironment`'s `hosts` entries as well.

## Evidence

The stated exemption, services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/ops/create_environment_host.rs:62-66:

    //! `keyName` is the one member of a host naming an EC2 resource that is NOT resolved:
    //! the account's Amazon EC2 store holds no key pairs at all — every `…KeyPair` operation
    //! of that service is still a scaffold stub, so nothing can put one there — and a store
    //! that holds no records of a kind is no evidence about any one name, the same reading
    //! this service takes of an EC2 store it cannot read at all.

The EC2 crate contradicts it. services/service-com-amazonaws-ec2-amazonec2/src/state/store.rs:3406:

    pub key_pairs: BTreeMap<String, KeyPairRecord>,

services/service-com-amazonaws-ec2-amazonec2/src/ops/create_key_pair.rs:3-9:

    //! @op CreateKeyPair
    //!
    //! Implemented: generates a key pair of the type the request asked for, stores its PUBLIC
    //! half in this service's key pair store and hands the caller the private half once

(and create_key_pair.rs:315 `.insert_key_pair(spec)`), services/service-com-amazonaws-ec2-amazonec2/src/ops/import_key_pair.rs:5-9:

    //! Implemented: stores the PUBLIC half of a key pair the caller created with a third-party
    //! tool, in the ONE key pair store this service has
    //! ([`crate::state::ComAmazonawsEc2Amazonec2State::key_pairs`], the same store
    //! `CreateKeyPair` writes to)

and services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_key_pairs.rs:242-252 — "a name or an id that names no key pair of the account is an error, not an absent row" — so a name the store lacks is refused there while Amazon EVS admits it.

The sibling rule this crate already applies to a readable store that holds no record of the named id, services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state/ec2_resources.rs:281-290:

    network_acls
        .iter()
        .filter(|acl| {
            store
                .network_acls
                .get(acl.network_acl_id)
                .is_none_or(|held| held.vpc_id.as_deref() != Some(vpc_id))
        })
        .map(|acl| invalid_network_acl_clause(acl.member, acl.network_acl_id, vpc_id))

and the handler's own rule 6 for the other two EC2 references of the SAME `host` member, create_environment_host.rs:43-48 ("Amazon EVS creates neither, so a host that names one names a resource the account already holds"). The unresolved name is then written onto the EC2 instance: src/state/ec2_resources.rs:584 `key_name: Some(launch.key_name.to_string()),`.

Test coverage: tests/all/host_ec2_instances.rs passes key names ("evs-key", "second_key-2", "THIRD-KEY") that no EC2 CreateKeyPair ever created, and tests/all/host_placement_references.rs covers the placement references that ARE resolved; no test states a reason for admitting an unknown key pair name.

## Steps to Reproduce

1. Call EC2 CreateKeyPair with KeyName evs-key, so the account's key-pair store holds exactly that one pair.
2. CreateEnvironment (VCF-5.2.1) and wait for environmentState CREATED.
3. CreateEnvironmentHost with host.keyName set to no-such-key (and a valid hostName/instanceType): the call answers 200 and the host is created.
4. Call EC2 DescribeKeyPairs with KeyNames=[no-such-key]: InvalidKeyPair.NotFound — the host was admitted with a key pair the account does not hold, while the same request naming an unknown host.placementGroupId is refused with ValidationException.
