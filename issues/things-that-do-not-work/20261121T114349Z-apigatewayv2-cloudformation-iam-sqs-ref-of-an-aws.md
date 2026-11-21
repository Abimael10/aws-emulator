Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-credentialsarn-round-trip apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected
Issue-Key: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-credentialsarn-round-trip|run#25|AssertionError|assert got == want, f"GetIntegration reads {got!r}, expected {want!r}"
Issue-Key: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected|run#31|AssertionError|assert got == want, f"control GetIntegration reads {got!r}, expected {want!r}"
Issue-Commit: 3f1d064d908eb4a469d73223dfef0ca475b0d2e0

# apigatewayv2__cloudformation__iam__sqs: Ref of an AWS::SQS::Queue resolves to a synthesized public sqs.<region>.amazonaws.com URL instead of the queue URL SQS itself serves

> _Note: this issue was identified against the code base at commit `3f1d064d908eb4a469d73223dfef0ca475b0d2e0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause, grounded in two failing checks of this service: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-credentialsarn-round-trip and apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected. Both fail on the same assertion class and are closed by the same fix.

Cause 1 of 1: CloudFormation's intrinsic-function resolution for AWS::SQS::Queue synthesizes a canonical public AWS queue URL rather than reading the QueueUrl the SQS service in this deployment actually assigned to the queue, so the value a stack hands to a downstream resource disagrees with the value SQS reports for the very same queue.

AWS: CloudFormation documents that `Ref` of an AWS::SQS::Queue (and `Fn::GetAtt <Queue>.QueueUrl`) returns the queue's URL — the same string the SQS API returns for that queue. Smithy (sqs): CreateQueue output member `QueueUrl: String`; GetQueueUrl input `QueueName` (required) output `QueueUrl: String`. The model does not fix the URL's text, so the requirement is a consistency one and is recorded behavior only: whatever endpoint the SQS service of this deployment returns from CreateQueue/GetQueueUrl for queue <QueueName> in account <AccountId>, that exact string is what `Ref`/`GetAtt QueueUrl` must yield, and therefore what apigatewayv2 GetIntegration must read back when a template writes `"QueueUrl": {"Ref": "Queue"}` into an AWS::ApiGatewayV2::Integration's RequestParameters. In the recording, GetQueueUrl answered the deployment's own served SQS endpoint for that account and queue name.

Emulator: `Ref` of the template's AWS::SQS::Queue resolves to a string built from a hard-coded public AWS template — `https://sqs.<region>.amazonaws.com/<AccountId>/<QueueName>` — while sqs:GetQueueUrl for the identical queue, in the same run, returns the endpoint the emulator actually serves. The two disagree. The host and port of the served endpoint are per-environment and are NOT the requirement; the defect is that the two answers are produced by two independent sources instead of one.

Why: a client that deploys a stack and then uses the resolved value gets a queue URL that does not address the queue. Everything a template wires from `Ref <Queue>` — API Gateway v2 SQS-SendMessage RequestParameters, Lambda environment variables, SSM parameters, Step Functions task parameters — is populated with an unroutable address, so the integration or application sends to a queue that cannot be reached, and a round-trip assertion comparing the stack's value against sqs:GetQueueUrl can never agree.

Fix: CloudFormation's resource handler / intrinsic-resolution for AWS::SQS::Queue — the component that computes the resource's `Ref` value and its `QueueUrl` attribute must take the URL from the created queue's SQS state (the same value CreateQueue/GetQueueUrl serve) rather than formatting a canonical amazonaws.com string.

Related but NOT covering this: the open issue 20261118T133240Z-apigatewayv2-cloudformation-iam-sqs-2-net-new-causes.md carries the second check's replay key for a different assertion (the relayed CREATE_FAILED refusal wording and its SDK trailer at check line 213); the failure reported here is the later control assertion at check line 245 on GetIntegration's RequestParameters and is a distinct cause. The fixed issue 20261117T135333Z-apigatewayv2-cloudformation-iam-sqs-cloudformation-accepts-an-aws-apigatewayv2-integration-sqs-sendmessage.md covered the missing-MessageBody refusal itself; that refusal is no longer the failing step, so this is not a reopen of it.

