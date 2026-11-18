Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found
Issue-Key: cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found|run#27|PublishedUrlRejected|direct = f"{ctx.endpoint_url(out['BucketDomainName']).rstrip('/')}/{_FIRST_KEY}"
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# cloudformation__cloudfront__s3: an AWS::S3::Bucket's RegionalDomainName attribute is published as <bucket>.s3.<region>.amazonaws.com, a host the emulator does not serve, so the direct-to-S3 leg of the CFN distribution check is unreachable

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
One net-new root cause (Cause 1 of 1). Failing check: cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found.

Cause 1 of 1: the stack output built from Fn::GetAtt [Bucket, RegionalDomainName] on an AWS::S3::Bucket is published as a real-AWS virtual-hosted-style S3 hostname, an address the emulator announces but does not serve, so no client of the emulator can address the bucket at the host the emulator itself handed it.

AWS:      recorded behavior only for the serve leg (no Smithy model covers the host a CloudFormation attribute resolves to; the CloudFormation wire shape is Stack.Outputs[].OutputValue, read with DescribeStacks). On AWS, a stack declaring a private AWS::S3::Bucket and exporting Fn::GetAtt [Bucket, RegionalDomainName] publishes the bucket's regional REST endpoint host, and that host resolves and answers: the recording performs an unsigned GET of https://<that host>/<key> and S3 refuses it with HTTP 403 and error code AccessDenied, which is the behavior the check is asserting (the bucket policy grants only the distribution's origin access control). The requirement is that the published attribute is an address the emulator actually answers on, and that an unsigned read of the OAC-protected object at that address is refused 403 AccessDenied; the generated bucket name, the region in the host and the stack id are values of the run, not part of the contract.
Emulator: the emulator publishes the real-AWS form and serves nothing there. The replay refuses the published value before the request leaves the harness: 'simparity.published.PublishedUrlRejected: endpoint_url was given https://<bucket>.s3.<region>.amazonaws.com (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener'. This is an egress refusal caused by the emulator announcing an address other than the one it is reached on, not a runner fault.
Why:      a client that deploys a bucket with CloudFormation and feeds the stack's RegionalDomainName/DomainName output to an SDK, a CLI --endpoint-url, a CloudFront origin or any HTTP client is pointed at a host the emulator never answers; every request against the published address fails to connect instead of being served or refused by S3. Because the direct-to-S3 read never happens, the rest of this check's trajectory (the in-place DefaultRootObject update serving the second object, and the post-delete NoSuchDistribution refusal) is also never graded.
Fix:      the CloudFormation resource handler for AWS::S3::Bucket — its resolution of the RegionalDomainName (and DomainName) attribute used by Fn::GetAtt and surfaced through the stack's Outputs on DescribeStacks — together with whatever front-door/host routing must accept the value it publishes. The published host must be one the emulator serves for that bucket, as the already-filed family of 'published host the emulator does not serve' issues requires for other services.
Related, NOT covering: open issue 20261113T232314Z-cloudformation-cloudfront-s3-a-cloudformation-provisioned-cloudfront-distribution.md lists this same check id, but it records a different failure point (the distribution's edge never serving the root object, Issue-Key _assert_serves#26 TimeoutError). That serve leg now gets past the point it failed at and the check fails later, on the bucket host this issue describes; the replay did not report this check as OWNED. Fixing that issue alone leaves this cause open.
Traces:   cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found.

## Evidence
Check cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found, verdict mismatch, from the replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (531/702 passed, 75.6%; bootstrap_signal false — the emulator served the stack calls).

Actual (emulator):
  File "context.py", line 616, in endpoint_route
    return self._published_target(url, "endpoint_url"), {}
  File "context.py", line 632, in _published_target
    raise PublishedUrlRejected(...)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://sim2real-cfncfd-stack-738f5a217517-bucket-daa80a325361.s3.us-west-2.amazonaws.com (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:36345/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
(the bucket name, stack suffix, region and port are per-run values; the defect is the FORM of the published host — a real-AWS s3.<region>.amazonaws.com authority the emulator does not serve.)

Where the value comes from, in the check's template and body:
  Outputs.BucketDomainName = {"Value": {"Fn::GetAtt": ["Bucket", "RegionalDomainName"]}}
  direct = f"{ctx.endpoint_url(out['BucketDomainName']).rstrip('/')}/{_FIRST_KEY}"
  status, body = _http_get(direct, {}, ctx.http_verify)
  assert got == (403, _DIRECT_REFUSED)

Expected (AWS): the published BucketDomainName is an address that answers, and the unsigned GET of the OAC-protected key at that address is refused with HTTP 403 and S3 error code AccessDenied. Status 403 and code AccessDenied are the requirement; the host's own spelling must simply be an address the emulator serves for that bucket.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout; replay the behavioral suite simparity-behavioral-aws-v1.
2. CreateStack with the check's template: a private, encrypted AWS::S3::Bucket, an AWS::CloudFront::OriginAccessControl, and an AWS::CloudFront::Distribution whose origin DomainName is Fn::GetAtt [Bucket, RegionalDomainName] and whose DefaultRootObject names a key in the bucket; export Outputs.BucketDomainName = Fn::GetAtt [Bucket, RegionalDomainName].
3. Wait for CREATE_COMPLETE and read the stack's Outputs with DescribeStacks.
4. Observe BucketDomainName is '<bucket>.s3.<region>.amazonaws.com' — a host the emulator does not serve; building any URL from it is refused with PublishedUrlRejected ('its host is not one of the emulator's own hosts') before a request is made.
5. Expected: the output names an address the emulator answers on, and an unsigned GET of '<that address>/<root object key>' returns 403 with S3 error code AccessDenied (the bucket policy grants only the distribution's OAC).
6. Re-replay cloudformation__cloudfront__s3.cfn-cloudfront-distribution-in-place-update-serves-new-root-object-then-not-found to confirm the fix.
