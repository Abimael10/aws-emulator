Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-ipam-trajwire-vpc-from-top-pool-then-child
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-ipam-trajwire-vpc-from-top-pool-then-child ec2-ipam-trajwire-teardown-order-mistakes ec2-ipam-trajwire-reference-path-regional-child-pool
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|CreateVpc|4xx->2xx|$.Response
Issue-Key: ec2|CreateVpc|4xx->2xx|status
Issue-Key: ec2|CreateVpc|2xx->2xx|$.CreateVpcResponse.vpc.cidrBlock
Issue-Key: ec2|CreateVpc|2xx->2xx|$.CreateVpcResponse.vpc.cidrBlockAssociationSet.item.cidrBlock
Issue-Key: ec2|GetIpamPoolAllocations|2xx->2xx|$.GetIpamPoolAllocationsResponse.ipamPoolAllocationSet
Issue-Key: ec2|GetIpamPoolAllocations|2xx->2xx|$.GetIpamPoolAllocationsResponse.ipamPoolAllocationSet.item.resourceProvider
Issue-Related: 20261116T190237Z-ec2-responses-omit-members-aws-always-emits-in.md
Issue-Commit: 688abb98d0ad3ae2c07107aa110c7330aa2e99f9

# ec2 IPAM pool allocation and CreateVpc provisioning diverge from AWS

