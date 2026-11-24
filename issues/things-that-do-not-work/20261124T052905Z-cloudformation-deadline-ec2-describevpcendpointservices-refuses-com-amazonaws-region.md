Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__deadline__ec2.deploy-deadline-license-endpoint-and-metered-product-round-trip-and-delete cloudformation__deadline__ec2.deploy-deadline-metered-product-unsupported-product-id-rejected-and-rolled-back
Issue-Key: ec2|DescribeVpcEndpointServices|InvalidServiceName
Issue-Commit: ee29b9ed80499ffda1e629bc10e5c9c45fea7a64

# cloudformation__deadline__ec2: DescribeVpcEndpointServices refuses com.amazonaws.<region>.deadline.management with InvalidServiceName — the endpoint-service catalog still has no Deadline Cloud entry

> _Note: this issue was identified against the code base at commit `ee29b9ed80499ffda1e629bc10e5c9c45fea7a64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeVpcEndpointServices

## Description
REOPENS 20261109T114319Z-ec2-iam-imagebuilder-describevpcendpointservices-refuses-com-amazonaws-region.md: its fix left the call refused.

Cause 1 of 1: EC2's VPC endpoint-service catalog resolves the Image Builder name that issue was grounded in, but still does not contain the Deadline Cloud license/management interface endpoint service, so DescribeVpcEndpointServices refuses every request naming it and the two Deadline Cloud deploy scenarios stop on their first call. Failing checks: cloudformation__deadline__ec2.deploy-deadline-license-endpoint-and-metered-product-round-trip-and-delete, cloudformation__deadline__ec2.deploy-deadline-metered-product-unsupported-product-id-rejected-and-rolled-back.

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2) models DescribeVpcEndpointServices(DryRun, ServiceNames: list<String> (xmlName ServiceName), Filters, ServiceRegions, MaxResults, NextToken) -> DescribeVpcEndpointServicesResult{ServiceNames (serviceNameSet), ServiceDetails (serviceDetailSet), NextToken} and declares NO modeled errors, so which endpoint services exist is AWS's contract and not a modelled refusal. AWS publishes an interface endpoint service for Deadline Cloud named com.amazonaws.<region>.deadline.management in the regions where Deadline Cloud is offered, so a request whose ServiceNames lists exactly that name answers HTTP 200 with one ServiceDetails entry whose ServiceName echoes the requested name and whose AvailabilityZones is a non-empty list of the zones that offer it. AWS's refusal for this operation — HTTP 400, code InvalidServiceName, message "The Vpc Endpoint Service '<service-name>' does not exist", where the fixed text is everything around the quoted name and the quoted name is copied verbatim from the request — applies only to a name that no endpoint service has.

Emulator: the same request is refused in full with HTTP 400 InvalidServiceName and the message "The Vpc Endpoint Service '<service-name>' does not exist" (in this run the quoted name was the us-west-2 Deadline management name; the region and name are request-derived and are not part of the requirement). No ServiceDetails is returned at all, so the caller cannot learn which availability zone offers the service.

Why: a client following AWS's documented PrivateLink pattern for Deadline Cloud cannot enumerate the endpoint service it must create an endpoint for, and reads 'does not exist' as 'this region does not offer Deadline Cloud over PrivateLink', which is wrong. Because the refusal is whole-request, a request that mixes this name with valid ones loses the valid ones too. Everything downstream (the VPC/subnet network, the license endpoint, the metered product, the stack round trip and the unsupported-product-id rollback) is unreachable.

Fix: the EC2 VPC endpoint-service catalog/state behind the DescribeVpcEndpointServices read path, and the CreateVpcEndpoint ServiceName resolution that consumes it, so com.amazonaws.<region>.deadline.management exists as an Interface endpoint service with its availability zones. The previous fix extended the catalog with one entry rather than making the catalog cover AWS's published services. Related open issues that do NOT carry these checks: 20261123T135115Z-ec2-describevpcendpointservices-serves-a-synthesized-service-catalog-not.md (field fidelity and missing entries on the wire corpus) and 20261027T074309Z-logs-syslog-configurations-are-ungradeable-ec2-describevpcendpointservices-omits.md (the CloudWatch Logs syslog-ingestion service missing from the same catalog).

Traces: cloudformation__deadline__ec2.deploy-deadline-license-endpoint-and-metered-product-round-trip-and-delete, cloudformation__deadline__ec2.deploy-deadline-metered-product-unsupported-product-id-rejected-and-rolled-back.

## Evidence
Both checks call, from their _usable_az() helper:
  ec2.describe_vpc_endpoint_services(ServiceNames=[f"com.amazonaws.{region}.deadline.management"])["ServiceDetails"]
Expected (AWS): HTTP 200, ServiceDetails with one entry; entry.ServiceName == the requested name; entry.AvailabilityZones a non-empty list, which the check intersects with DescribeAvailabilityZones(state=available) to pick a zone.
Actual (emulator): botocore.exceptions.ClientError: An error occurred (InvalidServiceName) when calling the DescribeVpcEndpointServices operation: The Vpc Endpoint Service 'com.amazonaws.us-west-2.deadline.management' does not exist — HTTP 400 and no ServiceDetails.
Message anatomy: fixed text "The Vpc Endpoint Service '" + <ServiceNames entry from the request> + "' does not exist"; error code InvalidServiceName. The divergence is not the wording but that this published service name is treated as nonexistent.

## Steps to Reproduce
1. Boot the emulator and replay the behavioral suite.
2. Run cloudformation__deadline__ec2.deploy-deadline-license-endpoint-and-metered-product-round-trip-and-delete (and the -unsupported-product-id- sibling). Both raise at the first DescribeVpcEndpointServices call.
3. Equivalently: ec2.describe_vpc_endpoint_services(ServiceNames=['com.amazonaws.<region>.deadline.management']) must answer HTTP 200 with one ServiceDetails entry carrying that ServiceName and a non-empty AvailabilityZones list, and ec2.create_vpc_endpoint(VpcEndpointType='Interface', ServiceName='com.amazonaws.<region>.deadline.management', ...) must be accepted; today the describe is refused HTTP 400 InvalidServiceName.
