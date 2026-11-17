Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.asvpce-streaming-vpce-round-trips-interface-shape ec2.ipam-vpc-from-localeless-top-pool-rejected ec2.ipam-vpc-missing-pool-required-resource-tag-rejected ec2.ipam-release-of-vpc-owned-allocation-frees-the-cidr-for-reuse
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.asvpce-streaming-vpce-round-trips-interface-shape|run#24|AssertionError|assert len(dns) == want_count, (
Issue-Key: ec2.ipam-vpc-from-localeless-top-pool-rejected|_vpc_refusal#3|AssertionError|raise AssertionError(
Issue-Key: ec2.ipam-vpc-missing-pool-required-resource-tag-rejected|_vpc_refusal#7|AssertionError|raise AssertionError(
Issue-Key: ec2.ipam-release-of-vpc-owned-allocation-frees-the-cidr-for-reuse|run#12|AssertionError|assert mismatch["Code"] == "InvalidParameterCombination", (
Issue-Commit: 7ce5552927764f5e5adc0c494ca5a7d589c732e6

# EC2: 4 net-new causes — interface-endpoint DnsEntries disagree with the service catalogue's PrivateDnsNames, CreateVpc from an IPAM pool skips the locale and AllocationResourceTags gates, and ReleaseIpamPoolAllocation answers NotImplemented

