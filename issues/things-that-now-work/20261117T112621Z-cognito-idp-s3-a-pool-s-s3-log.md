Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp__s3.xsvc-cognito-idp-s3-log-export-bucket-policy-grant
Issue-Key: cognito-idp__s3|GetBucketPolicy|NoSuchBucketPolicy
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cognito-idp__s3: a pool's S3 log-export destination is accepted but no delivery.logs.amazonaws.com grant is written onto the bucket policy

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetLogDeliveryConfiguration

## Description
Cause 1 of 1: configuring an S3 log-export destination writes no bucket-policy statements.
AWS: Smithy: SetLogDeliveryConfiguration.LogConfigurations with an S3Configuration {BucketArn}; errors InvalidParameterException (400). When Cognito accepts an S3 destination the log-delivery service's access is granted on the destination bucket's own policy: GetBucketPolicy on that bucket returns a policy containing exactly two statements whose Principal.Service is 'delivery.logs.amazonaws.com' — one s3:PutObject (scoped to the bucket's AWSLogs prefix for the calling account) and one s3:GetBucketAcl (the ACL check the delivery service performs first).
Emulator: GetBucketPolicy on the configured destination bucket is refused NoSuchBucketPolicy ('The bucket policy does not exist'), so the check errors out: the accepted configuration granted nothing. (A bucket that was never created is correctly refused NoSuchBucket later in the same check, so the failing read is the configured destination's.)
Why: a client inspecting or auditing the delivery grants that the configuration is supposed to create finds no policy, and nothing in the emulator represents the delivery service's authorization to write the exported logs.
Fix: the S3 destination path of SetLogDeliveryConfiguration in the cognito-idp crate must write the two delivery.logs.amazonaws.com statements onto the destination bucket's policy (and the s3 crate must report them through GetBucketPolicy).
Traces: cognito-idp__s3.xsvc-cognito-idp-s3-log-export-bucket-policy-grant.

## Evidence
botocore.exceptions.ClientError: An error occurred (NoSuchBucketPolicy) when calling the GetBucketPolicy operation: The bucket policy does not exist — raised from the check's _delivery_grants(), which expects a policy with exactly two Principal.Service='delivery.logs.amazonaws.com' statements, actions s3:PutObject and s3:GetBucketAcl.

## Steps to Reproduce
Replay cognito-idp__s3.xsvc-cognito-idp-s3-log-export-bucket-policy-grant: create an S3 bucket, call cognito-idp SetLogDeliveryConfiguration with an S3Configuration naming that bucket's ARN, then call s3 GetBucketPolicy on the bucket and inspect the delivery.logs.amazonaws.com statements.
