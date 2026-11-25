Issue-Source: trace-sweep-divergence
Issue-Trace: backup-report-jobs-listing
Issue-Step: setup#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-report-jobs-listing
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: s3|PutBucketPolicy|2xx->4xx|status
Issue-Commit: f468f37ea639de53f535b93618c112f23df311d1

# PutBucketPolicy refuses a valid bucket policy with 400 instead of 204 No Content

> _Note: this issue was identified against the code base at commit `f468f37ea639de53f535b93618c112f23df311d1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketPolicy

## Description
One cause.

Cause 1 of 1: a PutBucketPolicy call that AWS accepts is refused by the emulator with a 400 client error, so the bucket never acquires the policy.

AWS: Smithy PutBucketPolicy -> PUT /{Bucket}?policy, input {Bucket (httpLabel, required), Policy (httpPayload, required), ContentMD5, ChecksumAlgorithm, ConfirmRemoveSelfBucketAccess, ExpectedBucketOwner}, output smithy.api#Unit, errors: none modeled (smithy_query operation=PutBucketPolicy on services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json). A successful put has no body; recorded behavior for this step is HTTP 204 with an empty body, which is also what the emulator's own module documentation and PUT_BUCKET_POLICY_SUCCESS_STATUS pin (204, not the model's declared @http code 200). So the SUCCESS STATUS is not what is wrong here: the request is being rejected. The requirement is that a bucket policy of the form AWS Backup's report-delivery setup writes for a report bucket — a resource policy granting a SERVICE principal the object-write action on the bucket's objects, plus the bucket-level read actions on the bucket itself — is STORED and answered 204 with an empty body and no error document.

Emulator: HTTP 400. In this handler 400 is reached only from the MalformedPolicy guards (the published-size check, the policy-document validity check, the action/resource-type fault, the resource fault, and the ungrantable-principal check); the authorization and Block Public Access refusals answer 403 and a missing bucket answers 404, so the emulator is classifying this document as MalformedPolicy and returning an error document in place of the 204. Hypothesis (neither the model nor the recording states which guard fires, and the replay record carries only the status): one of the document-validation guards in the handler rejects a construct this policy uses — most plausibly the action/resource-type pairing or the principal reading — for a document real S3 stores.

Why: the caller is told its policy is malformed when it is not. The bucket is left with whatever policy it had (the handler validates before it touches the store), so every later call that depends on that grant behaves as if the grant was never written: a service that must write into the bucket is denied, GetBucketPolicy reports no policy or the previous one, and GetBucketPolicyStatus misreports the bucket. A client that branches on 400/MalformedPolicy treats its own, correct, configuration as the bug and cannot make progress.

Fix: the policy-document validation reached from the PutBucketPolicy handler — services/service-com-amazonaws-s3-amazons3/src/ops/put_bucket_policy.rs (the MalformedPolicy guards at the size check, is_valid_policy_document, policy_action_fault, policy_resource_fault and service::policy_principals::ungrantable_principal) and whichever of those readings rejects this document. The guard that fires must accept this document form and let the handler store the rendered policy and answer 204; the other guards and the 403/404 branches are unaffected.

Traces: backup-report-jobs-listing#setup#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up: /__admin/health answered 200 and the data listener accepts).

Cause 1 - s3 PutBucketPolicy, fixture backup-report-jobs-listing step setup#6:
  detail: "status: expected 204, got 400"
  expected_status: 204 (empty body, no error document)
  actual_status: 400 (a MalformedPolicy-class error document; the only 400 branch in this handler)
  verdict: mismatch, divergence_key service=s3|PutBucketPolicy|status=204->400|keys=status
  affected: backup-report-jobs-listing#setup#6 (1 step)

Code references for the 400 branches: services/service-com-amazonaws-s3-amazons3/src/ops/put_bucket_policy.rs lines 298-357 (MAX_BUCKET_POLICY_BYTES, is_valid_policy_document, policy_action_fault, policy_resource_fault, ungrantable_principal — all MALFORMED_POLICY_CODE with status 400); the 403 AccessDenied branches are lines 382-424 and the 404 NoSuchBucket branch is lines 368-374, neither of which can produce the observed status. Success status: PUT_BUCKET_POLICY_SUCCESS_STATUS = 204 (line 200).

## Steps to Reproduce
1. git clone the repository and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1.
3. Replay fixture backup-report-jobs-listing through step setup#6: the setup puts a bucket policy on the backup report-delivery bucket with PUT /{Bucket}?policy.
4. Observe the response is 400 with a MalformedPolicy-class error document instead of 204 No Content with an empty body.
5. Follow it with GetBucketPolicy on the same bucket: the policy the step put is not stored.
