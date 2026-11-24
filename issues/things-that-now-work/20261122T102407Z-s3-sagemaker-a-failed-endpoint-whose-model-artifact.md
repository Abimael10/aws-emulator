Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__sagemaker.sagemaker-endpoint-deploy-from-a-deleted-model-artifact-fails-with-an-s3-failure-reason
Issue-Key: s3__sagemaker.sagemaker-endpoint-deploy-from-a-deleted-model-artifact-fails-with-an-s3-failure-reason|run#44|AssertionError|assert broken.get("FailureReason") == (
Issue-Commit: 6b0d7166eeee5b3e65d1a0791f9b11ca91cde3cf

# s3__sagemaker: a Failed endpoint whose model artifact is missing reports an emulator-authored FailureReason instead of AWS's "Failed to download model data(bucket: …, key: …)" text

> _Note: this issue was identified against the code base at commit `6b0d7166eeee5b3e65d1a0791f9b11ca91cde3cf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEndpoint

## Description
One net-new root cause.

Cause 1 of 1: the FailureReason the emulator attaches to an endpoint that could not be deployed because its model's ModelDataUrl no longer resolves in S3 is emulator-authored prose, not the text AWS emits, and it does not name the bucket and key it failed to download.
Failing check ids: s3__sagemaker.sagemaker-endpoint-deploy-from-a-deleted-model-artifact-fails-with-an-s3-failure-reason

AWS:      Smithy (sagemaker) documents DescribeEndpoint output member FailureReason only as "If the status of the endpoint is Failed, the reason why it failed" — the wording itself is recorded behavior only. The recording shows that when CreateEndpoint's production variant references a Model whose PrimaryContainer.ModelDataUrl points at an S3 object that does not exist (HeadObject on it answers 404 Not Found), the endpoint settles in EndpointStatus=Failed and DescribeEndpoint returns FailureReason whose FIXED text is:
            "Failed to download model data(bucket: " … ", key: " … "). Please ensure that there is an object located at the URL and that the role passed to CreateModel has permissions to download the model."
          Its VARIABLE parts are exactly two values taken from the referenced model's ModelDataUrl: the S3 bucket name and the S3 object key (no other value from the request or state appears, and no per-run id or timestamp is part of the message). Hosting is otherwise unaffected: a sibling endpoint over a byte-identical artifact at a live key reaches InService with no FailureReason, and DescribeModel keeps echoing the dangling URI because the control plane never resolves it.

Emulator: the endpoint does reach Failed, but DescribeEndpoint returns a differently worded reason that omits the bucket/key pair and substitutes an s3:// URI and a different permission clause:
            "Failed to download data. Cannot download s3://<bucket>/<output-prefix>/<training-job>/output/model.tar.gz. Please ensure that there is an object located at the URL and that the role passed to CreateModel has permissions to access the object."
          i.e. the fixed head is "Failed to download data. Cannot download " + the URI, and the tail says "permissions to access the object" where AWS says "permissions to download the model". The check asserts the exact FailureReason and fails with AssertionError: "the FailureReason must attribute the failure to the download of the exact deleted key".

Why:      FailureReason is the only machine- and human-readable diagnosis of a failed deployment. Tooling and runbooks that match on AWS's text (or parse the "bucket: X, key: Y" pair out of it to re-upload or re-grant on the exact object) find neither the expected prefix nor the bucket/key fields, so an artifact-missing failure is reported as an unclassified deployment failure and operators cannot see WHICH object is missing without separately resolving the model.

Fix:      the endpoint-deployment path of the sagemaker service — the state transition that marks an endpoint Failed when a production variant's model artifact cannot be resolved, and the string it stores in the endpoint's FailureReason that DescribeEndpoint serializes. The reason must be built from the referenced model's ModelDataUrl split into bucket and key, using AWS's fixed wording above, rather than formatted from the whole s3:// URI.

Traces:   behavioral check s3__sagemaker.sagemaker-endpoint-deploy-from-a-deleted-model-artifact-fails-with-an-s3-failure-reason (deploys two endpoints over byte-identical artifacts, deletes one object, asserts the broken endpoint's FailureReason while the intact one serves inference).

## Evidence
Expected (AWS, asserted by the check): EndpointStatus="Failed" and FailureReason == "Failed to download model data(bucket: {bucket}, key: {key}). Please ensure that there is an object located at the URL and that the role passed to CreateModel has permissions to download the model." where {bucket}/{key} are the bucket and key of the deleted object named by the model's PrimaryContainer.ModelDataUrl.

Actual (emulator): EndpointStatus="Failed" with FailureReason == "Failed to download data. Cannot download s3://<bucket>/output/<training-job-name>/output/model.tar.gz. Please ensure that there is an object located at the URL and that the role passed to CreateModel has permissions to access the object."

Replay verdict: mismatch — AssertionError at check line 367: "the FailureReason must attribute the failure to the download of the exact deleted key". Positive control in the same run (identical artifact bytes at a live key) reached InService and returned a finite score, so hosting itself is not the cause.

## Steps to Reproduce
1. Create an S3 bucket and produce a model artifact at s3://<bucket>/<key> (the check uses a completed training job's ModelArtifacts.S3ModelArtifacts).
2. CopyObject it to a second key s3://<bucket>/intact-copy/model.tar.gz so two byte-identical copies exist.
3. CreateModel twice (same Image, same execution role): one with ModelDataUrl=s3://<bucket>/<key> ("broken"), one with ModelDataUrl=s3://<bucket>/intact-copy/model.tar.gz ("intact"). CreateEndpointConfig + CreateEndpoint for each.
4. DeleteObject s3://<bucket>/<key> and confirm HeadObject answers 404 Not Found.
5. Poll DescribeEndpoint on both endpoints until terminal.
6. The intact endpoint is InService with no FailureReason (control). The broken endpoint is Failed — compare its FailureReason to "Failed to download model data(bucket: <bucket>, key: <key>). Please ensure that there is an object located at the URL and that the role passed to CreateModel has permissions to download the model.": the emulator instead returns "Failed to download data. Cannot download s3://<bucket>/<key>. Please ensure that there is an object located at the URL and that the role passed to CreateModel has permissions to access the object."
