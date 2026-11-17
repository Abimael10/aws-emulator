Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-elastic-ip-tag-change-and-remove-last-then-not-found
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-elastic-ip-tag-change-and-remove-last-then-not-found ec2-launch-template-tag-change-and-remove-last-then-not-found ec2-security-group-tag-change-value-scoped-delete-and-remove-last ec2-subnet-tag-change-and-remove-last-then-not-found ec2-vpc-tag-change-and-remove-last-then-not-found
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeAddresses|2xx->2xx|$.DescribeAddressesResponse.addressesSet.item.tagSet
Issue-Key: ec2|DescribeLaunchTemplates|2xx->2xx|$.DescribeLaunchTemplatesResponse.launchTemplates.item.tagSet
Issue-Key: ec2|DescribeSecurityGroups|2xx->2xx|$.DescribeSecurityGroupsResponse.securityGroupInfo.item.tagSet
Issue-Key: ec2|DescribeSubnets|2xx->2xx|$.DescribeSubnetsResponse.subnetSet.item.tagSet
Issue-Key: ec2|DescribeVpcs|2xx->2xx|$.DescribeVpcsResponse.vpcSet.item.tagSet
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# ec2 describe replies emit an empty tagSet element after the last tag is deleted

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAddresses

## Description
5 causes, one shared fault: after a resource's last tag is removed, the EC2 ec2Query response serializer still emits the `tagSet` wrapper element (as an empty element) where AWS emits no `tagSet` member at all. A caller that tests for the presence of `tagSet` therefore sees a tagged resource that has no tags. Each cause is a separate describe operation whose serializer must stop emitting the empty list; a fix to the shared tag-list serialization closes all five, but each must be re-replayed.

Cause 1 of 5: DescribeAddresses
AWS: Smithy: Address.Tags -> xmlName `tagSet` / ec2QueryName `TagSet`, target TagList. Under ec2Query an empty list member is not serialized, so an address with no tags is returned with no `tagSet` element. The recorded response for this step is HTTP 200 with `addressesSet.item` carrying no `tagSet` member.
Emulator: HTTP 200, but `$.DescribeAddressesResponse.addressesSet.item.tagSet` is present and empty ('').
Why: a client that branches on `tagSet` being present (the common "does this address still carry tags?" test, and every SDK that maps the element to an empty-vs-absent list) concludes the address still has a tag collection after DeleteTags removed the last tag.
Fix: the DescribeAddresses response serializer / the shared ec2Query TagList serialization in the EC2 crate's response-building path - omit the member when the tag list is empty.
Traces: ec2-elastic-ip-tag-change-and-remove-last-then-not-found#task#8

Cause 2 of 5: DescribeLaunchTemplates
AWS: Smithy: LaunchTemplate.Tags -> xmlName `tagSet`, target TagList; absent when the template has no tags. Recorded response: HTTP 200, `launchTemplates.item` with no `tagSet`.
Emulator: HTTP 200 with `$.DescribeLaunchTemplatesResponse.launchTemplates.item.tagSet` present and empty ('').
Why: same - an untagged launch template reads back as one carrying an (empty) tag collection.
Fix: DescribeLaunchTemplates response serializer / shared TagList serialization.
Traces: ec2-launch-template-tag-change-and-remove-last-then-not-found#task#8

Cause 3 of 5: DescribeSecurityGroups
AWS: Smithy: SecurityGroup.Tags -> xmlName `tagSet`, target TagList; absent when empty. Recorded response: HTTP 200, `securityGroupInfo.item` with no `tagSet`.
Emulator: HTTP 200 with `$.DescribeSecurityGroupsResponse.securityGroupInfo.item.tagSet` present and empty (''), and it stays present across the step's whole poll window (30 attempts), so this is a steady-state serialization difference, not a propagation delay.
Why: a value-scoped DeleteTags that removes the last tag cannot be confirmed by reading the group back.
Fix: DescribeSecurityGroups response serializer / shared TagList serialization.
Traces: ec2-security-group-tag-change-value-scoped-delete-and-remove-last#task#11

Cause 4 of 5: DescribeSubnets
AWS: Smithy: Subnet.Tags -> xmlName `tagSet`, target TagList; absent when empty. Recorded response: HTTP 200, `subnetSet.item` with no `tagSet`.
Emulator: HTTP 200 with `$.DescribeSubnetsResponse.subnetSet.item.tagSet` present and empty (''), still present after 30 poll attempts.
Why: same.
Fix: DescribeSubnets response serializer / shared TagList serialization.
Traces: ec2-subnet-tag-change-and-remove-last-then-not-found#task#9

Cause 5 of 5: DescribeVpcs
AWS: Smithy: Vpc.Tags -> xmlName `tagSet`, target TagList; absent when empty. Recorded response: HTTP 200, `vpcSet.item` with no `tagSet`.
Emulator: HTTP 200 with `$.DescribeVpcsResponse.vpcSet.item.tagSet` present and empty ('').
Why: same.
Fix: DescribeVpcs response serializer / shared TagList serialization.
Traces: ec2-vpc-tag-change-and-remove-last-then-not-found#task#8

## Evidence
Replay of trace set simparity-wire-aws-v1 (service ec2), all five clusters verdict=mismatch, expected_status 200 / actual_status 200:
- ec2-elastic-ip-tag-change-and-remove-last-then-not-found#task#8 DescribeAddresses: $.DescribeAddressesResponse.addressesSet.item.tagSet: unexpected_present (expected None, got '')
- ec2-launch-template-tag-change-and-remove-last-then-not-found#task#8 DescribeLaunchTemplates: $.DescribeLaunchTemplatesResponse.launchTemplates.item.tagSet: unexpected_present (expected None, got '')
- ec2-security-group-tag-change-value-scoped-delete-and-remove-last#task#11 DescribeSecurityGroups: $.DescribeSecurityGroupsResponse.securityGroupInfo.item.tagSet: unexpected_present (expected None, got ''); poll: still present after 30 attempt(s)
- ec2-subnet-tag-change-and-remove-last-then-not-found#task#9 DescribeSubnets: $.DescribeSubnetsResponse.subnetSet.item.tagSet: unexpected_present (expected None, got ''); poll: still present after 30 attempt(s)
- ec2-vpc-tag-change-and-remove-last-then-not-found#task#8 DescribeVpcs: $.DescribeVpcsResponse.vpcSet.item.tagSet: unexpected_present (expected None, got '')
Expected in every case: the item element carries no tagSet member.

## Steps to Reproduce
1. Create the resource (Elastic IP / launch template / security group / subnet / VPC) with one tag, e.g. Name.
2. DeleteTags that last tag.
3. Describe the resource (DescribeAddresses / DescribeLaunchTemplates / DescribeSecurityGroups / DescribeSubnets / DescribeVpcs) and inspect the raw ec2Query XML.
4. The emulator returns `<tagSet/>` on the item; AWS returns no tagSet element at all.
Or replay the fixture steps listed under Traces.
