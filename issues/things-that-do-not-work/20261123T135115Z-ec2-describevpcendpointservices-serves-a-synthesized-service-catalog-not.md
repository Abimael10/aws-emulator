Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-vpc-endpoint-services
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-vpc-endpoint-services
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeVpcEndpointServices|2xx->2xx|$.DescribeVpcEndpointServicesResponse.serviceDetailSet.item.availabilityZoneIdSet.item[*]
Issue-Key: ec2|DescribeVpcEndpointServices|2xx->2xx|$.DescribeVpcEndpointServicesResponse.serviceDetailSet.item.privateDnsName
Issue-Related: 20261030T063325Z-ec2-static-catalogs-diverge-from-aws-instance-type.md
Issue-Related: 20261109T225653Z-ec2-vpc-endpoint-plane-missing-endpoint-policy-refusal.md
Issue-Related: 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md
Issue-Related: 20261116T190237Z-ec2-region-availability-zone-and-vpc-endpoint-service.md
Issue-Commit: 94f7b27553b727586aac9de0add3c17de28dbaa9

# ec2 DescribeVpcEndpointServices serves a synthesized service catalog, not AWS's

> _Note: this issue was identified against the code base at commit `94f7b27553b727586aac9de0add3c17de28dbaa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeVpcEndpointServices

## Description
Four net-new causes, all one fault: DescribeVpcEndpointServices answers from an emulator-derived table of AWS-owned endpoint services rather than the published catalog, so entries are missing, per-service facts are generated from the service name, and list-shaped members collapse. All four causes are "recorded behavior only" — the Smithy model describes the shape of ServiceDetail but not which AWS services exist or their attributes, so the recordings are the authority. Host names in the recordings (…vpce.amazonaws.com vs …vpce.localhost) are environment-dependent and are NOT part of these causes.

Cause 1 of 4 — per-service facts are derived rather than catalogued (fixture ec2-catalog-describe-vpc-endpoint-services, step task#1).
AWS: recorded behavior only — for an AWS-owned service, availabilityZoneIdSet lists the zone ids in the order AWS publishes them for that service (usw2-az2 before usw2-az1 in the recording), serviceId is the service's published vpce-svc id, and privateDnsNameSet is a LIST of every private DNS name the service offers (the base name and its -fips sibling).
Emulator: emits the zone ids in plain ascending order (usw2-az1, usw2-az2), mints its own serviceId, and emits privateDnsNameSet as a single element instead of the multi-element list.
Why: a client that picks a zone by position, or that stores the serviceId to create an endpoint and later match it, reads values that do not correspond to the service; with only one private DNS name the FIPS name is invisible.
Fix: the DescribeVpcEndpointServices read path and the endpoint-service catalog data it serves.

Cause 2 of 4 — supportedIpAddressTypeSet is widened for services AWS lists as IPv4-only (fixture ec2-catalog-describe-vpc-endpoint-services, step task#3).
AWS: recorded behavior only — supportedIpAddressTypeSet carries exactly the address types that service supports; in the recording the service supports 'ipv4' only (one element).
Emulator: returns ['ipv4','ipv6'] for the same service, and again mints its own serviceId and reorders availabilityZoneIdSet.
Why: a caller that creates an interface endpoint with IpAddressType=ipv6 on the strength of this list is told the service supports dual-stack when it does not.
Fix: the endpoint-service catalog data plus the DescribeVpcEndpointServices serializer (single-element sets must still serialize as a set).

Cause 3 of 4 — services absent from the catalog entirely (fixture ec2-catalog-describe-vpc-endpoint-services, step task#22).
AWS: recorded behavior only — a ServiceName filter for the healthlake services returns a serviceDetailSet with one entry per published service (com.amazonaws.<region>.healthlake, …healthlake-datatransformation, …), each with its own serviceId, DNS name set, zone set and vpcEndpointPolicySupported flag, and a serviceNameSet listing each name.
Emulator: returns no entry for those services (serviceDetailSet items missing) and a short serviceNameSet.
Why: a client enumerating available endpoint services cannot discover them, so creating a VPC endpoint for that service is unreachable in the emulator.
Fix: the endpoint-service catalog data behind DescribeVpcEndpointServices.

Cause 4 of 4 — catalog entries present but not the ones requested (fixture ec2-catalog-describe-vpc-endpoint-services, step task#36).
AWS: recorded behavior only — the xray filter returns both com.amazonaws.<region>.xray and com.amazonaws.<region>.xray-fips, the second being IPv4-only with a single private DNS name.
Emulator: those entries are missing and an unrelated entry is returned in their place (reported as item[0] missing AND item[0] extra), so the result set is a different set of services from the one filtered for.
Why: the response does not answer the filter the client sent; discovery by ServiceName returns the wrong service.
Fix: the DescribeVpcEndpointServices filter/ServiceName matching path and the catalog it reads.

Related open issue that does NOT carry these replay keys: 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md.

## Evidence
Cause 1 (task#1) — availabilityZoneIdSet.item[0] expected 'usw2-az2', got 'usw2-az1'; item[1] expected 'usw2-az1', got 'usw2-az2'; serviceId expected 'vpce-svc-0ee30985e06084f10', got 'vpce-svc-97e6b3275ede2efb5'; privateDnsNameSet.item expected a 2-element list ([cassandra-streams.<region>.api.aws, cassandra-streams-fips.<region>.api.aws]), got a single object.
Cause 2 (task#3) — supportedIpAddressTypeSet.item expected 'ipv4', got ['ipv4','ipv6']; serviceId and availabilityZoneIdSet diverge as in cause 1.
Cause 3 (task#22) — serviceDetailSet.item[0]/[1]/[2] missing (expected the healthlake and healthlake-datatransformation entries with their own serviceId/privateDnsNameSet/availabilityZone sets), serviceNameSet.item[1]/[2] missing.
Cause 4 (task#36) — serviceDetailSet.item[0]/[1] missing (expected the xray and xray-fips entries) and item[0] simultaneously reported extra; serviceNameSet.item[1] missing. All four steps: expected status 200, actual 200.

## Steps to Reproduce
Replay trace set simparity-wire-aws-v1 and inspect ec2-catalog-describe-vpc-endpoint-services steps task#1, task#3, task#22 and task#36. Equivalently: call DescribeVpcEndpointServices with a ServiceName filter for cassandra-streams, for an IPv4-only service, for healthlake, and for xray, and compare serviceId, availabilityZoneIdSet order, privateDnsNameSet cardinality, supportedIpAddressTypeSet and the set of serviceDetailSet entries against the published catalog.