Traces: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-credentialsarn-round-trip (check line 207), apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected (check line 245).

## Evidence
Check deploy-cfn-apigatewayv2-integration-credentialsarn-round-trip, line 207 — the check compares apigatewayv2 GetIntegration's RequestParameters against `sqs.get_queue_url(QueueName=queue_name)["QueueUrl"]` read in the same run:

AssertionError: GetIntegration reads ('arn:aws:iam::000000000000:role/sim2real-cfn-integcreds-role-30e50a4645d9', 'SQS-SendMessage', {'MessageBody': '$request.body.msg', 'QueueUrl': 'https://sqs.us-west-2.amazonaws.com/000000000000/sim2real-cfn-integcreds-q-c2733de9a36d'}), expected ('arn:aws:iam::000000000000:role/sim2real-cfn-integcreds-role-30e50a4645d9', 'SQS-SendMessage', {'QueueUrl': 'https://127.0.0.1:44831/000000000000/sim2real-cfn-integcreds-q-c2733de9a36d', 'MessageBody': '$request.body.msg'})

Check deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected, line 245 (the positive control integration of the same check):

AssertionError: control GetIntegration reads ('arn:aws:iam::000000000000:role/sim2real-cfn-sqssubtype-good-role-80c7e160e60e', {'MessageBody': '$request.body.msg', 'QueueUrl': 'https://sqs.us-west-2.amazonaws.com/000000000000/sim2real-cfn-sqssubtype-good-q-8e67225a420c'}), expected ('arn:aws:iam::000000000000:role/sim2real-cfn-sqssubtype-good-role-80c7e160e60e', {'QueueUrl': 'https://127.0.0.1:35349/000000000000/sim2real-cfn-sqssubtype-good-q-8e67225a420c', 'MessageBody': '$request.body.msg'})

Reading the two: the CredentialsArn and IntegrationSubtype members, and the MessageBody mapping, are correct. The only divergent member is QueueUrl. In both recordings the expected string is whatever sqs:GetQueueUrl returned in that run — a per-run host:port that is NOT part of the requirement — and the emulator instead returned the fixed public-AWS form `https://sqs.<region>.amazonaws.com/<account>/<queue-name>`. The fixed part of the requirement is 'equal to the deployment's own sqs:GetQueueUrl answer for the same QueueName and account'; the variable parts (account id, queue name, region, endpoint host/port) come from the request and from the SQS service's state.

## Steps to Reproduce
1. CreateStack (Capabilities=[CAPABILITY_NAMED_IAM]) with a template holding:
   - an AWS::SQS::Queue (logical id Queue) with an explicit QueueName,
   - an AWS::IAM::Role apigateway.amazonaws.com can assume with sqs:SendMessage on {"Fn::GetAtt": ["Queue", "Arn"]},
   - an AWS::ApiGatewayV2::Api with ProtocolType HTTP,
   - an AWS::ApiGatewayV2::Integration with IntegrationType=AWS_PROXY, IntegrationSubtype="SQS-SendMessage", PayloadFormatVersion="1.0", CredentialsArn={"Fn::GetAtt": ["Role","Arn"]} and RequestParameters={"QueueUrl": {"Ref": "Queue"}, "MessageBody": "$request.body.msg"}.
2. Poll DescribeStacks by StackId until CREATE_COMPLETE and read Outputs ApiId / IntegrationId.
3. Call sqs:GetQueueUrl(QueueName=<the template's QueueName>) and record its QueueUrl.
4. Call apigatewayv2:GetIntegration(ApiId, IntegrationId) and read RequestParameters["QueueUrl"].
Observed: step 4 returns `https://sqs.<region>.amazonaws.com/<account>/<queue-name>`, which is not the URL step 3 returned.
Expected: step 4 returns exactly the QueueUrl step 3 returned (CloudFormation's Ref of AWS::SQS::Queue is the queue's own URL as SQS serves it).
