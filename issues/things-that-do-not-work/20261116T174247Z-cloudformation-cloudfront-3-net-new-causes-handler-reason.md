Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudfront.deploy-cfn-cloudfront-cache-policy-invalid-ttl-order-create-rejected cloudformation__cloudfront.deploy-cfn-cloudfront-standalone-resources-lifecycle cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected
Issue-Key: cloudformation__cloudfront.deploy-cfn-cloudfront-cache-policy-invalid-ttl-order-create-rejected|run#18|AssertionError|assert got == _REJECTED, f"handler reason {got!r}, expected {_REJECTED!r}"
Issue-Key: cloudformation__cloudfront.deploy-cfn-cloudfront-standalone-resources-lifecycle|stack_outputs#7|KeyError|return {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Key: cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected|run#14|AssertionError|assert status == "ROLLBACK_COMPLETE", f"rejected stack ended {status!r}"
Issue-Commit: 16dfe53738d662d0dc392dd1f50e487d5b567c22

# cloudformation__cloudfront: 3 net-new causes — handler reason drops AWS's SDK error envelope, stack Outputs carry no OutputValue, and a malformed PublicKey EncodedKey is accepted

> _Note: this issue was identified against the code base at commit `16dfe53738d662d0dc392dd1f50e487d5b567c22`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Three distinct root causes in the CloudFormation/CloudFront path. Each must be closed before submitting.

=== Cause 1 of 3: a CloudFront resource handler's CREATE_FAILED reason drops AWS's relayed SDK error envelope ===
Failing check: cloudformation__cloudfront.deploy-cfn-cloudfront-cache-policy-invalid-ttl-order-create-rejected
AWS: when a CloudFront control-plane call refuses a resource a stack declares, CloudFormation records the resource's CREATE_FAILED reason as `Resource handler returned message: "<message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)`, where <message> is the relayed SDK error, i.e. the fixed text `Invalid request provided: ` followed by the resource type, `: `, the service's own error message, and the SDK's envelope ` (Service: CloudFront, Status Code: 400, Request ID: <request id>) (SDK Attempt Count: <n>)`. Smithy: CreateCachePolicy declares error InvalidArgument (client, httpError 400); the TTL-ordering message itself is recorded behavior only — `MinTTL, MaxTTL and DefaultTTL should follow order MinTTL less than or equal to DefaultTTL less than or equal to MaxTTL`. HandlerErrorCode is `InvalidRequest`.
Emulator: the handler reason carries only the bare service message with no `Invalid request provided: AWS::CloudFront::CachePolicy: ` prefix and no `(Service: ..., Status Code: ..., Request ID: ...) (SDK Attempt Count: ...)` suffix. HandlerErrorCode (`InvalidRequest`) is already right.
Fixed text vs variables: `Invalid request provided: `, `: `, `(Service: CloudFront, Status Code: 400, Request ID: `, `) (SDK Attempt Count: `, `)` are fixed; the resource type comes from the template, the service message from the refusing CloudFront operation, and the request id / attempt count are per-run values that must be present but are never compared literally.
Why: a client that parses the handler reason to recover the originating service, HTTP status and request id — the documented shape of a CloudFormation resource-handler failure — gets none of them, so a failed deploy cannot be attributed to CloudFront or correlated with a request id.
Fix: the CloudFormation resource-provider error relay that formats a downstream SDK error into the handler message (shared by the CloudFront handlers), not the CachePolicy validation itself — the TTL rule is already enforced. Related open issue 20261111T095650Z-cloudformation-cloudfront-ec2-elbv2-a-cloudformation-resource-handler.md reports the same envelope drop for an ELBv2 resource; it does not carry this check's key.

=== Cause 2 of 3: DescribeStacks returns a CREATE_COMPLETE stack's Outputs entries with no OutputValue ===
Failing check: cloudformation__cloudfront.deploy-cfn-cloudfront-standalone-resources-lifecycle
AWS: every Output declared by a stack that reached CREATE_COMPLETE is returned with both members resolved. Smithy: com.amazonaws.cloudformation#Output has OutputKey ("The key associated with the output") and OutputValue ("The value associated with the output"); a resolved output always carries OutputValue.
Emulator: DescribeStacks returns at least one Outputs entry with OutputKey but no OutputValue for a stack whose outputs are Ref / Fn::GetAtt on standalone CloudFront resources — AWS::CloudFront::OriginAccessControl, CachePolicy (Id, LastModifiedTime), CloudFrontOriginAccessIdentity (Id, S3CanonicalUserId), OriginRequestPolicy, ResponseHeadersPolicy, KeyValueStore (Arn/Id/Status) and ConnectionFunction (ConnectionFunctionArn/Stage/Status) — so reading the outputs raises KeyError 'OutputValue'.
Why: the deploy-then-use pattern is dead for these resources: a client cannot learn the id/ARN of anything the stack created, so nothing downstream can reference it.
Fix: the CloudFormation DescribeStacks output serializer / the output-resolution step that must resolve each declared output's Ref and Fn::GetAtt against the provisioned CloudFront resource before the stack reports CREATE_COMPLETE. Open issues report the same missing member for other resource families (20261111T070000Z-cloudformation-three-stacks-report-an-output-with-no-outputvalue.md, 20261107T085452Z-cloudformation-8-net-new-behavioral-causes-stack-outputs.md); neither carries this check's key.

