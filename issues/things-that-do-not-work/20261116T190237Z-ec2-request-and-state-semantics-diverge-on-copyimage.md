Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-ami-copy-roundtrip
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-ami-copy-roundtrip ec2-copy-ami-cross-region-workflow ec2-ipam-trajwire-read-side-mistakes ec2-pst-traj-route-filters-match-per-table-not-per-route
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|CopyImage|2xx->4xx|$.CopyImageResponse
Issue-Key: ec2|CopyImage|2xx->4xx|status
Issue-Key: ec2|GetIpamPoolAllocations|4xx->2xx|$
Issue-Key: ec2|GetIpamPoolAllocations|4xx->2xx|$.Response
Issue-Key: ec2|GetIpamPoolAllocations|4xx->2xx|status
Issue-Key: ec2|DescribeRouteTables|2xx->2xx|$.DescribeRouteTablesResponse.routeTableSet.item[routeTableId="rtb-000000000000000aa"].routeSet.item[*].state
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# ec2 request and state semantics diverge on CopyImage, filters and route state

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CopyImage

## Description
Three EC2 paths take the opposite decision from AWS: a valid CopyImage is rejected, an unsupported filter name is accepted, and a route whose target is gone is still reported as active. This bundle carries 3 net-new divergence clusters, none owned by an open issue.

GENERAL FAULT
AWS: CopyImage of an AMI that exists in the source region succeeds with 200 and a new imageId; an unknown Filter name on a describe/get call is rejected with 400 InvalidFilter; and a route whose target (NAT gateway, network interface, gateway) no longer exists is reported with state 'blackhole', not 'active'.
Emulator: CopyImage answers 400 and no CopyImageResponse at all; GetIpamPoolAllocations answers 200 with an empty set for a filter name it does not implement; DescribeRouteTables leaves the stale route 'active'.
Why: a client that copies an AMI cannot complete the workflow at all (the whole fixture's downstream steps are unreachable); a client that misspells or uses an unsupported filter silently gets an empty result instead of being told the filter is invalid, which looks like 'no resources' and hides a bug; and a client that checks route health to decide whether traffic is blackholed concludes the route is fine.
Fix: CopyImage's handler/validation (it must accept an existing source image in the source region and return the new image id), the filter-name validation shared by the IPAM get paths, and the route-state derivation in the route-table state/serializer.

Cause 1 of 3: CopyImage for an AMI that exists. AWS: 200 with $.CopyImageResponse carrying requestId and imageId. Emulator: 400 with no CopyImageResponse body. Smithy: CopyImage requires Name, SourceImageId and SourceRegion and returns CopyImageResult.ImageId. Traces: ec2-ami-copy-roundtrip#task#2 (cluster also covers ec2-copy-ami-cross-region-workflow#task#1).

Cause 2 of 3: GetIpamPoolAllocations with Filter name 'resource-type'. AWS: 400 with Code 'InvalidFilter' and Message "The filter '<name>' is invalid" - fixed text with the filter name from the request. Smithy: GetIpamPoolAllocations.Filters is a FilterList, and the service validates filter names. Emulator: 200 with an empty ipamPoolAllocationSet, so the fixture's poll for the error code never sees one. Traces: ec2-ipam-trajwire-read-side-mistakes#task#10.

Cause 3 of 3: DescribeRouteTables for a table holding a route whose target has been deleted. AWS: that routeSet item's state is 'blackhole'. Emulator: 'active'. Traces: ec2-pst-traj-route-filters-match-per-table-not-per-route#task#11 (cluster also covers task#12 and task#13 of the same fixture).

## Evidence
Cause 1 (ec2-ami-copy-roundtrip#task#2): expected status 200, actual status 400; $.CopyImageResponse expected {'requestId': <shaped>, 'imageId': <shaped>}, got None.
Cause 2 (ec2-ipam-trajwire-read-side-mistakes#task#10): expected status 400, actual status 200; expected $.Response.Errors.Error = {'Code':'InvalidFilter','Message':"The filter 'resource-type' is invalid"}, got {'GetIpamPoolAllocationsResponse': {'ipamPoolAllocationSet': '', 'requestId': ...}}; the step's poll gave up after 16 attempts with error code None.
Cause 3 (ec2-pst-traj-route-filters-match-per-table-not-per-route#task#11): expected status 200, actual status 200; $.DescribeRouteTablesResponse.routeTableSet.item[...].routeSet.item[2].state expected 'blackhole', got 'active'.
(The route-table and filter literals are this recording's values; the requirements are the accept/reject decision and the derived route state.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay ec2-ami-copy-roundtrip step task#2 - CopyImage of the AMI created earlier in the fixture: the emulator answers 400 instead of 200 with an imageId.
3. Replay ec2-ipam-trajwire-read-side-mistakes step task#10 - GetIpamPoolAllocations with Filter.Name 'resource-type': the emulator answers 200 with an empty set instead of 400 InvalidFilter.
4. Replay ec2-pst-traj-route-filters-match-per-table-not-per-route step task#11 and read the route whose target was deleted: state 'active' instead of 'blackhole'.
