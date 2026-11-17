Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sns__sqs.custom-resource-delete-blocks-its-dependson-queue-until-provider-answers cloudformation__sns__sqs.custom-resource-failed-delete-response-leaves-its-dependson-queue-alive cloudformation__sns__sqs.deploy-cfn-sns-topic-invalid-tracingconfig-value-rolls-back-creating-nothing
Issue-Key: cloudformation__sns__sqs.custom-resource-delete-blocks-its-dependson-queue-until-provider-answers|run#22|AssertionError|assert probe_mid == "DELETE_IN_PROGRESS", f"Probe status before the answer: expected DELETE_IN_PROGRESS, got {probe_mid!r}"
Issue-Key: cloudformation__sns__sqs.custom-resource-failed-delete-response-leaves-its-dependson-queue-alive|_drain#30|Timeout|page = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Key: cloudformation__sns__sqs.deploy-cfn-sns-topic-invalid-tracingconfig-value-rolls-back-creating-nothing|run#21|AssertionError|assert (code, message) == (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# cloudformation__sns__sqs: a custom resource's DELETE never blocks its dependents or honours the provider's response, and SNS CreateTopic's invalid-TracingConfig refusal drops the "Attributes Reason:" clause (2 causes)

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteStack

## Description
Two net-new root causes on the cloudformation__sns__sqs surface. A fixer must close both separately before submitting.

Cause 1 of 2: a stack delete does not wait for an AWS::CloudFormation::CustomResource provider's DELETE response, so the resources that DependsOn it are torn down (or left) without regard to the provider's answer.
AWS: a custom resource's lifecycle is the provider contract — CloudFormation issues a DELETE request to the ServiceToken, holds the resource in DELETE_IN_PROGRESS, and only proceeds once the provider PUTs a response document to the pre-signed ResponseURL. The model is silent about this (recorded behavior only): the recording shows that while the provider has not answered, DescribeStackResource for the custom resource's dependent probe resource reports DELETE_IN_PROGRESS, and that a provider DELETE response of Status FAILED leaves the custom resource and every resource that DependsOn it alive (the stack settles DELETE_FAILED), rather than deleting them anyway.
Emulator: the dependent probe resource still reads 'CREATE_COMPLETE' at the point where the delete should be in flight — 'Probe status before the answer: expected DELETE_IN_PROGRESS, got CREATE_COMPLETE' — i.e. the delete is not being driven through the provider at all; the ordering and blocking the DependsOn edge is supposed to impose never happens. The sibling check that grades the FAILED delete response does not complete at all (killed at the harness cap, 'Check timed out after 60.0s') on the same stalled custom-resource delete path.
Why: a client that implements a provider to gate teardown (draining a queue, deregistering an endpoint, refusing a delete that would lose data) gets none of that: the gate never holds, and a provider that answers FAILED cannot protect its dependents. Custom-resource-based teardown ordering is unusable.
Fix: the custom-resource lifecycle driver in the stack DELETE path — it must issue the DELETE request to the ServiceToken, hold the resource and its dependents in DELETE_IN_PROGRESS until a response lands on the ResponseURL, and map a Status=FAILED response to DELETE_FAILED for the resource and its DependsOn dependents.
Traces: cloudformation__sns__sqs.custom-resource-delete-blocks-its-dependson-queue-until-provider-answers, cloudformation__sns__sqs.custom-resource-failed-delete-response-leaves-its-dependson-queue-alive.

