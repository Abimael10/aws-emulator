Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.get-image-findings-invalid-image-id-rejected ec2.modify-vpc-tenancy-to-dedicated-rejected ec2.traffic-mirror-filter-rule-number-out-of-range-rejected ec2.traffic-mirror-filter-rule-remove-fields-invalid-value-rejected ec2.register-image-missing-source-rejected ec2.create-key-pair-ed25519-fingerprint-is-sha256-of-public-key ec2.create-subnet-rejects-an-ipv6-cidr-block-in-a-vpc-with-no-ipv6-association ec2.network-interface-ipv4-prefix-released-on-delete ec2.describe-locked-snapshots-invalid-filter-name-rejected ec2.unlock-snapshot-not-locked-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.get-image-findings-invalid-image-id-rejected|run#3|AssertionError|assert code == "InvalidAMIID.Malformed", f"expected InvalidAMIID.Malformed, got {code!r}"
Issue-Key: ec2.modify-vpc-tenancy-to-dedicated-rejected|run#7|AssertionError|assert code == "UnsupportedTenancy", (
Issue-Key: ec2.traffic-mirror-filter-rule-number-out-of-range-rejected|run#8|AssertionError|assert code == "InvalidParameterValue", f"expected InvalidParameterValue, got {code!r}"
Issue-Key: ec2|ModifyTrafficMirrorFilterRule|ValidationError
Issue-Key: ec2.register-image-missing-source-rejected|run#12|AssertionError|assert message == "Either imageLocation or rootDeviceName must be set.", (
Issue-Key: ec2.create-key-pair-ed25519-fingerprint-is-sha256-of-public-key|run#9|AssertionError|assert created["KeyFingerprint"] == expected_fingerprint, (
Issue-Key: ec2.create-subnet-rejects-an-ipv6-cidr-block-in-a-vpc-with-no-ipv6-association|run#12|AssertionError|assert got == want, (
Issue-Key: ec2|CreateNetworkInterface|NotImplemented
Issue-Key: ec2.describe-locked-snapshots-invalid-filter-name-rejected|run#3|AssertionError|assert code == "InvalidFilter", f"expected InvalidFilter, got {code!r}"
Issue-Key: ec2.unlock-snapshot-not-locked-rejected|run#10|AssertionError|assert code == "SnapshotLockNotFound", f"expected SnapshotLockNotFound, got {code!r}"
Issue-Commit: 85ee122b95e0a025d1600c3d9babe834193f0ff3

# ec2: 10 more net-new causes — GetImageFindings is not routed (InvalidAction), ModifyVpcTenancy/traffic-mirror refusals answer shape-validation classes, the ed25519 key fingerprint is wrong, and five request shapes still answer NotImplemented

