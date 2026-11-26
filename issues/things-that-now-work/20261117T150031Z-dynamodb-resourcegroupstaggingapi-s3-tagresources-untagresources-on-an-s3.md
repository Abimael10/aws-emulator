Issue-Severity: HIGH
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# dynamodb__resourcegroupstaggingapi__s3: TagResources/UntagResources on an S3 bucket ARN never reach the bucket's own tag set — GetBucketTagging answers NoSuchTagSet

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResources

## Description
Cause 1 of 1: a tag the Resource Groups Tagging API writes on an S3 bucket ARN is acknowledged but never becomes a bucket tag, so S3's own GetBucketTagging still reports that the bucket has no tag set at all.
AWS: Smithy (services/service-com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126/smithy-model/com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126.json): TagResources takes ResourceARNList (1..20) and Tags (1..50) and answers HTTP 200 with FailedResourcesMap; UntagResources takes ResourceARNList + TagKeys and answers 200 with FailedResourcesMap. An ARN that is NOT named in FailedResourcesMap was tagged. For the S3<->tagging coupling the model says nothing, so recorded behavior only: the tagging API is a facade over the owning service's tag plane, so after TagResources(ResourceARNList=[arn:aws:s3:::<bucket>], Tags={<key>:<value>}) returns 200 with that ARN absent from FailedResourcesMap, s3:GetBucketTagging(Bucket=<bucket>) returns a TagSet containing <key>=<value> merged with any tags PutBucketTagging already set, and UntagResources(TagKeys=[<key>]) removes exactly that key from the same TagSet. S3's GetBucketTagging only raises HTTP 404 'NoSuchTagSet' / 'The TagSet does not exist' when the bucket genuinely has no tags.
Emulator: TagResources answers 200 and reports no per-resource failure for the bucket ARN (and correctly reports the absent DynamoDB table in the same batch with ResourceNotFoundException/400), but GetBucketTagging on that bucket then raises ClientError 'An error occurred (NoSuchTagSet) when calling the GetBucketTagging operation: The TagSet does not exist' — the bucket has no tag set, so the write was dropped. In the bulk check, which seeds the bucket with PutBucketTagging and then adds/removes keys through TagResources/UntagResources, the poll of GetBucketTagging for the expected post-write tag set never settles and the check exhausts its deadline ('Check timed out after 60.0s'); the first poll in that check is the bucket's own tag set, so the S3 leg is where it stalls.
Why: a client that tags buckets through the tagging API (bulk tagging jobs, cost-allocation enforcement, IaC tag reconciliation) and then reads back through S3 — or relies on bucket tags for S3 tag-condition IAM policies, lifecycle/cost reporting — sees the tag silently absent while the tagging call reported success. Worse, the acknowledged-then-lost write makes an untagged bucket look compliant.
Fix: the per-ARN dispatch in the TagResources/UntagResources handlers must apply S3 bucket ARNs (arn:aws:s3:::<bucket>) to S3's bucket-tagging state (the same store GetBucketTagging/PutBucketTagging read and write) rather than to a tagging-API-private table, and the S3 side must then serve the merged TagSet instead of NoSuchTagSet.
Traces: dynamodb__resourcegroupstaggingapi__s3.tag-resources-mixed-live-bucket-and-absent-table-rejected-per-resource (errored on GetBucketTagging after the per-resource failure assertions passed), dynamodb__resourcegroupstaggingapi__s3.bulk-tag-untag-reaches-bucket-and-table-native-tags (timed out on the GetBucketTagging poll). No open issue covers the S3 leg of the tagging facade; 20261107T173512Z-dynamodb-resourcegroupstaggingapi-getresources-returns-no-match-for-a.md covers DynamoDB-table discovery through GetResources, not bucket tag write-back.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 against the emulator built from this checkout: 47/66 passed; service dynamodb__resourcegroupstaggingapi__s3 0/2 (0%), 2 failed, neither marked ALREADY FILED nor OWNED.

1. [error] tag-resources-mixed-live-bucket-and-absent-table-rejected-per-resource: the check creates a bucket, builds a well-formed ARN for a DynamoDB table it never creates, and calls rgt.tag_resources(ResourceARNList=[arn:aws:s3:::<bucket>, arn:aws:dynamodb:<region>:<account>:table/<ghost>], Tags={<key>:'partial'}). The FailedResourcesMap assertions (sole key = ghost ARN, ErrorCode 'ResourceNotFoundException', StatusCode 400, ErrorMessage "Requested resource not found: ResourceArn: <ghost arn> not found") all pass. The check then fails on the positive control:
   botocore.exceptions.ClientError: An error occurred (NoSuchTagSet) when calling the GetBucketTagging operation: The TagSet does not exist
   i.e. the live bucket named in the same successful TagResources call has no tag set.

2. [mismatch] bulk-tag-untag-reaches-bucket-and-table-native-tags: 'Check timed out after 60.0s'. The check seeds the bucket with PutBucketTagging({keep, drop}) and the table with ddb.tag_resource, then issues one TagResources (adds <added_key>) and one UntagResources (removes <drop_key>) over both ARNs and polls, first, {t['Key']:t['Value'] for t in s3.get_bucket_tagging(Bucket=<bucket>)['TagSet']} until it equals {keep, added}. That poll never settles, so the ListTagsOfResource and GetResources assertions below are never reached.

Model evidence: smithy_query(query='operation', operation='TagResources,UntagResources') on the RGT model — TagResources input ResourceARNList (required, length 1..20) + Tags (required, length 1..50), output FailedResourcesMap, http POST /TagResources code 200; UntagResources input ResourceARNList + TagKeys, output FailedResourcesMap, code 200. Nothing in the model describes the S3 coupling, hence 'recorded behavior only' above.

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral suite simparity-behavioral-aws-v1, then re-run both checks in trace_ids.

1. CreateBucket <bucket> (with CreateBucketConfiguration.LocationConstraint when the region is not us-east-1).
2. Call resourcegroupstaggingapi TagResources(ResourceARNList=['arn:aws:s3:::<bucket>'], Tags={'<key>':'<value>'}). It answers HTTP 200 with the bucket absent from FailedResourcesMap.
3. Call s3 GetBucketTagging(Bucket='<bucket>'). Expect a TagSet containing <key>=<value>; observe HTTP 404 NoSuchTagSet 'The TagSet does not exist'.
4. Repeat with the bucket pre-seeded via PutBucketTagging(Tagging={'TagSet':[{keep},{drop}]}), then one TagResources adding a third key and one UntagResources removing <drop> over the bucket ARN: expect GetBucketTagging to report exactly {keep, added} and both calls to return an empty FailedResourcesMap; observe the seeded tag set unchanged by the tagging-API calls.
