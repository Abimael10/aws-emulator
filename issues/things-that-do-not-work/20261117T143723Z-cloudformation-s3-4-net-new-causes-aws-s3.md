Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3.cfn-s3-bucket-accelerate-configuration-in-place-update-then-delete cloudformation__s3.cfn-s3-bucket-accelerate-configuration-invalid-status-rejected cloudformation__s3.cfn-s3-bucket-lifecycle-expiration-days-in-place-update-then-delete cloudformation__s3.cfn-s3-bucket-loggingconfiguration-unsupported-property-rejected-then-round-trips-with-policy cloudformation__s3.deploy-bucket-getatt-name-case-mismatch-rejected-and-rolled-back cloudformation__s3.deploy-website-bucket-getatt-values-match-s3-config
Issue-Key: cloudformation__s3.cfn-s3-bucket-accelerate-configuration-in-place-update-then-delete|run#11|AssertionError|assert status == "Suspended", f"the templated AccelerationStatus must read back: {status!r}"
Issue-Key: cloudformation__s3.cfn-s3-bucket-accelerate-configuration-invalid-status-rejected|run#11|AssertionError|assert got == "Enabled", f"control bucket must read back Enabled, got {got!r}"
Issue-Key: cloudformation__s3.cfn-s3-bucket-lifecycle-expiration-days-in-place-update-then-delete|_wait#13|AssertionError|raise AssertionError(
Issue-Key: cloudformation__s3.cfn-s3-bucket-loggingconfiguration-unsupported-property-rejected-then-round-trips-with-policy|run#11|AssertionError|assert bad_status == "ROLLBACK_COMPLETE", f"expected ROLLBACK_COMPLETE, got {bad_status!r}"
Issue-Key: cloudformation__s3.deploy-bucket-getatt-name-case-mismatch-rejected-and-rolled-back|run#10|CapabilityNotOffered|sites = ctx.s3_website_urls([f"http://{bucket}.s3-website-{ctx.region}.amazonaws.com",
Issue-Key: cloudformation__s3.deploy-website-bucket-getatt-values-match-s3-config|run#11|CapabilityNotOffered|sites = ctx.s3_website_urls([f"http://{bucket}.s3-website-{ctx.region}.amazonaws.com",
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# cloudformation__s3: 4 net-new causes — AWS::S3::Bucket AccelerateConfiguration is never applied, an in-place LifecycleConfiguration update rolls the stack back, an unsupported LoggingConfiguration property is accepted instead of rejected, and no S3 website endpoint is published

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Four distinct root causes in the CloudFormation AWS::S3::Bucket provisioner and in the S3 website endpoint the emulator publishes.

Cause 1 of 4: AWS::S3::Bucket AccelerateConfiguration is accepted by CreateStack/UpdateStack but never applied to the bucket, so GetBucketAccelerateConfiguration reports no Status.
AWS: Smithy (s3) models PutBucketAccelerateConfiguration PUT /{Bucket}?accelerate with a required AccelerateConfiguration payload, and GetBucketAccelerateConfiguration reads that Status back; CloudFormation's AWS::S3::Bucket AccelerateConfiguration.AccelerationStatus maps onto exactly that call, so a templated 'Enabled' must read back as 'Enabled' and an updated value must read back as the updated value.
Emulator: the templated AccelerationStatus reads back as None — 'AssertionError: the templated AccelerationStatus must read back: None' — and in the negative-control check even the sibling control bucket reads back None: 'AssertionError: control bucket must read back Enabled, got None'.
Why: a client that turns on Transfer Acceleration through a stack sees a CREATE_COMPLETE stack whose bucket is not accelerated, and an operator reading the bucket back cannot tell the property was dropped.
Fix: the AWS::S3::Bucket resource provisioner's AccelerateConfiguration handling (it must issue the PutBucketAccelerateConfiguration state write), and the bucket state that backs GetBucketAccelerateConfiguration.
Traces: cloudformation__s3.cfn-s3-bucket-accelerate-configuration-in-place-update-then-delete, cloudformation__s3.cfn-s3-bucket-accelerate-configuration-invalid-status-rejected.

