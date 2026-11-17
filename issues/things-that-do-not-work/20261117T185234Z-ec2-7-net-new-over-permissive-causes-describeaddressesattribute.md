Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.describe-addresses-attribute-max-results-below-the-allocation-count-rejected ec2.diagnostic-interrupt-graviton-processor-rejected ec2.modify-subnet-attribute-assign-ipv6-without-ipv6-cidr-rejected ec2.modify-transit-gateway-misaligned-cidr-block-rejected ec2.run-instances-enclave-options-rejected-on-unsupported-type ec2.traffic-mirror-filter-rule-port-range-requires-tcp-udp-rejected ec2.traffic-mirror-filter-rule-target-lifecycle
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.describe-addresses-attribute-max-results-below-the-allocation-count-rejected|_client_error#9|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded and returned {result!r}")
Issue-Key: ec2.diagnostic-interrupt-graviton-processor-rejected|_error#8|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded and returned {result!r}")
Issue-Key: ec2.modify-subnet-attribute-assign-ipv6-without-ipv6-cidr-rejected|_error_code#8|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Key: ec2.modify-transit-gateway-misaligned-cidr-block-rejected|_error#9|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: ec2.run-instances-enclave-options-rejected-on-unsupported-type|_error#5|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: ec2.traffic-mirror-filter-rule-port-range-requires-tcp-udp-rejected|_error_code#8|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Key: ec2.traffic-mirror-filter-rule-target-lifecycle|run#25|AssertionError|assert not problems, "unexpected traffic-mirror semantics:\n" + "\n".join(problems)
Issue-Commit: 85ee122b95e0a025d1600c3d9babe834193f0ff3

# ec2: 7 net-new over-permissive causes — DescribeAddressesAttribute, SendDiagnosticInterrupt, ModifySubnetAttribute, ModifyTransitGateway, RunInstances enclave options and the traffic-mirror rule/target paths accept requests real EC2 refuses

