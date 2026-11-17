Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs__resourcegroupstaggingapi.tag-resources-on-a-log-group-reaches-native-tags-and-is-discovered-by-get-resources logs__resourcegroupstaggingapi.tag-resources-rejects-the-log-group-arns-trailing-wildcard-suffix
Issue-Key: logs__resourcegroupstaggingapi.tag-resources-on-a-log-group-reaches-native-tags-and-is-discovered-by-get-resources|run#14|AssertionError|assert native == {key: value}, (
Issue-Key: logs__resourcegroupstaggingapi.tag-resources-rejects-the-log-group-arns-trailing-wildcard-suffix|run#13|AssertionError|assert set(failed) == {starred_arn}, (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# logs__resourcegroupstaggingapi: 2 causes — TagResources on a log group never reaches CloudWatch Logs' own tags, and a wildcard-suffixed log-group ARN is accepted instead of failing

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResources

## Description
Two net-new root causes where the Resource Groups Tagging API meets CloudWatch Logs' native tag plane.

Cause 1 of 2: a tag written through TagResources is invisible to the log group's own tag API.
AWS: Smithy resourcegroupstaggingapi TagResources applies the tags to the named resources through each service's tagging plane, so a log group tagged this way reports that key/value from CloudWatch Logs' own ListTagsForResource, and GetResources then discovers the group by that tag. There is one tag set per resource, not two.
Emulator: TagResources answers success, but ListTagsForResource on the same log group returns {} — the tag went somewhere the service's own read path cannot see.
Why: the two tag views disagree, so tag-based cost allocation, IAM conditions on aws:ResourceTag and every native read of a tag applied through the tagging API see an untagged log group.
Fix: the TagResources handler's log-group backend — it must write into CloudWatch Logs' native tag state (the state ListTagsForResource/TagResource read) rather than a separate store.
Failing check: logs__resourcegroupstaggingapi.tag-resources-on-a-log-group-reaches-native-tags-and-is-discovered-by-get-resources

Cause 2 of 2: a log-group ARN carrying DescribeLogGroups' trailing ':*' is accepted.
AWS: TagResources returns FailedResourcesMap for each ARN it cannot tag; a log-group ARN with the trailing ':*' suffix (the form DescribeLogGroups reports for a group's streams) is not a taggable resource ARN, so it is reported in FailedResourcesMap while the same group's bare ARN in the SAME batch is tagged successfully.
Emulator: FailedResourcesMap comes back empty — the wildcard-suffixed ARN is treated as a successful tag.
Why: a client that feeds DescribeLogGroups' arn straight into TagResources (a very common mistake) is told it worked; against AWS it half-fails, and the per-ARN failure map that would have told it which form to use is empty.
Fix: the TagResources handler's ARN parsing/validation for log groups (reject the ':*'-suffixed form into FailedResourcesMap).
Failing check: logs__resourcegroupstaggingapi.tag-resources-rejects-the-log-group-arns-trailing-wildcard-suffix

## Evidence
Cause 1: [mismatch] logs__resourcegroupstaggingapi.tag-resources-on-a-log-group-reaches-native-tags-and-is-discovered-by-get-resources — AssertionError: 'ListTagsForResource (a different operation than TagResources) returned {}, expected {<key>: <value>}' (the key/value are the per-run tag the check applied).

Cause 2: [mismatch] logs__resourcegroupstaggingapi.tag-resources-rejects-the-log-group-arns-trailing-wildcard-suffix — AssertionError: FailedResourcesMap named [], expected exactly the wildcard-suffixed ARN 'arn:aws:logs:<region>:<account>:log-group:<name>:*' to fail while the bare ARN in the same batch succeeds.

## Steps to Reproduce
1. CreateLogGroup; call TagResources with the group's bare ARN and one tag, then call logs ListTagsForResource on the group and call GetResources with that TagFilter.
2. Call TagResources with a batch of two ARNs for the same group — the bare ARN and the ':*'-suffixed form — and inspect FailedResourcesMap.