> _Note: this issue was identified against the code base at commit `7ce5552927764f5e5adc0c494ca5a7d589c732e6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateVpc

## Description
Four distinct net-new root causes in the EC2 emulator, all grounded in failing behavioral checks of the `ec2` suite. Each must be closed before this issue is submitted.

Cause 1 of 4: DescribeVpcEndpoints DnsEntries for a private-DNS interface endpoint do not agree with DescribeVpcEndpointServices PrivateDnsNames.
AWS: Smithy (com.amazonaws.ec2) — DescribeVpcEndpoints -> VpcEndpoint$DnsEntries : DnsEntrySet of DnsEntry{DnsName, HostedZoneId}; DescribeVpcEndpointServices -> ServiceDetail$PrivateDnsNames. For an Interface endpoint created with PrivateDnsEnabled defaulting to true against an AWS-owned endpoint service, AWS reports exactly one regional endpoint-specific entry (<vpce-id>-<hash>.<service>.<region>.vpce.amazonaws.com), one zonal endpoint-specific entry per subnet the endpoint is placed in (<vpce-id>-<hash>-<az>.<service>.<region>.vpce.amazonaws.com), and one entry per private DNS name the service catalogue advertises for that service in ServiceDetail$PrivateDnsNames — so the entry count is 1 + (number of subnets) + (number of advertised private names), and the non-endpoint-specific entries are exactly the advertised private names. The two reads are two views of the same catalogue fact and cannot disagree. (Private-name composition beyond that is recorded behavior only.)
Emulator: for a single-subnet Interface endpoint on com.amazonaws.<region>.appstream.streaming, DescribeVpcEndpoints returns 4 DnsEntries — one regional entry, one zonal entry, and TWO private names ('streaming.appstream.<region>.amazonaws.com' and 'streaming.appstream.<region>.api.aws') — while DescribeVpcEndpointServices advertises only ONE PrivateDnsName for the same service, so the expected count was 1+1+1 = 3. The emulator serves an extra private-DNS entry (the dual-stack '.api.aws' name) that its own service catalogue does not advertise.
Why: a client that resolves the endpoint by walking the catalogue (ServiceDetail$PrivateDnsNames) and then matching DnsEntries sees an entry with no catalogue provenance; code that counts DnsEntries to derive zonal placement (1 + subnets + private names) miscounts, and DNS-wiring automation installs a private name AWS never publishes for that endpoint.
Fix: the DescribeVpcEndpoints serializer/state for VPC endpoints and the endpoint-service catalogue must derive private DnsEntries from one source — the same PrivateDnsNames the catalogue advertises for the service — instead of synthesizing an additional '.api.aws' name in the endpoint read path.
Traces: ec2.asvpce-streaming-vpce-round-trips-interface-shape.

Cause 2 of 4: CreateVpc against a locale-less IPAM pool is accepted and allocates a CIDR.
AWS: Smithy — CreateVpc input member Ipv4IpamPoolId ('The ID of an IPv4 IPAM pool you want to use for allocating this VPC's CIDR'), with Ipv4NetmaskLength. A pool with no Locale cannot allocate to a Region-scoped resource; AWS refuses the request with HTTP 400, error code InvalidParameterValue and the message 'The resource type vpc is not valid for pool <ipam-pool-id> with None locale.' — fixed text 'The resource type ', ' is not valid for pool ' and ' with None locale.' around the resource type being allocated ('vpc') and the IpamPoolId taken from the request; the refusal leaves no allocation in the pool.
Emulator: the call SUCCEEDS. CreateVpc(Ipv4IpamPoolId=<locale-less top pool>, Ipv4NetmaskLength=24) returns an available VPC carrying a /24 drawn from the pool, and the pool records the allocation. No refusal is produced at all.
Why: a client relying on AWS's locale model builds its pool hierarchy wrong and never learns that only a regional child pool may back a VPC; worse, state that AWS refuses to create exists in the emulator, so the subsequent allocation accounting and any test of the documented error path silently invert.
Fix: CreateVpc's IPAM validation (the operation's handler, before any allocation is written) must reject a pool whose Locale is unset for a vpc allocation, with the code and message above, and leave the pool's allocations untouched.
Traces: ec2.ipam-vpc-from-localeless-top-pool-rejected.

Cause 3 of 4: CreateVpc ignores the pool's AllocationResourceTags gate.
AWS: Smithy — IpamPool$AllocationResourceTags (round-tripped by DescribeIpamPools, and the emulator does round-trip it). A pool that requires resource tags allocates only to a resource carrying every required key AND value; a request with the tag absent, and a request whose required key is present with a different value, are both refused with HTTP 400, error code InvalidParameterValue and the message 'The resource is missing one or more of the resource tags required by the IPAM pool.' (entirely fixed text — no request value appears in it), and neither refusal leaves an allocation in the pool.
Emulator: both requests SUCCEED. CreateVpc(Ipv4IpamPoolId=<pool requiring env=dev>, Ipv4NetmaskLength=24) with no TagSpecifications returns an available VPC with a /24 from the pool; the tag rule is stored and read back but never enforced at allocation time.
Why: AllocationResourceTags is the mechanism accounts use to keep an IPAM pool's address space reserved for tagged workloads. An emulator that stores the rule but allocates regardless lets a client believe its tag-based guardrail works while every untagged or mis-tagged resource draws address space.
Fix: CreateVpc's IPAM allocation validation must evaluate the pool's AllocationResourceTags against the request's TagSpecifications for the vpc resource type and refuse as above before allocating.
Traces: ec2.ipam-vpc-missing-pool-required-resource-tag-rejected.

Cause 4 of 4: ReleaseIpamPoolAllocation answers NotImplemented, so the whole release path (including its refusal classes) is unreachable.
AWS: Smithy — ReleaseIpamPoolAllocation(IpamPoolId required, Cidr required, IpamPoolAllocationId required) -> {Success: Boolean}. Releasing a VPC-owned allocation removes the pool's record of it while the VPC keeps its CIDR, so the identical block becomes allocatable again; a request whose Cidr does not match the allocation named by IpamPoolAllocationId is refused as a 400 client error with code InvalidParameterCombination (recorded behavior only for the message text).
Emulator: every call answers HTTP 501 with error code 'NotImplemented' — the handler is still a scaffold stub, so neither the success path nor the mismatched-CIDR refusal exists. NOTE: the open issue 20261116T044838Z-ec2-releaseipampoolallocation-stub.md names this stub and its handler file; this check is additional grounding for it and records the semantics the implementation must satisfy (release frees the CIDR for reuse while the VPC survives, and a mismatched Cidr is InvalidParameterCombination, not NotImplemented).
Why: a client cannot reclaim pool space at all, and a client that handles AWS's 400 InvalidParameterCombination sees a 501 instead — a status class it treats as a transport/feature failure rather than its own bad request, so it retries forever instead of correcting the Cidr.
Fix: implement the ReleaseIpamPoolAllocation handler (its state mutation on the pool's allocation list and its parameter-combination validation).
Traces: ec2.ipam-release-of-vpc-owned-allocation-frees-the-cidr-for-reuse.

## Evidence
Cause 1 — ec2.asvpce-streaming-vpce-round-trips-interface-shape (mismatch):
  AssertionError: a private-DNS endpoint carries a regional entry, one zonal entry per subnet and one entry per advertised private name (3); it carries ['vpce-...-....streaming.appstream.us-west-2.vpce.amazonaws.com', 'vpce-...-...-us-west-2a.streaming.appstream.us-west-2.vpce.amazonaws.com', 'streaming.appstream.us-west-2.amazonaws.com', 'streaming.appstream.us-west-2.api.aws']
  Expected: len(DnsEntries) == 1 + len(SubnetIds) + len(ServiceDetail.PrivateDnsNames) (= 3 here: the catalogue advertises one private name); non-endpoint-specific entries == the advertised private names.
  Actual: 4 entries, the extra one being the '.api.aws' private name the catalogue does not advertise.

Cause 2 — ec2.ipam-vpc-from-localeless-top-pool-rejected (mismatch):
  AssertionError: CreateVpc against <locale-less ipam pool id> with {'Ipv4NetmaskLength': 24} succeeded as <vpc id> ('10.0.0.0/24'); real AWS rejects it
  Expected: HTTP 400, Code 'InvalidParameterValue', Message 'The resource type vpc is not valid for pool <ipam-pool-id> with None locale.', and no allocation left in the pool.
  Actual: HTTP 200 with a provisioned VPC and a recorded pool allocation.

Cause 3 — ec2.ipam-vpc-missing-pool-required-resource-tag-rejected (mismatch):
  AssertionError: CreateVpc against <child pool requiring env=dev> with {'Ipv4NetmaskLength': 24} succeeded as <vpc id> ('10.0.0.0/24'); real AWS rejects it
  Expected (both for an untagged VPC and for one tagged env=prod): HTTP 400, Code 'InvalidParameterValue', Message 'The resource is missing one or more of the resource tags required by the IPAM pool.', no allocation left behind. DescribeIpamPools already reports AllocationResourceTags=[{'Key':'env','Value':'dev'}].
  Actual: HTTP 200 with a provisioned VPC drawing from the pool.

Cause 4 — ec2.ipam-release-of-vpc-owned-allocation-frees-the-cidr-for-reuse (mismatch):
  AssertionError: releasing with a mismatched CIDR returned 'NotImplemented'
  Expected: HTTP 400 with Code 'InvalidParameterCombination' for a Cidr that does not match the named IpamPoolAllocationId; and on the success path, Success=true with the pool's allocation gone while the VPC keeps its CIDR.
  Actual: HTTP 501, Code 'NotImplemented' for every ReleaseIpamPoolAllocation call.

## Steps to Reproduce
Replay the four named checks against the emulator (behavioral suite 'simparity-behavioral-aws-v1', service heading `ec2`):

1. ec2.asvpce-streaming-vpce-round-trips-interface-shape — create a VPC with one subnet in an AZ the appstream.streaming endpoint service is offered in, CreateVpcEndpoint(VpcEndpointType='Interface', ServiceName='com.amazonaws.<region>.appstream.streaming', SubnetIds=[one subnet], SecurityGroupIds=[sg]) with PrivateDnsEnabled omitted; wait for state 'available'; compare DescribeVpcEndpoints(...).VpcEndpoints[0].DnsEntries against DescribeVpcEndpointServices(ServiceNames=[same]).ServiceDetails[0].PrivateDnsNames.
2. ec2.ipam-vpc-from-localeless-top-pool-rejected — create an IPAM, a private scope and a top pool with NO Locale, provision 10.0.0.0/8 into it, then CreateVpc(Ipv4IpamPoolId=<top pool>, Ipv4NetmaskLength=24).
3. ec2.ipam-vpc-missing-pool-required-resource-tag-rejected — create a child pool with AllocationResourceTags=[{'Key':'env','Value':'dev'}], provision a /16, then CreateVpc(Ipv4IpamPoolId=<child>, Ipv4NetmaskLength=24) with no TagSpecifications, and again with TagSpecifications=[{'ResourceType':'vpc','Tags':[{'Key':'env','Value':'prod'}]}].
4. ec2.ipam-release-of-vpc-owned-allocation-frees-the-cidr-for-reuse — provision a /16 into a child pool, CreateVpc from it (Ipv4NetmaskLength=24), read the VPC-owned allocation, then ReleaseIpamPoolAllocation(IpamPoolId=<child>, IpamPoolAllocationId=<allocation id>, Cidr=<a CIDR that does not match>) and observe the status/code.
