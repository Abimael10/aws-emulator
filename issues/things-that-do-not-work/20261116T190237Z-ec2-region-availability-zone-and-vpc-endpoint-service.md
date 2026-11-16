Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-vpc-endpoint-services
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-vpc-endpoint-services ec2-default-vpc-and-subnet ec2-default-vpc-and-subnet-already-exist ec2-default-vpc-and-subnet-bootstrap
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeVpcEndpointServices|2xx->2xx|$.DescribeVpcEndpointServicesResponse.serviceDetailSet.item.privateDnsNameSet.item
Issue-Key: ec2|CreateDefaultSubnet|4xx->4xx|$.Response.Errors.Error.Message
Issue-Related: 20261030T063325Z-ec2-static-catalogs-diverge-from-aws-instance-type.md
Issue-Related: 20261108T143229Z-ec2-static-describe-catalogs-serve-synthesized-hardware-and.md
Issue-Related: 20261109T225653Z-ec2-vpc-endpoint-plane-missing-endpoint-policy-refusal.md
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# ec2 region availability zone and VPC endpoint service catalogs are incomplete

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeVpcEndpointServices

## Description
The emulator's us-west-2 zone table holds three Availability Zones where AWS has four, and its VPC endpoint-service catalog diverges in zone lists, DNS-name shape and service-id form. This bundle carries 2 net-new divergence clusters, neither owned by an open issue; they share one fault: the region/zone and endpoint-service reference data the read paths serve.

GENERAL FAULT
AWS: us-west-2 exposes us-west-2a/b/c/d with AZ ids usw2-az1..usw2-az4, and the mapping from zone name to zone id is per-account but stable within a response; endpoint-service details enumerate every zone the service is available in, list every private DNS name the service publishes as a list element, and identify the service with a serviceId of the form 'vpce-svc-' followed by 17 hex characters.
Emulator: only three zones exist, the zone-name/zone-id pairing differs from the recording, the fourth zone is missing from both zone sets, privateDnsNameSet is serialized as one struct instead of a two-element list, and serviceId is 'vpce-svc-' plus 8 hex characters.
Why: a client that spreads subnets or endpoints across all AZs of the region can never reach the fourth zone and gets an error that claims it does not exist; AZ-id-based placement maps to the wrong zone; a caller reading privateDnsNameSet as a list sees one entry (and loses the -fips name); id-format assertions on serviceId fail.
Fix: the service's region/availability-zone reference table (used by the AZ catalog, CreateDefaultSubnet validation and the endpoint-service catalog), the endpoint-service catalog rows, and the list serialization of privateDnsNameSet.

Cause 1 of 2: CreateDefaultSubnet with an Availability Zone that does not exist. AWS: 400 whose message enumerates the region's zones - fixed text 'Value (<supplied-az>) for parameter availabilityZone is invalid. Subnets can currently only be created in the following availability zones: ' followed by the region's zone names, which for us-west-2 is us-west-2a, us-west-2b, us-west-2c, us-west-2d. Emulator: same frame but only us-west-2a, us-west-2b, us-west-2c - evidence the zone table is short one entry. Traces: ec2-default-vpc-and-subnet#task#3 (cluster also covers ec2-default-vpc-and-subnet-already-exist#task#3 and ec2-default-vpc-and-subnet-bootstrap#task#4).

Cause 2 of 2: DescribeVpcEndpointServices serviceDetailSet. AWS: availabilityZoneSet lists all four us-west-2 zones and availabilityZoneIdSet the four matching AZ ids in the same per-zone order; privateDnsNameSet is a list with one element per published name (the service name and its -fips variant); serviceId is 'vpce-svc-' + 17 hex. Emulator: drops the fourth zone from availabilityZoneSet and availabilityZoneIdSet, orders the first two AZ ids the other way round from the recorded name/id pairing, serializes privateDnsNameSet as a single struct holding only the non-FIPS name, and emits a short 8-hex serviceId. Traces: ec2-catalog-describe-vpc-endpoint-services#task#1.

## Evidence
Cause 1 (ec2-default-vpc-and-subnet#task#3): expected status 400, actual 400; $.Response.Errors.Error.Message expected 'Value (bogus-az-does-not-exist) for parameter availabilityZone is invalid. Subnets can currently only be created in the following availability zones: us-west-2a, us-west-2b, us-west-2c, us-west-2d.', got the same sentence ending '... us-west-2a, us-west-2b, us-west-2c.'.
Cause 2 (ec2-catalog-describe-vpc-endpoint-services#task#1): expected status 200, actual 200; serviceDetailSet.item.availabilityZoneIdSet.item[0] expected 'usw2-az2', got 'usw2-az1'; item[1] expected 'usw2-az1', got 'usw2-az2'; availabilityZoneIdSet.item[3] expected 'usw2-az4', got None; availabilityZoneSet.item[3] expected 'us-west-2d', got None; privateDnsNameSet.item expected a two-element list [{'privateDnsName': '<service>.us-west-2.api.aws'}, {'privateDnsName': '<service>-fips.us-west-2.api.aws'}], got a single struct with only the first; serviceId expected the 'vpce-svc-' + 17-hex form, got 'vpce-svc-' + 8 hex.
(The zone-id pairing, service names and the recorded serviceId literal are this recording's values; the requirements are the four-zone region, the per-zone name/id correspondence, the list shape and the id form.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay ec2-catalog-describe-vpc-endpoint-services step task#1 and compare serviceDetailSet.item's availabilityZoneSet, availabilityZoneIdSet, privateDnsNameSet and serviceId with the recording.
3. Replay ec2-default-vpc-and-subnet step task#3 - CreateDefaultSubnet with a non-existent AZ - and read the enumerated zone list in the error message: three zones instead of four.
