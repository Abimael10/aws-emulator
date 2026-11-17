Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lambda__ssm.cdk-custom-resource-lifecycle-wires-response-field-and-rejects-conflict cloudformation__lambda__ssm.sam-ssm-parameter-read-policy-other-parameter-denied
Issue-Key: cloudformation__lambda__ssm.cdk-custom-resource-lifecycle-wires-response-field-and-rejects-conflict|_cdk#18|AssertionError|assert result.returncode == expect_rc, (
Issue-Key: cloudformation__lambda__ssm.sam-ssm-parameter-read-policy-other-parameter-denied|run#16|AssertionError|assert statements == [[
Issue-Commit: c31be09f961a7f044ea59076daac7fb377aa83e8

# cloudformation__lambda__ssm: 2 net-new causes — a Custom::AWS (CDK AwsCustomResource) provider never answers CloudFormation, and the SAM SSMParameterReadPolicy template expands to ssm:GetParameterHistory instead of ssm:GetParametersByPath

> _Note: this issue was identified against the code base at commit `c31be09f961a7f044ea59076daac7fb377aa83e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
2 net-new root causes in this service, both grounded in failing checks of this suite.

Cause 1 of 2: a CDK `AwsCustomResource` (resource type `Custom::AWS`) is never answered, so CloudFormation fails the resource with 'did not receive a response from your Custom Resource' and then cannot delete it.
Failing check: cloudformation__lambda__ssm.cdk-custom-resource-lifecycle-wires-response-field-and-rejects-conflict.
AWS: recorded behavior only — the custom-resource protocol is not in the Smithy model. On AWS, `cdk deploy` of a stack containing an `AwsCustomResource` whose onCreate calls `ssm:PutParameter` exits 0: CloudFormation PUTs the request to the provider function, the provider performs the SDK call and PUTs a response with Status=SUCCESS to the pre-signed ResponseURL, the resource reaches CREATE_COMPLETE, GetAtt on the declared response field resolves, and a second resource whose call conflicts is reported through the provider's FAILED response as `Received response status [FAILED] from custom resource. Message returned: The parameter already exists. To overwrite this value, set the overwrite option in the request to true.` On stack delete the Delete request is likewise delivered and acknowledged, so the resource is removed.
Emulator: `cdk deploy <stack> --require-approval never` exits 1. The resource goes CREATE_FAILED with CloudFormation's own no-response text — fixed text `CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [<request-id>]. If you are using the Python cfn-response module, you may need to update your Lambda function code so that CloudFormation can attach the updated version.` (the bracketed request id is the only per-run value) — on the `Custom::AWS` resource, and the stack then reports `The following resource(s) failed to delete: [<LogicalId><hash>]`, so the failed resource is also not removable. The conflict leg (the documented FAILED-response message) is never reached.
Why: a client deploying any CDK app that uses `AwsCustomResource` — the standard escape hatch for an API CloudFormation does not model — gets a failed deploy and a stack it cannot delete. Both the SUCCESS path (GetAtt on the provider's response field) and the FAILED path (the provider's error message surfacing as the resource's status reason) are unavailable.
Fix: the custom-resource driver in CloudFormation's stack engine — it must deliver the Create/Update/Delete request to the provider function, accept the provider's response PUT on a ResponseURL the emulator itself serves, and settle the resource on it; and a resource whose create failed must still be deletable during rollback.

Cause 2 of 2: the SAM policy template `SSMParameterReadPolicy` expands with the wrong third action.
Failing check: cloudformation__lambda__ssm.sam-ssm-parameter-read-policy-other-parameter-denied.
AWS: documented in the SAM policy-template list (serverless-policy-template-list.html#ssm-parameter-read-policy). The template expands to exactly two statements: `{"Action": ["ssm:DescribeParameters"], "Effect": "Allow", "Resource": "*"}` and `{"Action": ["ssm:GetParameters", "ssm:GetParameter", "ssm:GetParametersByPath"], "Effect": "Allow", "Resource": {"Fn::Sub": ["arn:${AWS::Partition}:ssm:${AWS::Region}:${AWS::AccountId}:parameter/${parameterName}", {"parameterName": {"Ref": "<the template's parameterName value>"}}]}}`.
Emulator: GetTemplate(TemplateStage=Processed) on the transformed stack renders the second statement's Action list as `["ssm:GetParameters", "ssm:GetParameter", "ssm:GetParameterHistory"]` — `ssm:GetParameterHistory` appears where `ssm:GetParametersByPath` belongs. The first statement and both Resource expressions match.
Why: the generated role's grant is wrong in both directions: a function that calls `ssm:GetParametersByPath` on the granted parameter is refused although the documented template allows it, and a function that calls `ssm:GetParameterHistory` is allowed although the documented template does not grant it. Anyone auditing the processed template against the SAM documentation sees a policy the documentation does not describe.
Fix: the SAM transform's policy-template table in the CloudFormation transform (the `SSMParameterReadPolicy` entry's action list).

Related open issues, neither of which covers either cause above: 20261108T003043Z-cloudformation-lambda-ssm-a-cdk-custom-resource-whose.md (a CDK custom resource whose onEvent handler RAISES leaves the stack ROLLBACK_FAILED) and 20261029T211043Z-cloudformation-lambda-ssm-cloudformation-a-failed-ssm-dynamic-reference-produces-no.md (a failed SSM dynamic reference).

## Evidence
Cause 1 — expected: `cdk deploy <stack> --require-approval never` exits 0, the Custom::AWS resource reaches CREATE_COMPLETE and GetAtt on its response field resolves; the conflicting sibling is refused with `Received response status [FAILED] from custom resource. Message returned: The parameter already exists. To overwrite this value, set the overwrite option in the request to true.`
Cause 1 — actual: exit code 1, output tail: `CREATE_FAILED | Custom::AWS | Writer/Resource/Default (Writer3ED51D24) CloudFormation did not receive a response from your Custom Resource. Please check your logs for requestId [<request-id>]. If you are using the Python cfn-response module, you may need to update your Lambda function code so that CloudFormation can attach the updated version.` followed by `The following resource(s) failed to delete: [Writer3ED51D24].` (logical id and request id are per-run values; the sentence is fixed text).

Cause 2 — expected second statement Action list: ["ssm:GetParameters", "ssm:GetParameter", "ssm:GetParametersByPath"].
Cause 2 — actual second statement Action list: ["ssm:GetParameters", "ssm:GetParameter", "ssm:GetParameterHistory"]. Full observed Policies[*].PolicyDocument.Statement: [[{'Action': ['ssm:DescribeParameters'], 'Effect': 'Allow', 'Resource': '*'}, {'Action': ['ssm:GetParameters', 'ssm:GetParameter', 'ssm:GetParameterHistory'], 'Effect': 'Allow', 'Resource': {'Fn::Sub': ['arn:${AWS::Partition}:ssm:${AWS::Region}:${AWS::AccountId}:parameter/${parameterName}', {'parameterName': {'Ref': 'AllowedParameter'}}]}}]].

## Steps to Reproduce
Cause 1:
1. Synthesize a CDK app with an `AwsCustomResource` (Custom::AWS) whose onCreate performs ssm:PutParameter and which declares a response field used by GetAtt.
2. `cdk deploy <stack> --require-approval never`.
Observed: exit 1; the Custom::AWS resource is CREATE_FAILED with 'CloudFormation did not receive a response from your Custom Resource...', and rollback reports the resource failed to delete. Expected: exit 0, CREATE_COMPLETE, GetAtt resolved.

Cause 2:
1. CreateStack with Capabilities CAPABILITY_IAM + CAPABILITY_AUTO_EXPAND from a SAM template whose function carries `Policies: [{SSMParameterReadPolicy: {ParameterName: !Ref AllowedParameter}}]`.
2. GetTemplate(StackName=<id>, TemplateStage=Processed) and read Resources.<Role>.Properties.Policies[0].PolicyDocument.Statement.
Observed: third action is ssm:GetParameterHistory. Expected: ssm:GetParametersByPath.
