Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-model-card-create-export-verify
Issue-Step: teardown#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-model-card-create-export-verify
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: s3|DeleteBucket|2xx->4xx|status
Issue-Commit: 17fc776fd0c94fbf7cc0d582120a564335a8fc64

# s3 DeleteBucket answers 409 BucketNotEmpty for a bucket the recorded teardown emptied

> _Note: this issue was identified against the code base at commit `17fc776fd0c94fbf7cc0d582120a564335a8fc64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteBucket

## Description
ONE cause. A bucket that a client emptied by deleting every object it was told about cannot be deleted against the emulator: the delete is refused 409 instead of succeeding 204.

Cause 1 of 1: DeleteBucket of an emptied general purpose bucket is refused 409 BucketNotEmpty (fixture sagemaker-model-card-create-export-verify, step teardown#7).

AWS: Smithy (s3) DeleteBucket -> http {method: DELETE, uri: /{Bucket}, code: 204}, input Bucket (BucketName, @httpLabel, required) and ExpectedBucketOwner (@httpHeader x-amz-expected-bucket-owner); the operation models NO errors and its output shape is Unit. Its documentation states the rule: "Deletes the S3 bucket. All objects (including all object versions and delete markers) in the bucket must be deleted before the bucket itself can be deleted." So a bucket whose objects have all been deleted is deleted and answered 204 No Content with an empty body and no error element; only a bucket that still holds an object, object version or delete marker is refused 409 with code BucketNotEmpty (documented by S3, not modeled as a shape). In the recording the client deletes the objects it knows about — every key S3 reported to it — and AWS then answers the DELETE with 204.

Emulator: the same DELETE answers 409. The only 409 this handler can produce is its emptiness refusal (services/service-com-amazonaws-s3-amazons3/src/ops/delete_bucket.rs, the `if !bucket.holds_no_versions()` branch, which returns state::BUCKET_NOT_EMPTY_CODE with status 409 and the BucketNotEmpty sentence), so the emulator still considers the bucket to hold an object after the client deleted everything S3 had named to it. Expected status 204, actual status 409.

Why: a client cannot release the bucket name or finish a teardown. The refusal is indistinguishable from a genuine BucketNotEmpty, so correct client code (delete all keys you can list / all keys the service reported, then delete the bucket) loops or aborts, CloudFormation/CDK style stack deletes fail on the bucket resource, and the bucket plus its contents leak for the rest of the emulator's life. Every later step that assumes the name is free diverges too.

Hypothesis: the residue is an object the emulator wrote at a key it never reported. In this same fixture the model-card export artifact is written at `model-card/export/<card>-<version>-<job>.pdf` while the response's ExportArtifacts.S3ExportArtifacts (and AWS) name `model-card/export/<card>/<job>.pdf` — the divergence the open issue 20261113T193514Z-sagemaker-resource-provenance-and-export-artifact-response-fields.md owns as its Cause 3. A client that deletes exactly the artifact URI it was handed therefore removes nothing, and the stray object is what makes the bucket non-empty here. That open issue does NOT own this DeleteBucket cluster (the replay reports it unowned) and closing it only removes one way to produce the residue; the invariant to restore is that no object exists in a bucket under a key no response the client received ever named.

Fix: s3 DeleteBucket's emptiness check and the state it reads (the bucket's version/object index behind `Bucket::holds_no_versions`) must agree with what the service reports to clients — a bucket whose every client-visible key has been deleted must answer 204. Concretely, service-written objects must be stored under exactly the key the owning operation's response reports (here the model-card export writer in the sagemaker crate), or the emptiness check must not count residue the client was never given a key for. Do not weaken the BucketNotEmpty rule itself: a bucket that really holds an object, version or delete marker must still be refused 409 BucketNotEmpty.

Traces: sagemaker-model-card-create-export-verify#teardown#7.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, 497 steps, 398 passed, score 80.1%). One unowned, non-stub divergence cluster:

service=s3 | DeleteBucket | status=204->409 | keys=status
  fixture_id: sagemaker-model-card-create-export-verify
  step_key:   teardown#7
  verdict:    mismatch
  detail:     "status: expected 204, got 409"
  expected_status 204, actual_status 409
  affected_count 1 (sagemaker-model-card-create-export-verify#teardown#7)
  owned_by: [] (no open issue carries this replay key), stub_path: ""

Expected (AWS): 204 No Content, empty body — the bucket's objects were all deleted in the preceding teardown steps, which replayed without divergence.
Actual (emulator): 409. The handler's only 409 path is BucketNotEmpty (fixed text: the BucketNotEmpty code and the "bucket you tried to delete is not empty" sentence; no variable parts), so the store still holds an object version for the bucket.

Model evidence (smithy_query, services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json): DeleteBucket input {Bucket: BucketName @httpLabel required, ExpectedBucketOwner: AccountId @httpHeader x-amz-expected-bucket-owner}, output Unit, errors [], http {DELETE /{Bucket}, code 204}. Operation doc: "All objects (including all object versions and delete markers) in the bucket must be deleted before the bucket itself can be deleted."

Code evidence: services/service-com-amazonaws-s3-amazons3/src/ops/delete_bucket.rs lines 207 (SUCCESS_STATUS = 204) and 314-337 (the `!bucket.holds_no_versions()` -> BUCKET_NOT_EMPTY_CODE / 409 refusal).

Related (does not own this cluster): 20261113T193514Z-sagemaker-resource-provenance-and-export-artifact-response-fields.md, whose Cause 3 records that this fixture's export PDF is written at model-card/export/<card>-<version>-<job>.pdf instead of the model-card/export/<card>/<job>.pdf the response reports.

## Steps to Reproduce
1. Clone this checkout and boot the emulator built from it.
2. Replay the simparity-wire-aws-v1 wire corpus, fixture sagemaker-model-card-create-export-verify, in order: it creates a bucket, creates a model card, runs CreateModelCardExportJob with OutputConfig.S3OutputPath pointing into that bucket, verifies the job, then in teardown deletes the objects it was told about and finally issues DELETE /<bucket> (step teardown#7).
3. Observe step teardown#7: the emulator answers 409 (BucketNotEmpty) where the recording has 204 No Content.
4. Equivalent minimal repro: create a bucket, have a service write an object into it through an operation whose response reports the object's S3 URI, delete exactly that reported URI with DeleteObject, then DELETE the bucket — the delete is refused 409 because an object remains under a key no response named.
5. Expected after the fix: step teardown#7 answers 204 with an empty body, and a bucket that genuinely still holds an object/version/delete marker continues to answer 409 BucketNotEmpty.