Cause 2 of 2: SNS CreateTopic's refusal of an unmodelled TracingConfig value omits the 'Attributes Reason: ' clause, so the message is not AWS's.
AWS: Smithy sns#CreateTopic takes Attributes (TopicAttributesMap) and models InvalidParameterException (httpError 400, awsQueryError code InvalidParameter). The recorded refusal for an unmodelled TracingConfig value is HTTP 400, Code 'InvalidParameter', Message 'Invalid parameter: Attributes Reason: Invalid tracing config value: <value>'. Fixed text for this class: 'Invalid parameter: Attributes Reason: Invalid tracing config value: '; the only variable part is the rejected value taken from the request's Attributes.TracingConfig.
Emulator: the status (400) and the code ('InvalidParameter') are right, but the message is 'Invalid parameter: Invalid tracing config value: <value>' — the 'Attributes Reason: ' clause naming the faulted input member is dropped.
Why: 'InvalidParameter' is shared by dozens of unrelated SNS validation failures, so the message is the only discriminator; AWS's wording also tells the caller WHICH input member is at fault (Attributes). A client asserting on AWS's message, or routing on the member name, sees a mismatch. The same text is what CloudFormation forwards as the AWS::SNS::Topic CREATE_FAILED reason, so the stack-event reason diverges too.
Fix: the CreateTopic handler's Attributes validation — emit AWS's fixed text, naming the faulted member, for the invalid-tracing-config class (and the same wording wherever SetTopicAttributes validates TracingConfig).
Traces: cloudformation__sns__sqs.deploy-cfn-sns-topic-invalid-tracingconfig-value-rolls-back-creating-nothing.

## Evidence
Cause 1 — cloudformation__sns__sqs.custom-resource-delete-blocks-its-dependson-queue-until-provider-answers (verdict: mismatch):
  Expected (AWS): with the provider's DELETE response withheld, the probe resource that DependsOn the custom resource reports DELETE_IN_PROGRESS.
  Actual (emulator): `AssertionError: Probe status before the answer: expected DELETE_IN_PROGRESS, got 'CREATE_COMPLETE'` (check file line 236).
  Sibling cloudformation__sns__sqs.custom-resource-failed-delete-response-leaves-its-dependson-queue-alive (verdict: mismatch): `Error: Check timed out after 60.0s` — the check's own budgets are _STACK_TIMEOUT_S 240 s / _REQUEST_TIMEOUT_S 150 s / _QUEUE_GONE_TIMEOUT_S 180 s, so it never reaches a verdict; its first blocking wait is on the same custom-resource DELETE request reaching the provider.

Cause 2 — cloudformation__sns__sqs.deploy-cfn-sns-topic-invalid-tracingconfig-value-rolls-back-creating-nothing (verdict: mismatch):
  Expected (AWS): CreateTopic(Name=<n>, Attributes={'TracingConfig': 'Sampled'}) -> Code 'InvalidParameter', Message 'Invalid parameter: Attributes Reason: Invalid tracing config value: Sampled'.
  Actual (emulator): Code 'InvalidParameter' (correct), Message 'Invalid parameter: Invalid tracing config value: Sampled' — `AssertionError: CreateTopic with TracingConfig Sampled answered ('InvalidParameter', 'Invalid parameter: Invalid tracing config value: Sampled')` (check file line 90). The stack-level legs of the check (ROLLBACK_COMPLETE, one Topic CREATE_FAILED event naming the value, GetTopicAttributes NotFound for the never-created topic) all passed, so the divergence is isolated to the message text.

## Steps to Reproduce
Against the emulator (any region/account):

Cause 1:
1. CreateStack with an AWS::CloudFormation::CustomResource whose ServiceToken is an endpoint you control, plus an AWS::SQS::Queue carrying DependsOn: <the custom resource>. Let it reach CREATE_COMPLETE.
2. DeleteStack(StackName). Do NOT answer the provider's DELETE request.
3. While the request is outstanding, DescribeStackResource for the DependsOn'd probe resource: AWS reports DELETE_IN_PROGRESS; the emulator reports CREATE_COMPLETE (and no DELETE request is driven through the ServiceToken).
4. Repeat, this time PUT a response of {"Status": "FAILED"} to the ResponseURL: AWS leaves the custom resource and the DependsOn'd queue alive and the stack settles DELETE_FAILED.

Cause 2:
1. CreateTopic(Name=<any>, Attributes={'TracingConfig': 'Sampled'}) -> observe Code 'InvalidParameter' with Message 'Invalid parameter: Invalid tracing config value: Sampled'; AWS answers 'Invalid parameter: Attributes Reason: Invalid tracing config value: Sampled'.
2. The same value in an AWS::SNS::Topic TracingConfig property: DescribeStackEvents' Topic CREATE_FAILED ResourceStatusReason must carry AWS's wording.
