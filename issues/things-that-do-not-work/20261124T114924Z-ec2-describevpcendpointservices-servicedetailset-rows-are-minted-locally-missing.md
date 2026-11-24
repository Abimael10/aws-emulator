Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-vpc-endpoint-services
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-vpc-endpoint-services
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeVpcEndpointServices|2xx->2xx|$.DescribeVpcEndpointServicesResponse.serviceDetailSet.item.privateDnsNameSet.item.privateDnsName
Issue-Key: ec2|DescribeVpcEndpointServices|2xx->2xx|$.DescribeVpcEndpointServicesResponse.serviceDetailSet.item.payerResponsibility
Issue-Related: 20261030T063325Z-ec2-static-catalogs-diverge-from-aws-instance-type.md
Issue-Related: 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md
Issue-Related: 20261116T190237Z-ec2-region-availability-zone-and-vpc-endpoint-service.md
Issue-Related: 20261123T135115Z-ec2-describevpcendpointservices-serves-a-synthesized-service-catalog-not.md
Issue-Commit: a5b444fd2887334b1205747ac6df31fc24ea8f9f

# ec2 DescribeVpcEndpointServices serviceDetailSet rows are minted locally: missing services, wrong serviceId, AZ-id order, privateDnsNameSet and payerResponsibility (4 causes)