> _Note: this issue was identified against the code base at commit `85ee122b95e0a025d1600c3d9babe834193f0ff3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetImageFindings

## Description
Ten independent causes left over after the refusal-code and refusal-message clusters. Causes 7-10 are reached through an unimplemented request shape: the open issues 20261022T193451Z-ec2-createsubnet-stub.md and the DescribeLockedSnapshots / traffic-mirror stub issues filed for this suite are NEIGHBOURS of these checks but do not carry their replay keys, so they are listed here with the refusal each one hides.

Cause 1 of 10: GetImageFindings is not routed at all.
AWS: GetImageFindings with a malformed image id answers `InvalidAMIID.Malformed`.
Emulator: `InvalidAction` — the operation name is not dispatched, so the request never reaches a handler.
Why: a client cannot distinguish "this API does not exist in this region" from "your image id is wrong"; the whole operation is invisible.
Fix: route GetImageFindings to a handler and validate the image id there.
Traces: ec2.get-image-findings-invalid-image-id-rejected.

Cause 2 of 10: ModifyVpcTenancy(InstanceTenancy='dedicated').
AWS: accepts the request on the wire and refuses it with `UnsupportedTenancy`.
Emulator: `ValidationError` "1 validation error detected: Value 'dedicated' at 'instanceTenancy' failed to satisfy constraint: Member must satisfy enum value set: [default]" — strict enum rejection at deserialization.
Why: Smithy's VpcTenancy enum holds only `default`, but AWS's wire behaviour is a service-level refusal, not a shape error; clients handling UnsupportedTenancy see an unrelated code.
Fix: ModifyVpcTenancy handler — accept the member and refuse with UnsupportedTenancy rather than enum-validating it away.
Traces: ec2.modify-vpc-tenancy-to-dedicated-rejected.

Cause 3 of 10: CreateTrafficMirrorFilterRule with a RuleNumber outside the valid range.
AWS: `InvalidParameterValue`.
Emulator: `MissingParameter` — it blames an absent parameter for an out-of-range one.
Fix: CreateTrafficMirrorFilterRule validation.
Traces: ec2.traffic-mirror-filter-rule-number-out-of-range-rejected.

Cause 4 of 10: ModifyTrafficMirrorFilterRule with an invalid RemoveFields value.
AWS: `InvalidParameterValue`.
Emulator: `ValidationError`.
Fix: the traffic-mirror rule modify validation.
Traces: ec2.traffic-mirror-filter-rule-remove-fields-invalid-value-rejected.

Cause 5 of 10: RegisterImage with neither ImageLocation nor RootDeviceName.
AWS: InvalidParameterCombination with AWS's own sentence for the missing-source combination.
Emulator: InvalidParameterCombination with the emulator-authored "Either imageLocation or rootDeviceName must be specified".
Fix: RegisterImage validation message.
Traces: ec2.register-image-missing-source-rejected.

Cause 6 of 10: CreateKeyPair(KeyType='ed25519') reports a KeyFingerprint that is not the SHA-256 fingerprint of the key it returned.
AWS: for an ed25519 key the KeyFingerprint is the base64 SHA-256 digest of the public key derived from the returned private key — base64 INCLUDING its '=' padding, prefixed 'SHA256:'.
Emulator: returns the same digest with the trailing padding character dropped, so the value does not match the fingerprint recomputed from the material it just handed out.
Why: a client that verifies the key it received (ssh-keygen -lf, or re-deriving the fingerprint to match an instance's reported key) concludes the key pair is not the one AWS created.
Fix: the CreateKeyPair fingerprint computation/serialization.
Traces: ec2.create-key-pair-ed25519-fingerprint-is-sha256-of-public-key.

Cause 7 of 10: CreateSubnet with an Ipv6CidrBlock in a VPC that has no IPv6 association.
AWS: refuses with `InvalidSubnet.Range`, message "The IPv6 CIDR '<ipv6-cidr>' is invalid." (<ipv6-cidr> echoed from the request), and creates no subnet.
Emulator: `NotImplemented` "operation CreateSubnet with an IPv6 CIDR block is scaffolded but not implemented in this emulator".
Why: the IPv6 admission rule cannot exist while the shape is a stub; related open issue 20261022T193451Z-ec2-createsubnet-stub.md covers the stub but not this refusal's key.
Fix: the CreateSubnet handler's IPv6 path and its range validation.
Traces: ec2.create-subnet-rejects-an-ipv6-cidr-block-in-a-vpc-with-no-ipv6-association.

Cause 8 of 10: CreateNetworkInterface with assigned IPv4 prefixes.
AWS: creates the interface with Ipv4Prefixes and releases those prefixes back to the subnet on DeleteNetworkInterface.
Emulator: `NotImplemented` "operation CreateNetworkInterface with assigned IPv4 prefixes is scaffolded but not implemented in this emulator".
Why: prefix-delegated ENIs (EKS/CNI prefix mode) cannot be modelled at all, so the prefix-release behaviour is unobservable. Smithy: CreateNetworkInterface declares Ipv4Prefixes/Ipv4PrefixCount and errors: [].
Fix: the CreateNetworkInterface handler's prefix assignment and the delete path's prefix release.
Traces: ec2.network-interface-ipv4-prefix-released-on-delete.

Cause 9 of 10: DescribeLockedSnapshots filter validation.
AWS: an unknown filter name answers `InvalidFilter`.
Emulator: `NotImplemented` — the operation is a stub, so no filter is ever parsed. Smithy: DescribeLockedSnapshots takes Filters and MaxResults @range(min: 5, max: 1000).
Fix: the DescribeLockedSnapshots handler and its filter validation (a stub issue for the operation itself is filed separately; this cause is the InvalidFilter contract it hides).
Traces: ec2.describe-locked-snapshots-invalid-filter-name-rejected.

Cause 10 of 10: UnlockSnapshot on a snapshot with no lock.
AWS: refuses with `SnapshotLockNotFound`.
Emulator: `NotImplemented`.
Why: snapshot-lock lifecycle automation cannot tell "not locked" from "unsupported". Smithy: UnlockSnapshot requires SnapshotId and declares errors: [].
Fix: the UnlockSnapshot handler.
Traces: ec2.unlock-snapshot-not-locked-rejected.

## Evidence
Replay output per cause:
- get-image-findings-invalid-image-id-rejected: "expected InvalidAMIID.Malformed, got 'InvalidAction'"
- modify-vpc-tenancy-to-dedicated-rejected: "expected UnsupportedTenancy for ModifyVpcTenancy(InstanceTenancy='dedicated'), got 'ValidationError' (\"1 validation error detected: Value 'dedicated' at 'instanceTenancy' failed to satisfy constraint: Member must satisfy enum value set: [default]\")"
- traffic-mirror-filter-rule-number-out-of-range-rejected: "expected InvalidParameterValue, got 'MissingParameter'"
- traffic-mirror-filter-rule-remove-fields-invalid-value-rejected: "expected code 'InvalidParameterValue', got 'ValidationError'"
- register-image-missing-source-rejected: "unexpected InvalidParameterCombination message 'Either imageLocation or rootDeviceName must be specified'"
- create-key-pair-ed25519-fingerprint-is-sha256-of-public-key: "CreateKeyPair returned KeyFingerprint 'SHA256:0PlksmOM6t5toeyP8pyyNH0UBQgsknBKKhBRnG1x8pU', expected the SHA-256 fingerprint '0PlksmOM6t5toeyP8pyyNH0UBQgsknBKKhBRnG1x8pU=' of the returned private key's own public key" (note the dropped base64 padding)
- create-subnet-rejects-an-ipv6-cidr-block-in-a-vpc-with-no-ipv6-association: "CreateSubnet with an Ipv6CidrBlock in a VPC with no IPv6 block answered ('NotImplemented', 'operation CreateSubnet with an IPv6 CIDR block is scaffolded but not implemented in this emulator'), expected ('InvalidSubnet.Range', \"The IPv6 CIDR '2001:db8::/64' is invalid.\")"
- network-interface-ipv4-prefix-released-on-delete: "ClientError (NotImplemented) when calling the CreateNetworkInterface operation: operation CreateNetworkInterface with assigned IPv4 prefixes is scaffolded but not implemented in this emulator"
- describe-locked-snapshots-invalid-filter-name-rejected: "expected InvalidFilter, got 'NotImplemented'"
- unlock-snapshot-not-locked-rejected: "expected SnapshotLockNotFound, got 'NotImplemented'"

## Steps to Reproduce
Replay the checks in trace_ids. Minimal reproductions:
1. GetImageFindings(ImageId='ami-bad') -> expect InvalidAMIID.Malformed, not InvalidAction.
2. CreateVpc, ModifyVpcTenancy(InstanceTenancy='dedicated') -> expect UnsupportedTenancy.
3. CreateTrafficMirrorFilterRule with RuleNumber out of range -> expect InvalidParameterValue; ModifyTrafficMirrorFilterRule with a bogus RemoveFields value -> expect InvalidParameterValue.
4. RegisterImage with neither ImageLocation nor RootDeviceName.
5. CreateKeyPair(KeyType='ed25519'); recompute the SHA-256 fingerprint of the returned private key's public half and compare to KeyFingerprint.
6. CreateVpc without IPv6, CreateSubnet with Ipv6CidrBlock -> expect InvalidSubnet.Range.
7. CreateNetworkInterface with Ipv4Prefixes, then DeleteNetworkInterface and confirm the prefixes are released.
8. DescribeLockedSnapshots with an unknown filter name -> expect InvalidFilter.
9. UnlockSnapshot on an unlocked snapshot -> expect SnapshotLockNotFound.
