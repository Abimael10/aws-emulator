Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__lambda__logs__s3__ssm.cdk-bootstrap-legacy-upgrade-in-use-retains-old-bucket-redeploys-from-new
Issue-Key: cloudformation__ecr__iam__lambda__logs__s3__ssm.cdk-bootstrap-legacy-upgrade-in-use-retains-old-bucket-redeploys-from-new|run#36|AssertionError|assert old_events == ["DELETE_SKIPPED"], (
Issue-Commit: ffc1b1cf6830ce87db020ed3c6a9f4f0c15fe389

# cloudformation__ecr__iam__lambda__logs__s3__ssm: a replacing UpdateStack of a Retain-marked AWS::S3::Bucket issues a real delete (DELETE_IN_PROGRESS/DELETE_FAILED) instead of emitting DELETE_SKIPPED for the retained old bucket

> _Note: this issue was identified against the code base at commit `ffc1b1cf6830ce87db020ed3c6a9f4f0c15fe389`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: an UpdateStack that replaces a resource declaring UpdateReplacePolicy: Retain (the CDK bootstrap toolkit template's StagingBucket, replaced because the bootstrap bucket name changes) is not retained — the provisioner's cleanup phase issues a delete against the OLD physical resource and reports DELETE_IN_PROGRESS followed by DELETE_FAILED, where CloudFormation emits a single DELETE_SKIPPED row and issues no delete at all. Failing check: cloudformation__ecr__iam__lambda__logs__s3__ssm.cdk-bootstrap-legacy-upgrade-in-use-retains-old-bucket-redeploys-from-new.

AWS: recorded behavior only — UpdateReplacePolicy/DeletionPolicy are template-level resource attributes and the stack-event vocabulary is provisioner level, so neither is Smithy-modelled (no CloudFormation operation shape carries them; DescribeStackEvents only returns StackEvent.ResourceStatus/ResourceStatusReason/PhysicalResourceId). When a property change forces replacement of a logical resource whose template declares UpdateReplacePolicy: Retain, CloudFormation provisions the new physical resource and then, for the OLD physical id, emits exactly one DELETE_* event: DELETE_SKIPPED. It issues no delete call for that resource, so no DELETE_IN_PROGRESS, DELETE_COMPLETE or DELETE_FAILED row for the old physical id exists, and the old resource survives the update with its contents intact (here the legacy staging bucket keeps the assets/*.zip the earlier legacy-synthesized deploy published). The fixed part of this requirement is the status token DELETE_SKIPPED and the absence of any other DELETE_* row; the PhysicalResourceId on that row is a value taken from state (the old bucket name).

Emulator: for the StagingBucket logical id, the DELETE_* events the upgrade produces against the old physical id are ['DELETE_IN_PROGRESS', 'DELETE_FAILED'] — DELETE_SKIPPED is never emitted and a real delete is attempted against a resource the template told CloudFormation to retain. Hypothesis: the delete fails because the retained bucket still holds the previously published CDK asset objects, and S3 refuses DeleteBucket with HTTP 409 BucketNotEmpty ("The bucket you tried to delete is not empty."); the sibling open issue below records exactly that reason text on the same code path. Either way the divergence this check grades is the attempted delete and the missing DELETE_SKIPPED row.

Why: a client that relies on UpdateReplacePolicy: Retain to carry data across a rename loses that guarantee — the CDK bootstrap upgrade path (legacy toolkit bucket -> modern toolkit bucket under a new name) exists precisely so earlier-published assets stay readable from the old staging bucket. Against the emulator the rename either destroys that data or, when the bucket is non-empty, turns the upgrade into a resource-level DELETE_FAILED, so the stack's event history reports a failure AWS would never have produced and tooling that polls events for the replacement cannot distinguish a retained resource from a broken delete.

Related open issue, which does NOT carry this check's replay key: 20261113T120806Z-cloudformation-s3-a-replacement-update-of-aws-s3.md files the same Retain defect (plus the CREATE_*-instead-of-UPDATE_* event vocabulary) from check cloudformation__s3.cfn-s3-bucket-create-update-in-place-replace-retained-then-delete; the same provisioner behaviour is filed per-service for sqs/kms/logs/events/iam/lambda/glue/dynamodb/config/scheduler/pipes. None of those cover the check named here, which exercises the CDK bootstrap toolkit stack upgrade.

Fix: the CloudFormation stack provisioner's update/replacement cleanup step in the cloudformation service crate (services/service-com-amazonaws-cloudformation-cloudformation) — the cleanup of a replaced physical resource must consult that resource's UpdateReplacePolicy and, when it is Retain, skip the delete entirely and emit one DELETE_SKIPPED resource event carrying the old PhysicalResourceId instead of DELETE_IN_PROGRESS/DELETE_FAILED.

Traces: cloudformation__ecr__iam__lambda__logs__s3__ssm.cdk-bootstrap-legacy-upgrade-in-use-retains-old-bucket-redeploys-from-new.

## Evidence
Check cloudformation__ecr__iam__lambda__logs__s3__ssm.cdk-bootstrap-legacy-upgrade-in-use-retains-old-bucket-redeploys-from-new [mismatch].

Replay output (verbatim):
  AssertionError: the replaced bucket's delete events are ['DELETE_IN_PROGRESS', 'DELETE_FAILED'], expected ['DELETE_SKIPPED'] for the retained 'sim2real-cdklegacy-38a2e26976e5'
  (check_cloudformation__ecr__iam__lambda__logs__s3__ssm_cdk_bootstrap_legacy_upgrade_in_use_retains_old_bucket_redeploys_from_new.py, line 211: assert old_events == ["DELETE_SKIPPED"])

Expected: the DescribeStackEvents rows for LogicalResourceId 'StagingBucket' whose PhysicalResourceId is the OLD bucket and whose ResourceStatus starts with DELETE_ are exactly ['DELETE_SKIPPED'], and the old bucket still lists the same objects (assets/*.zip) it held before the upgrade.

Actual: those rows are ['DELETE_IN_PROGRESS', 'DELETE_FAILED'] — the emulator issued a delete for the retained bucket and emitted no DELETE_SKIPPED row.

The bucket name 'sim2real-cdklegacy-38a2e26976e5' is a per-run generated value, not part of the requirement; the requirement is the status token DELETE_SKIPPED on the old physical id and the absence of any other DELETE_* row for it.

## Steps to Reproduce
1) CreateStack a legacy CDK toolkit stack whose only resources are StagingBucket (AWS::S3::Bucket, BucketName: <bucket-a>) and StagingBucketPolicy (AWS::S3::BucketPolicy); wait for CREATE_COMPLETE and note CreationTime.
2) Publish an object into <bucket-a> (e.g. deploy a legacy-synthesized CDK app so an assets/<hash>.zip lands there), so a DeleteBucket against it would be refused.
3) UpdateStack the same toolkit stack with the modern bootstrap template, where StagingBucket declares UpdateReplacePolicy: Retain and BucketName: <bucket-b> — a replacing change on the same StackId.
4) After the update completes, DescribeStackEvents(StackName=<toolkit stack>) and keep the rows with LogicalResourceId == 'StagingBucket', PhysicalResourceId == <bucket-a> and ResourceStatus starting with 'DELETE_'.
   Expected: ['DELETE_SKIPPED'].
   Emulator: ['DELETE_IN_PROGRESS', 'DELETE_FAILED'].
5) ListObjectsV2(Bucket=<bucket-a>) — AWS still lists the pre-upgrade objects because no delete was ever issued.