> _Note: this issue was identified against the code base at commit `85ee122b95e0a025d1600c3d9babe834193f0ff3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifySubnetAttribute

## Description
Seven independent causes of the same shape: the emulator answers 200 and MUTATES state for requests real EC2 refuses with a 400. Every one of them hides a client bug and leaves the emulator holding a configuration AWS would never have. None of the ops carries modeled errors in the EC2 Smithy model (errors: []), so the refusal is recorded behavior; the status/code is quoted from the recording per cause.

Cause 1 of 7: DescribeAddressesAttribute with MaxResults below the number of allocations.
AWS: refuses the page size with a 400 ClientError (recorded behavior only).
Emulator: 200, returning the Addresses page.
Why: paginating clients never learn their page size is invalid and silently see a truncated address inventory.
Fix: DescribeAddressesAttribute MaxResults validation.
Traces: ec2.describe-addresses-attribute-max-results-below-the-allocation-count-rejected.

Cause 2 of 7: SendDiagnosticInterrupt against a Graviton (arm64) instance.
AWS: refuses — the NMI diagnostic interrupt is unsupported on Graviton processors — with a 400 ClientError.
Emulator: 200 with an empty body.
Why: a caller believes it has triggered a kernel panic/crash dump on an instance where AWS cannot; recovery automation waits for a dump that never appears.
Fix: SendDiagnosticInterrupt handler — gate on the instance type's processor before accepting.
Traces: ec2.diagnostic-interrupt-graviton-processor-rejected.

Cause 3 of 7: ModifySubnetAttribute setting AssignIpv6AddressOnCreation on a subnet with no IPv6 CIDR association.
AWS: refuses with a 400 ClientError.
Emulator: 200, and the attribute is set.
Why: the subnet is left advertising IPv6-on-create with no IPv6 range to allocate from — a state AWS forbids, so later launches diverge unpredictably.
Fix: ModifySubnetAttribute validation — require an IPv6 association for the IPv6 attributes.
Traces: ec2.modify-subnet-attribute-assign-ipv6-without-ipv6-cidr-rejected.

Cause 4 of 7: ModifyTransitGateway adding a misaligned TransitGatewayCidrBlock.
AWS: refuses the misaligned CIDR with a 400 ClientError.
Emulator: 200, State 'modifying', and the block is appended to Options.TransitGatewayCidrBlocks.
Why: the transit gateway persists a CIDR block AWS would never store, so downstream Connect/peer address assignment is built on an impossible range.
Fix: ModifyTransitGateway CIDR validation.
Traces: ec2.modify-transit-gateway-misaligned-cidr-block-rejected.

Cause 5 of 7: RunInstances with EnclaveOptions.Enabled=true on an instance type that does not support Nitro Enclaves.
AWS: refuses the launch with a 400 ClientError and launches nothing.
Emulator: 200 and a running reservation.
Why: the emulator boots an instance whose advertised enclave capability does not exist, so an enclave-dependent workload appears deployable and fails only in production.
Fix: RunInstances admission — validate EnclaveOptions against the instance type's supported features.
Traces: ec2.run-instances-enclave-options-rejected-on-unsupported-type.

Cause 6 of 7: CreateTrafficMirrorFilterRule with a DestinationPortRange/SourcePortRange while Protocol is neither TCP (6) nor UDP (17).
AWS: refuses — port ranges are only meaningful for TCP/UDP — with a 400 ClientError.
Emulator: 200, returning a TrafficMirrorFilterRule that carries the port range.
Why: the stored rule can never match traffic the way the caller intends, and the misconfiguration is invisible.
Fix: CreateTrafficMirrorFilterRule validation — bind port ranges to the protocol number.
Traces: ec2.traffic-mirror-filter-rule-port-range-requires-tcp-udp-rejected.

Cause 7 of 7: the traffic-mirror target/session pair.
AWS: CreateTrafficMirrorTarget naming a network interface that cannot be a mirror target is refused with `NetworkInterfaceNotSupported`; CreateTrafficMirrorSession with an unusable/absent target is refused with one of InvalidParameterValue / InvalidTrafficMirrorTargetID.NotFound / NetworkInterfaceNotSupported.
Emulator: CreateTrafficMirrorTarget SUCCEEDS on the unsupported interface, and CreateTrafficMirrorSession answers `NotImplemented` — so neither refusal exists and the session path is unreachable.
Why: traffic-mirroring setup appears to succeed against an interface that cannot mirror, and the session call that would surface it is not served at all.
Fix: CreateTrafficMirrorTarget validation (interface eligibility) and the CreateTrafficMirrorSession handler.
Traces: ec2.traffic-mirror-filter-rule-target-lifecycle.

## Evidence
Replay output per cause:
- describe-addresses-attribute-max-results-below-the-allocation-count-rejected: "expected a ClientError, but the call succeeded and returned {'Addresses': [...], 'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}"
- diagnostic-interrupt-graviton-processor-rejected: "expected a ClientError, but the call succeeded and returned {'ResponseMetadata': {'HTTPStatusCode': 200, ...}}"
- modify-subnet-attribute-assign-ipv6-without-ipv6-cidr-rejected: "expected the call to be rejected, but it succeeded and returned {... 'HTTPStatusCode': 200 ...}"
- modify-transit-gateway-misaligned-cidr-block-rejected: "expected the call to be rejected, but it succeeded: {'TransitGateway': {... 'State': 'modifying', 'Options': {'TransitGatewayCidrBlocks': ['10.100.0.0/16', '10.0.0.0/8'] ...}}}"
- run-instances-enclave-options-rejected-on-unsupported-type: "expected the call to be refused, but it succeeded: {'ReservationId': 'r-...', 'Instances': [{...}]}"
- traffic-mirror-filter-rule-port-range-requires-tcp-udp-rejected: "expected the call to be rejected, but it succeeded and returned {'TrafficMirrorFilterRule': {... 'DestinationPortRange': {'FromPort': 443, 'ToPort': 443} ...}}"
- traffic-mirror-filter-rule-target-lifecycle: "create_traffic_mirror_target: expected ['NetworkInterfaceNotSupported'] but the call SUCCEEDED" and "create_traffic_mirror_session: expected ['InvalidParameterValue', 'InvalidTrafficMirrorTargetID.NotFound', 'InvalidTrafficMirrorTargetId.NotFound', 'NetworkInterfaceNotSupported'], got NotImplemented"

Smithy (services/service-com-amazonaws-ec2-amazonec2): CreateTrafficMirrorTarget accepts NetworkInterfaceId | NetworkLoadBalancerArn | GatewayLoadBalancerEndpointId and declares errors: []; ModifyVpcTenancy/CreateNetworkInterface likewise carry no modeled errors, so these refusals are recorded behavior.

## Steps to Reproduce
Replay the checks in trace_ids. Minimal reproductions:
1. Allocate N elastic IPs, call DescribeAddressesAttribute with MaxResults < N -> expect a 400.
2. RunInstances a t4g/arm64 instance, call SendDiagnosticInterrupt on it -> expect a 400.
3. CreateVpc/CreateSubnet with IPv4 only, ModifySubnetAttribute AssignIpv6AddressOnCreation=true -> expect a 400.
4. CreateTransitGateway, ModifyTransitGateway with a misaligned TransitGatewayCidrBlock -> expect a 400.
5. RunInstances with EnclaveOptions={'Enabled': True} on an unsupported type -> expect a 400 and no instance.
6. CreateTrafficMirrorFilter, CreateTrafficMirrorFilterRule with Protocol=1 and a DestinationPortRange -> expect a 400.
7. CreateTrafficMirrorTarget on an ineligible ENI -> expect NetworkInterfaceNotSupported; then CreateTrafficMirrorSession -> expect a modelled refusal, not NotImplemented.
