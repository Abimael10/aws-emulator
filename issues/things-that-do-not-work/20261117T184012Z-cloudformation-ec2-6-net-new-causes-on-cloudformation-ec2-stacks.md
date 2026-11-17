Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2.deploy-ec2-instance-block-device-mapping-read-back-through-describe-volumes cloudformation__ec2.deploy-ec2-instance-from-launch-template-inherits-its-properties cloudformation__ec2.deploy-ec2-instance-launch-template-nonexistent-version-rejected cloudformation__ec2.deploy-ec2-launch-template-same-fixed-name-in-two-regions-gets-its-own-id cloudformation__ec2.deploy-ec2-interface-vpc-endpoint-dns-entries-match-getatt-and-delete cloudformation__ec2.deploy-subnet-ipv6-getatt-on-ipv4-subnet-rejected-and-rolled-back cloudformation__ec2.deploy-vpc-subnet-getatt-values-match-ec2-describe cloudformation__ec2.cfn-ec2-vpc-dhcp-options-association-swap-replaces-then-removal-reverts-to-default cloudformation__ec2.deploy-cfn-ec2-security-group-duplicate-inline-ingress-rule-still-blocks-a-later-authorize cloudformation__ec2.deploy-ec2-instance-source-dest-check-and-monitoring-round-trip-and-delete cloudformation__ec2.deploy-ec2-security-group-ingress-unknown-source-prefix-list-id-rejected-and-rolled-back
Issue-Key: cloudformation__ec2.deploy-ec2-instance-block-device-mapping-read-back-through-describe-volumes|_cheapest_in_an_az#3|AssertionError|raise AssertionError(
Issue-Key: cloudformation__ec2.deploy-ec2-instance-from-launch-template-inherits-its-properties|_cheapest_in_an_az#3|AssertionError|raise AssertionError(
Issue-Key: cloudformation__ec2.deploy-ec2-instance-launch-template-nonexistent-version-rejected|_cheapest_in_an_az#3|AssertionError|raise AssertionError(
Issue-Key: cloudformation__ec2.deploy-ec2-launch-template-same-fixed-name-in-two-regions-gets-its-own-id|_cheapest_in_region#3|AssertionError|raise AssertionError(
Issue-Key: cloudformation__ec2.deploy-ec2-interface-vpc-endpoint-dns-entries-match-getatt-and-delete|run#10|KeyError|outputs = {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Key: cloudformation__ec2.deploy-subnet-ipv6-getatt-on-ipv4-subnet-rejected-and-rolled-back|run#7|KeyError|outputs = [(o["OutputKey"], o["OutputValue"]) for o in control.get("Outputs", [])]
Issue-Key: cloudformation__ec2.deploy-vpc-subnet-getatt-values-match-ec2-describe|run#8|KeyError|out = {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Key: cloudformation__ec2.cfn-ec2-vpc-dhcp-options-association-swap-replaces-then-removal-reverts-to-default|run#18|AssertionError|assert first_physical == f"{opts1}|{vpc_id}", f"association physical id {first_physical!r}"
Issue-Key: cloudformation__ec2.deploy-cfn-ec2-security-group-duplicate-inline-ingress-rule-still-blocks-a-later-authorize|_expect#7|AssertionError|raise AssertionError(
Issue-Key: cloudformation__ec2.deploy-ec2-instance-source-dest-check-and-monitoring-round-trip-and-delete|run#8|AssertionError|assert stack["StackStatus"] == "CREATE_COMPLETE", (
Issue-Key: cloudformation__ec2.deploy-ec2-security-group-ingress-unknown-source-prefix-list-id-rejected-and-rolled-back|run#8|AssertionError|assert got == _REJECTED, f"CREATE_FAILED reason: expected {_REJECTED!r}, got {got!r}"
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# 6 net-new causes on CloudFormation EC2 stacks: DescribeInstanceTypeOfferings is empty, stack Outputs omit OutputValue, DHCP-options-association physical id, duplicate inline ingress, SourceDestCheck instance, and a legacy SDK failure suffix

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypeOfferings

## Description
Six distinct defects reached through CloudFormation stacks that declare AWS::EC2::* resources. Each cause has its own failing check ids.

