Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__resourcegroupstaggingapi.internet-gateway-tags-visible-through-get-resources-both-directions ec2__resourcegroupstaggingapi.vpc-tags-visible-through-get-resources-both-directions
Issue-Key: ec2__resourcegroupstaggingapi.internet-gateway-tags-visible-through-get-resources-both-directions|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Key: ec2__resourcegroupstaggingapi.vpc-tags-visible-through-get-resources-both-directions|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Commit: a14965d6060559849ea419b74faa25e7836c4558

# ec2__resourcegroupstaggingapi: GetResources never discovers an EC2 internet gateway or VPC tagged through EC2's own CreateTags

> _Note: this issue was identified against the code base at commit `a14965d6060559849ea419b74faa25e7836c4558`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResources

## Description
One net-new root cause, grounded in two failing checks. The open issue 20261117T150031Z-ec2-resourcegroupstaggingapi-3-causes-getresources-never-discovers-ec2.md describes a similar GetResources gap but does not carry either of these replay keys, so they are filed here and that issue is named for the fixer's benefit.

Cause 1 of 1: the tagging index never indexes EC2 resources tagged by EC2.
AWS: a tag written by EC2's own CreateTags on an internet gateway or a VPC is indexed by the Resource Groups Tagging API; GetResources(TagFilters=[{Key: k, Values: [v]}]) returns exactly one ResourceTagMappingList entry whose ResourceARN is that resource's ARN (arn:aws:ec2:<region>:<account>:internet-gateway/<igw-id>, arn:aws:ec2:<region>:<account>:vpc/<vpc-id>). The index is eventually consistent, which is why both checks poll (120 s), but it does converge. Smithy (resourcegroupstaggingapi): GetResources returns ResourceTagMappingList entries carrying ResourceARN and Tags; nothing in the model excludes EC2 resource types. Recorded behavior only for the convergence timing.
Emulator: GetResources keeps returning an empty ResourceTagMappingList for a tag EC2's CreateTags has already written on a live internet gateway or VPC, for the whole budget the check allows — the poll never reaches one match and the checks end on the harness' 60 s cap with no result.
Why: the tagging API is the cross-service discovery plane. A client that tags EC2 resources and then enumerates them by tag (cost allocation, resource groups, estate inventory, tag-driven automation) finds nothing at all, and cannot distinguish "untagged" from "not indexed".
Fix: the GetResources handler and the tag index it reads — EC2's native tag writes must populate the tagging index for every EC2 resource type (at minimum internet-gateway and vpc here) under the resource's own ARN.
Traces: ec2__resourcegroupstaggingapi.internet-gateway-tags-visible-through-get-resources-both-directions, ec2__resourcegroupstaggingapi.vpc-tags-visible-through-get-resources-both-directions.

## Evidence
ec2__resourcegroupstaggingapi.internet-gateway-tags-visible-through-get-resources-both-directions:
  expected: after ec2.CreateTags(Resources=[igw-id], Tags=[{Key: k, Value: v}]), GetResources(TagFilters=[{Key: k, Values: [v]}]) returns exactly 1 mapping with ResourceARN == arn:aws:ec2:<region>:<account>:internet-gateway/<igw-id>.
  actual:   the poll for len(mappings) == 1 never satisfied; "Error: Check timed out after 60.0s" — GetResources kept returning an empty ResourceTagMappingList.

ec2__resourcegroupstaggingapi.vpc-tags-visible-through-get-resources-both-directions:
  expected: after ec2.CreateTags(Resources=[vpc-id], ...), GetResources(TagFilters=[{Key: k, Values: [v]}]) returns exactly 1 mapping with ResourceARN == arn:aws:ec2:<region>:<account>:vpc/<vpc-id>.
  actual:   same — the poll never satisfied; "Error: Check timed out after 60.0s".
(The tag keys/values and resource ids are per-run; the requirement is that any EC2-written tag on these resources is discoverable by TagFilters and reported under the resource's own ARN.)

## Steps to Reproduce
1. ec2.CreateInternetGateway (and separately ec2.CreateVpc with CidrBlock 10.80.0.0/16).
2. ec2.CreateTags(Resources=[<igw-id> | <vpc-id>], Tags=[{Key: <unique-key>, Value: <unique-value>}]).
3. Poll resourcegroupstaggingapi.GetResources(TagFilters=[{Key: <unique-key>, Values: [<unique-value>]}]) — ResourceTagMappingList stays empty instead of returning the one resource under its ARN.
