Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected
Issue-Key: cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected|run#19|AssertionError|assert got == _REJECTED, f"handler reason {got!r}, expected {_REJECTED!r}"
Issue-Commit: 7e4414010feb5d6e6de4259f326eb0449b52de38

# cloudformation__cloudfront: the CREATE_FAILED handler reason for a refused AWS::CloudFront::PublicKey drops AWS's relayed SDK error envelope (no 'Invalid request provided: <type>: ' prefix, no '(Service: ...)' suffix)

> _Note: this issue was identified against the code base at commit `7e4414010feb5d6e6de4259f326eb0449b52de38`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause on the CloudFormation/CloudFront resource-handler error relay.

Cause 1 of 1: a CloudFront resource handler's CREATE_FAILED reason carries the bare service message instead of AWS's relayed SDK error envelope.
Failing check: cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected

AWS: when a CloudFront control-plane call refuses a resource a stack declares, CloudFormation records that resource's CREATE_FAILED ResourceStatusReason as 'Resource handler returned message: "<message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)'. <message> is the relayed SDK error, composed of the fixed text 'Invalid request provided: ', the template's resource type, ': ', the refusing service's own error message, and the SDK envelope ' (Service: CloudFront, Status Code: 400, Request ID: <request id>) (SDK Attempt Count: <n>)'. For this resource: Smithy CreatePublicKey declares error InvalidArgument (errorKind client, httpError 400) and success 201, and the service message for a PublicKeyConfig.EncodedKey that is not a usable RSA public key is recorded behavior only -- 'Your request contains empty/invalid/out of limits RSA Encoded Key'. So the full reason message AWS reports is 'Invalid request provided: AWS::CloudFront::PublicKey: Your request contains empty/invalid/out of limits RSA Encoded Key (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' with HandlerErrorCode 'InvalidRequest'.
Fixed text vs variables: 'Invalid request provided: ', ': ', ' (Service: CloudFront, Status Code: 400, Request ID: ', ') (SDK Attempt Count: ', ')' are fixed; the resource type comes from the template, the service message from the refusing CloudFront operation, and the request id and attempt count are per-run values that must be present but are never compared literally.

Emulator: the refusal itself now happens -- the stack with the malformed PEM no longer reaches CREATE_COMPLETE -- but the handler reason carries only the bare CloudFront message, 'Your request contains empty/invalid/out of limits RSA Encoded Key', with no 'Invalid request provided: AWS::CloudFront::PublicKey: ' prefix and no '(Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' suffix. HandlerErrorCode ('InvalidRequest') is already correct.

Why: a client that parses a CloudFormation resource-handler failure to recover which service refused the deploy, the HTTP status it returned and the request id to correlate with -- the documented shape of that reason -- gets none of them, so a failed PublicKey deploy cannot be attributed to CloudFront or correlated with a request id, and CI that asserts AWS's reason text fails.

Fix: the CloudFormation resource-provider error relay that formats a downstream CloudFront SDK error into the handler message (shared by the CloudFront resource handlers), not CreatePublicKey's EncodedKey validation -- that validation now refuses correctly. The same envelope drop is already described as cause 1 of the open issue 20261116T174247Z-cloudformation-cloudfront-3-net-new-causes-handler-reason.md (for AWS::CloudFront::CachePolicy) and by 20261111T095650Z-cloudformation-cloudfront-ec2-elbv2-a-cloudformation-resource-handler.md (for an ELBv2 resource); neither carries this check's replay key -- the PublicKey check's key in the former names the earlier 'rejected stack ended CREATE_COMPLETE' assertion, not this handler-reason assertion -- so this failure is not covered by them.

Traces: cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected (mismatch) -- the assertion fires at check_cloudformation__cloudfront_deploy_cloudfront_publickey_invalid_encoded_key_rejected.py line 231.

## Evidence
cloudformation__cloudfront.deploy-cloudfront-publickey-invalid-encoded-key-rejected (mismatch):
  AssertionError: handler reason ('Your request contains empty/invalid/out of limits RSA Encoded Key', 'InvalidRequest'), expected ('Invalid request provided: AWS::CloudFront::PublicKey: Your request contains empty/invalid/out of limits RSA Encoded Key (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')

The check parses the resource's CREATE_FAILED ResourceStatusReason with the regex '^Resource handler returned message: "(?P<message>.*)" \\(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?P<code>[A-Za-z]+)\\)$' and compares (message, code).
HandlerErrorCode matches ('InvalidRequest'). Diverging part: the message. Missing fixed text at the head -- 'Invalid request provided: AWS::CloudFront::PublicKey: ' (the literal 'Invalid request provided: ' plus the template's resource type and ': ') -- and missing fixed text at the tail -- ' (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', where the request id and attempt count are per-run values that must merely be present. The CloudFront service sentence itself is already correct.

## Steps to Reproduce
1. CreateStack with a single AWS::CloudFront::PublicKey whose PublicKeyConfig.EncodedKey is not a parseable RSA public-key PEM (keep the valid-RSA-2048 positive control stack alongside; it reaches CREATE_COMPLETE).
2. Poll DescribeStacks by StackId until a terminal status. The stack reaches ROLLBACK_COMPLETE (the refusal now happens).
3. Call DescribeStackEvents and find the PublicKey resource's CREATE_FAILED event; read ResourceStatusReason and parse it as 'Resource handler returned message: "<message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)'.
4. The emulator's <message> is the bare CloudFront sentence 'Your request contains empty/invalid/out of limits RSA Encoded Key'. AWS's <message> prefixes 'Invalid request provided: AWS::CloudFront::PublicKey: ' and suffixes ' (Service: CloudFront, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)'.