Cause 1 of 6: DescribeInstanceTypeOfferings reports no offerings for the common burstable instance types.
AWS: recorded behavior only. DescribeInstanceTypeOfferings with LocationType=availability-zone and a `location` filter naming an available zone of the region lists the instance types offered there, which always includes t3.micro/t3.nano (and in most regions t2.micro/t2.nano); the same call with the default region location type lists them for the region.
Emulator: for every available zone the region reports, and for the region as a whole, none of t3.nano, t3.micro, t2.nano, t2.micro appears in InstanceTypeOfferings — the result set is empty of these types, so a client cannot pick any instance type to launch.
Why: every tool that selects an instance type by what the region/zone actually offers (the CDK, Terraform's data sources, capacity-planning scripts) concludes nothing is launchable and stops before it makes any call.
Fix: the ec2 DescribeInstanceTypeOfferings handler and the instance-type offering state it reads.
Traces: cloudformation__ec2.deploy-ec2-instance-block-device-mapping-read-back-through-describe-volumes, cloudformation__ec2.deploy-ec2-instance-from-launch-template-inherits-its-properties, cloudformation__ec2.deploy-ec2-instance-launch-template-nonexistent-version-rejected, cloudformation__ec2.deploy-ec2-launch-template-same-fixed-name-in-two-regions-gets-its-own-id

Cause 2 of 6: the Output rows DescribeStacks returns for a CREATE_COMPLETE EC2 stack carry OutputKey but no OutputValue.
AWS: Smithy: cloudformation#Output declares OutputKey, OutputValue, Description, ExportName; AWS populates OutputValue with the resolved Ref/GetAtt for every output of a stack that reached CREATE_COMPLETE.
Emulator: the Output structure is serialized without the OutputValue member, so reading `o["OutputValue"]` raises KeyError and no output of the stack is readable.
Why: every consumer of stack outputs gets a structurally invalid list instead of the value — GetAtt results such as a VPC endpoint's DnsEntries or a subnet's Ipv6CidrBlocks cannot be read out of the stack at all. The open issue 20261109T223435Z-cloudformation-ec2-6-net-new-causes-26-aws.md carries one other check with this symptom; it does not cover these three.
Fix: the CloudFormation DescribeStacks output serializer / the stack's output-resolution state.
Traces: cloudformation__ec2.deploy-ec2-interface-vpc-endpoint-dns-entries-match-getatt-and-delete, cloudformation__ec2.deploy-subnet-ipv6-getatt-on-ipv4-subnet-rejected-and-rolled-back, cloudformation__ec2.deploy-vpc-subnet-getatt-values-match-ec2-describe

Cause 3 of 6: the PhysicalResourceId of AWS::EC2::VPCDHCPOptionsAssociation is a generated name instead of the composite identifier.
AWS: recorded behavior only. The association has no id of its own, so CloudFormation's physical id is the pipe-joined pair `<DhcpOptionsId>|<VpcId>` — fixed text: the single '|' and the field order; variable parts: the two ids, from the resources the template associates.
Emulator: the physical id is a generated token of the form `<stack-name>-<LogicalId>-<suffix>`, which names neither the DHCP options set nor the VPC.
Why: a client cannot tell from the stack which DHCP options set is attached to which VPC, and replacement/removal of the association cannot be reasoned about from the stack's own records. The open issue 20261111T111216Z-cloudformation-ec2-8-net-new-causes-in-cloudformation-provisioned-ec2.md reports the same shape for AWS::EC2::VPCGatewayAttachment; it does not cover this resource type.
Fix: the AWS::EC2::VPCDHCPOptionsAssociation resource handler's physical-id construction.
Traces: cloudformation__ec2.cfn-ec2-vpc-dhcp-options-association-swap-replaces-then-removal-reverts-to-default

Cause 4 of 6: two identical inline SecurityGroupIngress entries on one AWS::EC2::SecurityGroup are refused, and the resulting rollback cannot delete the VPC.
AWS: recorded behavior only. A security group whose inline SecurityGroupIngress list repeats the same rule is created once and the stack reaches CREATE_COMPLETE; a later AuthorizeSecurityGroupIngress for that same rule is then refused as a duplicate.
Emulator: the group's create fails with `Resource handler returned message: "the specified rule "peer: <cidr>, <protocol>, from port: <from>, to port: <to>, ALLOW" already exists (...Error Code: InvalidPermission.Duplicate...)" (..., HandlerErrorCode: AlreadyExists)`, and the rollback then fails to delete the VPC with DependencyViolation, leaving the stack ROLLBACK_FAILED.
Why: a template AWS deploys is refused, and the failed deploy leaves an undeletable stack and a leaked VPC behind.
Fix: the AWS::EC2::SecurityGroup resource handler's inline-ingress application (de-duplicate the declared rule list) and the rollback ordering that must release the group before deleting its VPC.
Traces: cloudformation__ec2.deploy-cfn-ec2-security-group-duplicate-inline-ingress-rule-still-blocks-a-later-authorize

Cause 5 of 6: an AWS::EC2::Instance that declares SourceDestCheck and Monitoring cannot be created.
AWS: recorded behavior only. The instance is created with those properties applied and the stack reaches CREATE_COMPLETE; DescribeInstances then reports the declared SourceDestCheck and monitoring state.
Emulator: the stack settles at ROLLBACK_COMPLETE with `The following resource(s) failed to create: [Instance]. ` — the instance resource fails.
Why: a client cannot deploy a NAT-style instance or a monitored instance through CloudFormation at all.
Fix: the AWS::EC2::Instance resource handler's Create for the SourceDestCheck and Monitoring properties.
Traces: cloudformation__ec2.deploy-ec2-instance-source-dest-check-and-monitoring-round-trip-and-delete

Cause 6 of 6: the CREATE_FAILED reason for an AWS::EC2::SecurityGroupIngress naming an unknown prefix list uses the legacy SDK rendering and the wrong HandlerErrorCode.
AWS: recorded behavior only. The handler message is `The prefix list ID '<id>' does not exist (Service: Ec2, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)` and the HandlerErrorCode is `InvalidRequest`. Fixed text: the message and the comma-separated ` (Service: Ec2, Status Code: 400, Request ID: ` / ` (SDK Attempt Count: ` fragments; variable parts: the prefix list id from the template, the request id and the attempt count.
Emulator: `The prefix list ID '<id>' does not exist (Service: Ec2; Status Code: 400; Error Code: InvalidPrefixListID.NotFound; Request ID: <uuid>; Proxy: null)` with HandlerErrorCode `NotFound` — the semicolon-separated v1 rendering plus an `Error Code:`/`Proxy: null` field AWS does not emit, and a different handler error code.
Why: automation that classifies stack failures by HandlerErrorCode or parses the reason's SDK suffix mis-handles this failure class.
Fix: the AWS::EC2::SecurityGroupIngress resource handler's failure-reason serializer and its handler-error-code mapping.
Traces: cloudformation__ec2.deploy-ec2-security-group-ingress-unknown-source-prefix-list-id-rejected-and-rolled-back

## Evidence
Cause 1: AssertionError: "none of ('t3.nano', 't3.micro', 't2.nano', 't2.micro') is offered in any available AZ of ['us-west-2a', 'us-west-2b', 'us-west-2c']" (three checks) and "none of ('t3.nano', 't3.micro', 't2.nano', 't2.micro') is offered in region 'us-west-2'" (one check).
Cause 2: `{o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}` raises `KeyError: 'OutputValue'` on a CREATE_COMPLETE stack (three checks).
Cause 3: AssertionError: "association physical id 'sim2real-cfnvda-<suffix>-Association-<suffix>'"; expected `<DhcpOptionsId>|<VpcId>`.
Cause 4: stack ended 'ROLLBACK_FAILED' ('The following resource(s) failed to delete: [Vpc]. ') with Group CREATE_FAILED `..."the specified rule "peer: 192.0.2.0/24, TCP, from port: 8443, to port: 8443, ALLOW" already exists (Service: Ec2; Status Code: 400; Error Code: InvalidPermission.Duplicate; ...)" (..., HandlerErrorCode: AlreadyExists)` and Vpc DELETE_FAILED DependencyViolation; expected CREATE_COMPLETE.
Cause 5: stack ended 'ROLLBACK_COMPLETE' ('The following resource(s) failed to create: [Instance]. '), expected 'CREATE_COMPLETE'.
Cause 6: expected ("The prefix list ID 'pl-0123456789abcdef0' does not exist (Service: Ec2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)", 'InvalidRequest'); got ("The prefix list ID 'pl-0123456789abcdef0' does not exist (Service: Ec2; Status Code: 400; Error Code: InvalidPrefixListID.NotFound; Request ID: <id>; Proxy: null)", 'NotFound').

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. cloudformation__ec2.deploy-ec2-instance-block-device-mapping-read-back-through-describe-volumes
2. cloudformation__ec2.deploy-ec2-instance-from-launch-template-inherits-its-properties
3. cloudformation__ec2.deploy-ec2-instance-launch-template-nonexistent-version-rejected
4. cloudformation__ec2.deploy-ec2-launch-template-same-fixed-name-in-two-regions-gets-its-own-id
5. cloudformation__ec2.deploy-ec2-interface-vpc-endpoint-dns-entries-match-getatt-and-delete
6. cloudformation__ec2.deploy-subnet-ipv6-getatt-on-ipv4-subnet-rejected-and-rolled-back
7. cloudformation__ec2.deploy-vpc-subnet-getatt-values-match-ec2-describe
8. cloudformation__ec2.cfn-ec2-vpc-dhcp-options-association-swap-replaces-then-removal-reverts-to-default
9. cloudformation__ec2.deploy-cfn-ec2-security-group-duplicate-inline-ingress-rule-still-blocks-a-later-authorize
10. cloudformation__ec2.deploy-ec2-instance-source-dest-check-and-monitoring-round-trip-and-delete
11. cloudformation__ec2.deploy-ec2-security-group-ingress-unknown-source-prefix-list-id-rejected-and-rolled-back