> _Note: this issue was identified against the code base at commit `a5b444fd2887334b1205747ac6df31fc24ea8f9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeVpcEndpointServices

## Description
DescribeVpcEndpointServices answers HTTP 200, but the serviceDetailSet it serves is synthesized per request rather than served from the AWS endpoint-service catalog. Four distinct failures show up:

AWS: for a named/filtered query EC2 returns one serviceDetailSet entry per matching endpoint service, each carrying the catalog's own values - a stable serviceId, the Availability-Zone id list for that service, the full privateDnsNameSet list (one PrivateDnsDetails entry per private DNS name, including the -fips and .api.aws variants), payerResponsibility, and supportedIpAddressTypeSet limited to the address families the service actually offers - plus the matching serviceNameSet entries. Smithy (com-amazonaws-ec2-amazonec2.json): ServiceDetail.ServiceId (string), .AvailabilityZoneIds -> ValueStringList (xmlName availabilityZoneIdSet), .PrivateDnsNames -> PrivateDnsDetailsSet (xmlName privateDnsNameSet, a LIST), .PayerResponsibility -> PayerResponsibility enum (xmlName payerResponsibility), .SupportedIpAddressTypes -> SupportedIpAddressTypes with @length min 0 max 2; DescribeVpcEndpointServices declares no errors. The concrete per-service content is recorded behavior only (the fixture's recorded 200 bodies).
Emulator: mints serviceId values of its own, emits the AZ-id list in a different order, collapses privateDnsNameSet to a single element (so the modeled list degenerates to a struct on the wire), omits payerResponsibility, reports both ipv4 and ipv6 for a service AWS reports as ipv4-only, and for whole named queries returns none of the services AWS returns (serviceDetailSet entries and serviceNameSet entries missing, with an unrelated entry present instead).
Why: a caller resolves an interface endpoint from this read. A wrong or unstable serviceId makes CreateVpcEndpoint/tag-and-lookup flows refer to a service that does not exist; a collapsed privateDnsNameSet hides the FIPS and api.aws private names so private-DNS wiring and client decoding of a list both break; a missing payerResponsibility and an over-broad supportedIpAddressTypeSet make callers choose an address family or billing path the real service rejects; and an absent service makes a legitimate endpoint impossible to create at all.
Fix: the VPC endpoint-service catalog DATA and the query/filter projection behind the DescribeVpcEndpointServices handler (services/service-com-amazonaws-ec2-amazonec2 endpoint-service catalog), plus the response serializer for privateDnsNameSet, which must emit a list even with one member.

The open issues 20261123T135115Z-ec2-describevpcendpointservices-serves-a-synthesized-service-catalog-not.md, 20261116T190237Z-ec2-region-availability-zone-and-vpc-endpoint-service.md and 20261124T052905Z-cloudformation-deadline-ec2-describevpcendpointservices-refuses-com-amazonaws-region.md describe related faults but the replay reports the four clusters below with NO owning issue, so each needs its own re-replay.

Cause 1 of 4 (fixture ec2-catalog-describe-vpc-endpoint-services, step task#1 - the cassandra-streams service): serviceId differs from the catalog's, availabilityZoneIdSet item[0]/item[1] are swapped relative to AWS's order, and privateDnsNameSet is emitted as a single PrivateDnsDetails instead of the two-entry list AWS returns (the -fips name is dropped).
Cause 2 of 4 (same fixture, step task#3 - the emr-service-cell01 service): payerResponsibility is omitted entirely (AWS reports 'ServiceOwner'), supportedIpAddressTypeSet carries ['ipv4','ipv6'] where AWS reports only 'ipv4', serviceId differs, availabilityZoneIdSet order differs.
Cause 3 of 4 (same fixture, step task#22 - healthlake / healthlake-datatransformation): the whole serviceDetailSet entries AWS returns are missing (item[0], item[1], item[2]) and serviceNameSet item[1]/item[2] are missing, i.e. the query matches none of the services the catalog publishes.
Cause 4 of 4 (same fixture, step task#36 - xray / xray-fips): the serviceDetailSet entries AWS returns are missing and an unexpected extra entry is present instead, with serviceNameSet item[1] missing.

The host halves of these rows (baseEndpointDnsNameSet/privateDnsName resolving to a .localhost authority rather than .amazonaws.com) are the emulator's own serving host and are NOT part of this report; the membership, id, ordering, list-shape and member-presence divergences above are.

## Evidence
Replay verdict 'mismatch', expected status 200 / actual status 200 on all four clusters.

Cause 1 (ec2-catalog-describe-vpc-endpoint-services#task#1): $.DescribeVpcEndpointServicesResponse.serviceDetailSet.item.serviceId expected 'vpce-svc-0ee30985e06084f10', got 'vpce-svc-97e6b3275ede2efb5' (an emulator-minted id); .availabilityZoneIdSet.item[0] expected 'usw2-az2', got 'usw2-az1'; .availabilityZoneIdSet.item[1] expected 'usw2-az1', got 'usw2-az2'; .privateDnsNameSet.item type_mismatch expected [{'privateDnsName': 'cassandra-streams.us-west-2.api.aws'}, {'privateDnsName': 'cassandra-streams-fips.us-west-2.api.aws'}], got {'privateDnsName': 'cassandra-streams.us-west-2.api.aws'}.
Cause 2 (…#task#3): .payerResponsibility expected 'ServiceOwner', got None; .supportedIpAddressTypeSet.item type_mismatch expected 'ipv4', got ['ipv4','ipv6']; .serviceId expected 'vpce-svc-0109061b704c5f476', got 'vpce-svc-2fd9d201a4db8d072'; .availabilityZoneIdSet.item[0] expected 'usw2-az2', got 'usw2-az1'; item[1] expected 'usw2-az1', got 'usw2-az2'.
Cause 3 (…#task#22): .serviceDetailSet.item[0] missing (AWS returns the com.amazonaws.us-west-2.healthlake entry, serviceId vpce-svc-07a9d3ba96a216e2d, 4 AZ ids, 4 privateDnsNameSet entries), .serviceDetailSet.item[1] missing (com.amazonaws.us-west-2.healthlake-datatransformation, serviceId vpce-svc-0bcc1cbb618f40c7d), .serviceDetailSet.item[2] missing, .serviceNameSet.item[1] and .item[2] missing.
Cause 4 (…#task#36): .serviceDetailSet.item[0] missing (com.amazonaws.us-west-2.xray) and item[1] missing (com.amazonaws.us-west-2.xray-fips, supportedIpAddressTypeSet 'ipv4' only), plus a serviceDetailSet.item[0] reported 'extra', and .serviceNameSet.item[1] missing.

Smithy: ServiceDetail.PrivateDnsNames -> PrivateDnsDetailsSet (list, xmlName privateDnsNameSet); ServiceDetail.PayerResponsibility -> PayerResponsibility enum; ServiceDetail.SupportedIpAddressTypes @length 0..2; ServiceDetail.AvailabilityZoneIds -> ValueStringList; DescribeVpcEndpointServices errors: [].

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture ec2-catalog-describe-vpc-endpoint-services step task#1 and compare serviceDetailSet.item.serviceId, .availabilityZoneIdSet and .privateDnsNameSet with the recorded 200 body.
3. Cause 2: same fixture, step task#3 - check that payerResponsibility is present and that supportedIpAddressTypeSet is ipv4-only.
4. Cause 3: same fixture, step task#22 - the response must contain the healthlake and healthlake-datatransformation serviceDetailSet entries and their serviceNameSet names.
5. Cause 4: same fixture, step task#36 - the response must contain the xray and xray-fips entries and no extra entry.
All four steps answer HTTP 200 today; the divergence is entirely in serviceDetailSet/serviceNameSet membership and member values.