Cause 2 of 4: an in-place UpdateStack that changes an AWS::S3::Bucket LifecycleConfiguration expiration-in-days fails the resource and rolls the stack back.
AWS: recorded behavior only for the failure reason; AWS applies a LifecycleConfiguration change to the existing bucket with no replacement and the stack reaches UPDATE_COMPLETE (the SDK's StackUpdateComplete waiter is the contract the check uses).
Emulator: the waiter terminates on UPDATE_ROLLBACK_COMPLETE with StackStatusReason 'The following resource(s) failed to update: [Bucket]. ' — the bucket update is refused.
Why: an ordinary lifecycle-rule edit (the most common S3 stack update there is) cannot be deployed; the client is told its stack rolled back and the new expiry never takes effect.
Fix: the AWS::S3::Bucket provisioner's update path for LifecycleConfiguration.
Traces: cloudformation__s3.cfn-s3-bucket-lifecycle-expiration-days-in-place-update-then-delete.

Cause 3 of 4: an AWS::S3::Bucket LoggingConfiguration carrying a property the resource does not support is accepted and the stack reaches CREATE_COMPLETE, instead of failing the resource and rolling back.
AWS: recorded behavior only; AWS's AWS::S3::Bucket schema rejects an unrecognised LoggingConfiguration property at create time, the resource goes CREATE_FAILED and the stack ends ROLLBACK_COMPLETE; the same template with the property removed and the log-delivery bucket policy in place then round-trips.
Emulator: 'AssertionError: expected ROLLBACK_COMPLETE, got 'CREATE_COMPLETE'' — the unsupported property is silently ignored.
Why: a template typo or an unsupported field is swallowed, so a client believes logging is configured as written when it is not; CloudFormation's schema validation is the only thing that would have told them.
Fix: the AWS::S3::Bucket provisioner's property-schema validation for LoggingConfiguration (unknown members must fail the resource, not be dropped).
Traces: cloudformation__s3.cfn-s3-bucket-loggingconfiguration-unsupported-property-rejected-then-round-trips-with-policy.

Cause 4 of 4: the emulator publishes no S3 website endpoint, so any stack check that reads a bucket's website URL (Fn::GetAtt WebsiteURL / DomainName) cannot be driven at all.
AWS: Smithy (s3) models PutBucketWebsite and the website endpoint host form <bucket>.s3-website-<region>.amazonaws.com; CloudFormation surfaces it as AWS::S3::Bucket's WebsiteURL attribute, and a GET of that host serves the bucket's index document.
Emulator: the harness is refused before any assertion — 'simparity.behavioral.context.CapabilityNotOffered: the emulator offers no S3 website endpoint (the run names no website endpoint template: s3_website_url=, SIMPARITY_S3_WEBSITE_URL)'. The emulator stands up no website listener and announces no address for one.
Why: a client deploying a static site through CloudFormation gets a WebsiteURL it cannot fetch, and GetAtt-based wiring (CloudFront origins, Route 53 aliases) cannot be validated.
Fix: the S3 website endpoint listener and the address the emulator announces for it; the AWS::S3::Bucket provisioner's WebsiteURL/DomainName attribute must name the address the emulator is actually reached on. Related open issue 20261109T220008Z-s3-7-net-new-causes-cors-preflight-answers.md and 20261113T120806Z-s3-the-emulator-publishes-no-s3-website-endpoint.md describe the same missing endpoint for s3-service checks, but neither carries these two cloudformation__s3 checks.
Traces: cloudformation__s3.deploy-bucket-getatt-name-case-mismatch-rejected-and-rolled-back, cloudformation__s3.deploy-website-bucket-getatt-values-match-s3-config.

## Evidence
Cause 1 — Expected: GetBucketAccelerateConfiguration returns Status 'Enabled' (then the updated value) for a bucket whose template sets AccelerateConfiguration.AccelerationStatus. Actual: 'AssertionError: the templated AccelerationStatus must read back: None'; negative-control check: 'AssertionError: control bucket must read back Enabled, got None'.
Cause 2 — Expected: UPDATE_COMPLETE. Actual: 'did not reach its terminal state: UPDATE_ROLLBACK_COMPLETE ('The following resource(s) failed to update: [Bucket]. ')'.
Cause 3 — Expected: ROLLBACK_COMPLETE (resource CREATE_FAILED on the unsupported LoggingConfiguration property). Actual: 'AssertionError: expected ROLLBACK_COMPLETE, got 'CREATE_COMPLETE''.
Cause 4 — Expected: a reachable website endpoint of the form http://<bucket>.s3-website-<region>.amazonaws.com serving the index document. Actual: 'CapabilityNotOffered: the emulator offers no S3 website endpoint'.

## Steps to Reproduce
Cause 1: CreateStack with an AWS::S3::Bucket whose AccelerateConfiguration.AccelerationStatus is Enabled; call GetBucketAccelerateConfiguration on the created bucket. Then UpdateStack flipping the status and read it back.
Cause 2: CreateStack with an AWS::S3::Bucket carrying a LifecycleConfiguration rule with ExpirationInDays=N; UpdateStack changing N; wait on StackUpdateComplete.
Cause 3: CreateStack with an AWS::S3::Bucket whose LoggingConfiguration carries a property the resource type does not define; observe the stack's terminal status.
Cause 4: CreateStack with an AWS::S3::Bucket that has a WebsiteConfiguration; read Fn::GetAtt WebsiteURL and GET it.