=== Cause 3 of 3: AWS::CloudFront::PublicKey with a malformed EncodedKey reaches CREATE_COMPLETE instead of being refused ===
Failing check: cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected
AWS: CreatePublicKey validates PublicKeyConfig.EncodedKey and refuses a PEM that is not a usable RSA public key with InvalidArgument (Smithy: CreatePublicKey errors — InvalidArgument, client, httpError 400; success is 201), message `Your request contains empty/invalid/out of limits RSA Encoded Key`. Through CloudFormation the resource goes CREATE_FAILED with handler reason `Invalid request provided: AWS::CloudFront::PublicKey: Your request contains empty/invalid/out of limits RSA Encoded Key (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)` and HandlerErrorCode `InvalidRequest`, the stack ends ROLLBACK_COMPLETE, the refusal comes from the handler (no PROPERTY_VALIDATION row), and no public key with that Name is listed afterwards.
Emulator: the stack with the garbage PEM ends CREATE_COMPLETE — the malformed EncodedKey is accepted and a public key is created. (The positive control with a valid RSA-2048 PEM deployed, so this is an accept-instead-of-refuse divergence, not a provisioning gap.)
Why: a client gets a public key that cannot verify any signature and no signal that its input was garbage; a template that AWS would reject at deploy time silently "succeeds", so signed-URL/signed-cookie configurations built on it fail later with no trace to the bad key.
Fix: input validation in the CloudFront CreatePublicKey handler (reject an EncodedKey that is not a parseable RSA public key PEM with InvalidArgument/400 and AWS's message), which the CloudFormation AWS::CloudFront::PublicKey handler then relays as the CREATE_FAILED reason.

Traces: the three check ids above.

## Evidence
Cause 1 — cloudformation__cloudfront.deploy-cfn-cloudfront-cache-policy-invalid-ttl-order-create-rejected (mismatch):
  expected (message, HandlerErrorCode) = ('Invalid request provided: AWS::CloudFront::CachePolicy: MinTTL, MaxTTL and DefaultTTL should follow order MinTTL less than or equal to DefaultTTL less than or equal to MaxTTL (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')
  actual   (message, HandlerErrorCode) = ('MinTTL, MaxTTL and DefaultTTL should follow order MinTTL less than or equal to DefaultTTL less than or equal to MaxTTL', 'InvalidRequest')
  i.e. the fixed prefix 'Invalid request provided: ' + '<resource type>: ' and the fixed envelope ' (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' are missing; <id> and <n> are per-run values.

Cause 2 — cloudformation__cloudfront.deploy-cfn-cloudfront-standalone-resources-lifecycle (error):
  stack reached CREATE_COMPLETE, then reading DescribeStacks Outputs raised
    KeyError: 'OutputValue'
  at `{o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}`
  expected: every declared Output (OacId, CachePolicyId, CachePolicyLastModified, OaiId, OaiS3CanonicalUserId, OriginRequestPolicyId/LastModified, ResponseHeadersPolicyId/LastModified, KvsName/Id/Arn/Status, ConnFnId/Arn/Stage/Status) returned with both OutputKey and OutputValue.
  actual: at least one entry has OutputKey and no OutputValue.

Cause 3 — cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected (mismatch):
  expected: stack status 'ROLLBACK_COMPLETE', no PROPERTY_VALIDATION row, resource CREATE_FAILED reason ('Invalid request provided: AWS::CloudFront::PublicKey: Your request contains empty/invalid/out of limits RSA Encoded Key (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest'), and ListPublicKeys finds nothing under the declared Name.
  actual: AssertionError: rejected stack ended 'CREATE_COMPLETE' — the malformed PEM was accepted.
  The positive-control stack with a freshly generated RSA-2048 SubjectPublicKeyInfo PEM deployed and its key listed correctly.

## Steps to Reproduce
Cause 1: CreateStack with an AWS::CloudFront::CachePolicy whose CachePolicyConfig violates MinTTL <= DefaultTTL <= MaxTTL. Wait for the terminal status, then read the CachePolicy resource's CREATE_FAILED ResourceStatusReason from DescribeStackEvents and parse it as 'Resource handler returned message: "<message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)'. The emulator's <message> is the bare CloudFront message; AWS's is prefixed 'Invalid request provided: AWS::CloudFront::CachePolicy: ' and suffixed ' (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)'.

Cause 2: CreateStack with AWS::CloudFront::OriginAccessControl, CachePolicy, CloudFrontOriginAccessIdentity, OriginRequestPolicy, ResponseHeadersPolicy, KeyValueStore and ConnectionFunction, and Outputs that Ref each one and Fn::GetAtt their attributes (CachePolicy.LastModifiedTime, Oai.S3CanonicalUserId, Kvs.Arn/Id/Status, ConnFn.ConnectionFunctionArn/Stage/Status). Wait for CREATE_COMPLETE, call DescribeStacks and read Outputs: at least one entry has no OutputValue.

Cause 3: CreateStack with a single AWS::CloudFront::PublicKey whose PublicKeyConfig.EncodedKey is not a valid RSA public-key PEM (plus a control stack using a valid RSA-2048 SubjectPublicKeyInfo PEM). Wait for both terminal statuses: the control reaches CREATE_COMPLETE; the bad stack reaches CREATE_COMPLETE on the emulator where AWS leaves it ROLLBACK_COMPLETE with the InvalidArgument-derived handler reason and no public key created. Confirm with ListPublicKeys that the emulator did create a key under the declared Name.
