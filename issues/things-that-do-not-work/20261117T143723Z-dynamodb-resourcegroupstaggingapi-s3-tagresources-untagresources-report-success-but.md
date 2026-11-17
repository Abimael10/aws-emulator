Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__resourcegroupstaggingapi__s3.tag-resources-mixed-live-bucket-and-absent-table-rejected-per-resource dynamodb__resourcegroupstaggingapi__s3.bulk-tag-untag-reaches-bucket-and-table-native-tags
Issue-Key: dynamodb__resourcegroupstaggingapi__s3|GetBucketTagging|NoSuchTagSet
Issue-Key: dynamodb__resourcegroupstaggingapi__s3.bulk-tag-untag-reaches-bucket-and-table-native-tags|_poll_until#30|Timeout|time.sleep(interval_s)
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# dynamodb__resourcegroupstaggingapi__s3: TagResources/UntagResources report success but never reach an S3 bucket's own tag set — GetBucketTagging answers NoSuchTagSet

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResources

## Description
Cause 1 of 1: a resourcegroupstaggingapi TagResources (and UntagResources) call naming an S3 bucket ARN returns an empty FailedResourcesMap, yet the bucket's own tag set is untouched: GetBucketTagging then fails with NoSuchTagSet, and a check that polls for the tag it added never sees it and exhausts its budget.

AWS: Smithy (resourcegroupstaggingapi) models TagResources(ResourceARNList, Tags) -> FailedResourcesMap and UntagResources(ResourceARNList, TagKeys) -> FailedResourcesMap; a resource that was tagged successfully is absent from FailedResourcesMap, and the write lands on the resource's own tagging surface. For a bucket ARN that surface is S3's GetBucketTagging (Smithy s3: GetBucketTagging -> Tagging.TagSet, error NoSuchTagSet when no tag set exists). So after TagResources adds a key and UntagResources removes one, GetBucketTagging must report exactly the surviving keys; a per-resource failure must be reported in FailedResourcesMap with its own ErrorCode/ErrorMessage rather than reported as success.

Emulator: the batch call reports no per-resource failure, but the bucket's native tag set is never written: 'botocore.exceptions.ClientError: An error occurred (NoSuchTagSet) when calling the GetBucketTagging operation: The TagSet does not exist'. The bulk-tagging check, which polls GetBucketTagging for the merged tag set, never converges and ends 'Check timed out after 60.0s'.

Why: a client that uses the central tagging API to govern tags across services — the whole purpose of resourcegroupstaggingapi — is told the write succeeded while the bucket carries no tags. Cost allocation, tag-based policy conditions and tag-based discovery all then read a different truth than the tagging API reported, and nothing in the response says the write was lost.

Fix: the resourcegroupstaggingapi TagResources/UntagResources handlers — they must apply the write to the named resource's own tag state (for an S3 bucket ARN, the bucket tagging state that GetBucketTagging reads) and report any resource they could not tag in FailedResourcesMap instead of returning it as a success.

Traces: dynamodb__resourcegroupstaggingapi__s3.tag-resources-mixed-live-bucket-and-absent-table-rejected-per-resource, dynamodb__resourcegroupstaggingapi__s3.bulk-tag-untag-reaches-bucket-and-native-tags (suite id: dynamodb__resourcegroupstaggingapi__s3.bulk-tag-untag-reaches-bucket-and-table-native-tags).

## Evidence
Expected: after TagResources(ResourceARNList=[arn:aws:s3:::<bucket>, <table-arn>], Tags={<added>:<value>}) and UntagResources(TagKeys=[<dropped>]) both return FailedResourcesMap == {}, GetBucketTagging(<bucket>) returns the TagSet {<kept>, <added>} and ListTagsOfResource on the table returns the same; a resource that cannot be tagged appears in FailedResourcesMap with its own ErrorCode.
Actual: GetBucketTagging raises '(NoSuchTagSet) ... The TagSet does not exist' — the bucket has no tag set at all despite the reported success; the bulk check polling for the merged set ends 'Check timed out after 60.0s'.

## Steps to Reproduce
1. CreateBucket and PutBucketTagging with two tags; CreateTable and TagResource it with the same two tags.
2. Call resourcegroupstaggingapi TagResources over [bucket ARN, table ARN] adding a third tag, then UntagResources removing one key; assert FailedResourcesMap is empty on both.
3. Call s3 GetBucketTagging on the bucket.
Observed: step 3 raises NoSuchTagSet; polling it for the expected merged tag set never converges.
