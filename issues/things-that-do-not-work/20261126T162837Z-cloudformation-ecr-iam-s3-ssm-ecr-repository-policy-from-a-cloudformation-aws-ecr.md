Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-container-assets-repository-refuses-to-overwrite-an-image-tag
Issue-Key: cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-container-assets-repository-refuses-to-overwrite-an-image-tag|run#23|AssertionError|assert pull == [
Issue-Commit: 34fefd7aecead9141157dc0abcdfe1d4823d27aa

# ECR repository policy from a CloudFormation AWS::ECR::Repository loses an ARN's resource-type segment: the Lambda pull condition reads arn:aws:lambda:<region>:<account>:* instead of ...:function:*

> _Note: this issue was identified against the code base at commit `34fefd7aecead9141157dc0abcdfe1d4823d27aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRepositoryPolicy

## Description
One net-new root cause, reached by deploying the CDK bootstrap toolkit template and reading the container-assets repository's pull policy back.

Cause 1 of 1: the repository policy stored for an AWS::ECR::Repository's RepositoryPolicyText drops the resource-type segment of an ARN that has one, so the LambdaECRImageRetrievalPolicy statement's Condition value loses the literal `function:` and becomes a registry-wide wildcard ARN.

AWS: Smithy (ecr) SetRepositoryPolicy takes `policyText` (@required, RepositoryPolicyText, @length 0..10240) and GetRepositoryPolicy returns `policyText` with the same @length bound; neither models any rewriting of the document, so AWS stores and returns the IAM policy JSON as submitted (modulo IAM's own normalization of Action/Principal/Resource to JSON arrays). CloudFormation resolves `Fn::Sub`/`Fn::Join` to a plain string and passes it through verbatim. The CDK bootstrap template (read offline in the check with `cdk bootstrap --show-template --json`) declares the container-assets repository's pull policy with two statements, in order: Sid LambdaECRImageRetrievalPolicy (Principal Service lambda.amazonaws.com, Action ecr:BatchGetImage + ecr:GetDownloadUrlForLayer, Condition StringLike aws:sourceArn = `arn:<partition>:lambda:<region>:<account>:function:*`) and Sid EmrServerlessImageRetrievalPolicy (Principal Service emr-serverless.amazonaws.com, Condition StringLike aws:sourceArn = `arn:<partition>:emr-serverless:<region>:<account>:/applications/*`). On AWS, GetRepositoryPolicy on the deployed repository returns both statements with exactly those condition strings, the partition/region/account being values taken from the deployment.

Emulator: GetRepositoryPolicy answers 200 and returns both statements with the correct Sids, Effects, Actions and Principals and the correct emr-serverless condition, but the Lambda statement's `aws:sourceArn` is `arn:aws:lambda:<region>:<account>:*` — the `function:` resource-type segment is gone. The surviving emr-serverless value (`:/applications/*`, which contains no colon in its resource portion) shows the loss is specific to an ARN whose resource portion is `<resource-type>:<resource-id>`. Hypothesis: the string is parsed into an ARN and re-serialized with only six colon-separated fields (partition, service, region, account, resource), so the seventh segment is discarded; equivalently, a pseudo-parameter substitution consumes the trailing `:function` text.

Why: the condition is the whole point of that statement — it restricts which caller ARNs may pull the image. A client that deploys this template believes only Lambda *functions* in its own account can pull, and reads the policy back to audit or edit it; with `:*` the condition matches every Lambda-service resource ARN in the account, so the policy the client sees and the policy it asserted on do not agree, and a Get → edit → Set round trip silently rewrites the deployed policy to the widened form. Any tooling that diffs the deployed policy against the template (drift detection, policy linters, CDK re-bootstrap) reports a difference AWS would not report.

Fix: whatever ARN-shaped string handling sits between CloudFormation's AWS::ECR::Repository resource handler (its RepositoryPolicyText property resolution, including Fn::Sub of `${AWS::Partition}`/`${AWS::Region}`/`${AWS::AccountId}`) and ECR's SetRepositoryPolicy state — the policy document must be stored and returned byte-for-byte as submitted, with no ARN re-serialization; ECR's GetRepositoryPolicy serializer must echo the stored document.

Note: open issue 20261110T232635Z-cloudformation-ecr-iam-s3-ssm-cdk-bootstrap-toolkit-fidelity-ecr-lifecyclepolicy-dropped-defaulted.md lists this check among its trace ids, but its causes are the dropped LifecyclePolicy, unreported defaulted stack parameters, the missing IAM PermissionsBoundary, the downgraded bucket encryption and the un-canonicalized account principal; it does not carry this failure's replay key (the lifecycle assertion earlier in the same check now passes and the check fails on the repository policy instead), so this condition-string loss is not covered by it.

Traces: cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-container-assets-repository-refuses-to-overwrite-an-image-tag.

## Evidence
Check cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-container-assets-repository-refuses-to-overwrite-an-image-tag, assertion on the statements of ecr:GetRepositoryPolicy(repositoryName=cdk-<qualifier>-container-assets-<account>-<region>)["policyText"]:

AssertionError: repository policy statements are [{'Effect': 'Allow', 'Sid': 'LambdaECRImageRetrievalPolicy', 'Condition': {'StringLike': {'aws:sourceArn': 'arn:aws:lambda:<region>:<account>:*'}}, 'Action': ['ecr:BatchGetImage', 'ecr:GetDownloadUrlForLayer'], 'Principal': {'Service': ['lambda.amazonaws.com']}}, {'Effect': 'Allow', 'Sid': 'EmrServerlessImageRetrievalPolicy', 'Condition': {'StringLike': {'aws:sourceArn': 'arn:aws:emr-serverless:<region>:<account>:/applications/*'}}, 'Action': ['ecr:BatchGetImage', 'ecr:DescribeImages', 'ecr:GetDownloadUrlForLayer'], 'Principal': {'Service': ['emr-serverless.amazonaws.com']}}]

Expected (AWS, as the CDK bootstrap template declares it): the same two statements in the same order, with the first statement's Condition.StringLike['aws:sourceArn'] == 'arn:aws:lambda:<region>:<account>:function:*'.

Fixed text of the diverging value: 'arn:<partition>:lambda:' + ':' + ':function:*'; the variable parts are the partition, the region and the 12-digit account id of the deployment (us-west-2 / 000000000000 in the recorded run — run-specific values, not requirements). Only the `function:` segment differs; everything else in both statements matches.

## Steps to Reproduce
1. `cdk bootstrap --qualifier <q>` the CDK toolkit stack against the emulator (services: cloudformation, ecr, iam, s3, ssm), so CloudFormation creates the AWS::ECR::Repository logical id ContainerAssetsRepository with the template's RepositoryPolicyText.
2. Read the template the CLI deploys offline for comparison: `cdk bootstrap --show-template --json --ci`, and note ContainerAssetsRepository's RepositoryPolicyText — statement Sid LambdaECRImageRetrievalPolicy has Condition StringLike aws:sourceArn `arn:${AWS::Partition}:lambda:${AWS::Region}:${AWS::AccountId}:function:*`.
3. Call ecr:GetRepositoryPolicy(repositoryName="cdk-<q>-container-assets-<account>-<region>") and JSON-parse policyText.
4. Observe Statement[0].Condition.StringLike["aws:sourceArn"] == "arn:aws:lambda:<region>:<account>:*" — the `function:` resource-type segment has been dropped. Statement[1] (emr-serverless, resource `/applications/*`, no colon in the resource portion) is intact, which isolates the loss to ARNs carrying a `<resource-type>:<resource-id>` resource.