> _Note: this issue was identified against the code base at commit `688abb98d0ad3ae2c07107aa110c7330aa2e99f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateVpc

## Description
Four net-new causes on the ec2 IPAM surface: a refusal that is not made at all, a CIDR allocated from the wrong place, an allocation that is never released from the pool view, and modelled allocation members that are never emitted. The open issues 20261116T044838Z-ec2-5-net-new-causes-createdefaultsubnet-and-deleteipampool.md and 20261117T035019Z-ec2-4-net-new-causes-interface-endpoint-dnsentries.md touch the same surface but carry none of these replay keys.

Cause 1 of 4: CreateVpc with Ipv4IpamPoolId naming a pool whose configuration does not permit a vpc allocation (a top-level pool with no locale).
AWS: recorded behavior only. 400, Code InvalidParameterValue, Message "The resource type <resourceType> is not valid for pool <ipamPoolId> with <locale> locale." — fixed text "The resource type " ... " is not valid for pool " ... " with " ... " locale."; the resource type is the allocation's resource type (here vpc), the pool id comes from the request's Ipv4IpamPoolId, and the locale is the pool's locale from state (rendered as the absent-locale token when the pool has none). Smithy: CreateVpc.Ipv4IpamPoolId -> IpamPoolId; the operation models no errors (ec2Query).
Emulator: HTTP 200 with a fully-formed CreateVpcResponse — the refusal is not made at all and a VPC is created from a pool AWS would not allocate from.
Why: this is the only cause here that changes the status. A client is told its VPC exists, and the locale/resource-type rule that IPAM exists to enforce is silently skipped, so subsequent allocations and the pool's accounting are built on a VPC AWS would never have created.
Fix: the CreateVpc handler's IPAM allocation path — check the pool's locale and allowed resource types before allocating, and raise InvalidParameterValue with AWS's message.
Traces: ec2-ipam-trajwire-vpc-from-top-pool-then-child#task#7.

Cause 2 of 4: CreateVpc with Ipv4IpamPoolId + Ipv4NetmaskLength allocates from the wrong pool's space.
AWS: recorded behavior only. The CIDR in CreateVpcResponse.vpc.cidrBlock (and the single cidrBlockAssociationSet item) must be carved from the CIDR provisioned to the named IPAM pool, at the requested netmask length, from the lowest free block of that pool.
Emulator: 200 but the returned cidrBlock is drawn from a different provisioned range than the pool named in the request (the recording shows the first free /24 of the named pool, the emulator a /24 one octet higher — i.e. a block belonging to another pool in the hierarchy). The specific values in the recording are not the requirement; the requirement is that the block come from the named pool's provisioned CIDR.
Why: a caller that provisioned a pool per environment and allocates from it gets a VPC in another environment's address space, and pool utilisation reporting is wrong for both pools.
Fix: the pool-selection/free-block search in the ec2 crate's IPAM allocation state, as used by CreateVpc.
Traces: ec2-ipam-trajwire-vpc-from-top-pool-then-child#task#14.

Cause 3 of 4: GetIpamPoolAllocations after the allocation has been released.
AWS: recorded behavior only. Once the allocation backing a resource is released, GetIpamPoolAllocations for that pool returns an empty ipamPoolAllocationSet. Smithy: GetIpamPoolAllocations output member IpamPoolAllocations -> IpamPoolAllocationSet, xmlName ipamPoolAllocationSet, @paginated.
Emulator: 200 but ipamPoolAllocationSet still carries the released allocation item (cidr, ipamPoolAllocationId, resourceId, resourceType, resourceRegion, resourceOwner, tagSet), so the pool never shows the space as free.
Why: the pool looks exhausted to a caller that re-reads it, and teardown-order checks that wait for a pool to drain never converge.
Fix: the release path in the ec2 crate's IPAM allocation state must remove the allocation record the GetIpamPoolAllocations handler reads. (ReleaseIpamPoolAllocation is itself an unimplemented scaffold in this build — covered by 20261116T044838Z-ec2-releaseipampoolallocation-stub.md — but this cluster is about the pool view after the release step, and is not blocked by a stub.)
Traces: ec2-ipam-trajwire-teardown-order-mistakes#task#17 (cluster also covers ec2-ipam-trajwire-vpc-from-top-pool-then-child#task#8).

Cause 4 of 4: GetIpamPoolAllocations omits allocation members AWS always emits.
AWS: recorded behavior only — the on-disk model's IpamPoolAllocation shape predates them (its members are Cidr, IpamPoolAllocationId, Description, ResourceId, ResourceType, ResourceRegion, ResourceOwner, Tags), so the requirement comes from the recording: each allocation item also carries allocationId, cidrBlock, resourceProvider (value 'aws' for an AWS-provisioned allocation) and status (value 'active' for a live allocation). allocationId and cidrBlock are per-allocation values; resourceProvider and status are enumerated tokens.
Emulator: 200 with those four elements absent from every allocation item.
Why: a client reading cidrBlock/status to decide whether an allocation is live sees nothing at all and cannot tell an active allocation from a pending one.
Fix: the GetIpamPoolAllocations serializer and the allocation record in the ec2 crate's IPAM state, which must carry the allocation's status and provider.
Traces: ec2-ipam-trajwire-reference-path-regional-child-pool#task#11.

## Evidence
Replay of trace set simparity-wire-aws-v1. Four mismatch clusters, service=ec2, empty stub_path, empty owned_by:

1. ec2-ipam-trajwire-vpc-from-top-pool-then-child#task#7 CreateVpc, expected_status 400 actual_status 200: "status: expected 400, got 200"; $.Response missing (expected {'Errors': {'Error': {'Code': 'InvalidParameterValue', 'Message': 'The resource type vpc is not valid for pool ipam-pool-00000000000000092 with None locale.'}}, 'RequestID': '<shaped-not-asserted>'}, got None).
2. ec2-ipam-trajwire-vpc-from-top-pool-then-child#task#14 CreateVpc 200->200: $.CreateVpcResponse.vpc.cidrBlock expected '10.0.0.0/24' got '10.1.0.0/24'; $.CreateVpcResponse.vpc.cidrBlockAssociationSet.item.cidrBlock expected '10.0.0.0/24' got '10.1.0.0/24'.
3. ec2-ipam-trajwire-teardown-order-mistakes#task#17 GetIpamPoolAllocations 200->200: $.GetIpamPoolAllocationsResponse.ipamPoolAllocationSet type_mismatch (expected '', got {'item': {'cidr': '10.0.0.0/24', 'ipamPoolAllocationId': 'ipam-pool-alloc-...', 'resourceId': 'vpc-...', 'resourceType': 'vpc', 'resourceRegion': 'us-west-2', 'resourceOwner': '000000000000', 'tagSet': ''}}). affected_count 2.
4. ec2-ipam-trajwire-reference-path-regional-child-pool#task#11 GetIpamPoolAllocations 200->200: ...ipamPoolAllocationSet.item.allocationId missing (expected shaped value, got None); .cidrBlock missing (expected '10.0.0.0/24', got None); .resourceProvider missing (expected 'aws', got None); .status missing (expected 'active', got None).

Model grounding (smithy_query, services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json): CreateVpc input members Ipv4IpamPoolId -> IpamPoolId, Ipv4NetmaskLength -> NetmaskLength, errors []; GetIpamPoolAllocations required IpamPoolId, output IpamPoolAllocations -> IpamPoolAllocationSet (xmlName ipamPoolAllocationSet), @paginated; shape IpamPoolAllocation members Cidr/IpamPoolAllocationId/Description/ResourceId/ResourceType/ResourceRegion/ResourceOwner/Tags only — allocationId, cidrBlock, resourceProvider and status are not in the on-disk model, so they are grounded in the recording.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Re-replay ec2-ipam-trajwire-vpc-from-top-pool-then-child#task#7 (must answer 400 InvalidParameterValue, not 200) and each step in trace_ids: #task#14 (cidrBlock must come from the pool named in Ipv4IpamPoolId), ec2-ipam-trajwire-teardown-order-mistakes#task#17 (ipamPoolAllocationSet must be empty after release) and ec2-ipam-trajwire-reference-path-regional-child-pool#task#11 (each allocation item must carry allocationId, cidrBlock, resourceProvider and status).
