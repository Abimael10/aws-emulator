Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot__lambda.cfn-deploy-iot-rule-lambda-missing-permission-negative iot__lambda.cfn-deploy-iot-rule-wrong-lambda-arn-negative
Issue-Key: iot__lambda.cfn-deploy-iot-rule-lambda-missing-permission-negative|run#19|AssertionError|assert failure_total > 0, f"RuleAction Failure metric for {rule_a} never incremented"
Issue-Key: iot__lambda.cfn-deploy-iot-rule-wrong-lambda-arn-negative|run#19|AssertionError|assert failure_total > 0, f"RuleAction Failure metric for {rule_bad} never incremented"
Issue-Commit: 8470bd15600894c04d6297f04472e87ed5582e16

# iot__lambda: the rule engine publishes no AWS/IoT RuleAction metrics, so a failed rule action (missing lambda:InvokeFunction permission, or a target function that does not exist) is unobservable

> _Note: this issue was identified against the code base at commit `8470bd15600894c04d6297f04472e87ed5582e16`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTopicRule

## Description
Cause 1 of 1: when a topic rule's Lambda action cannot be performed, the IoT rule engine records no failure anywhere a client can read — the AWS/IoT RuleAction Failure metric series for that rule never exists.
AWS: recorded behavior only (the iot Smithy model describes the rule API, not the metrics the rule engine emits; CloudWatch's ListMetrics/GetMetricStatistics are the read path). AWS IoT Core publishes rule-engine metrics in the AWS/IoT namespace dimensioned by RuleName, and an action that is attempted and fails increments RuleAction Failure for that rule: a Lambda action whose function has no AWS::Lambda::Permission for iot.amazonaws.com fails AccessDenied, and a Lambda action pointing at a syntactically valid but never-created function ARN fails ResourceNotFound — both are failures of the action, so ListMetrics(Namespace="AWS/IoT", MetricName="Failure", Dimensions=[{"Name":"RuleName","Value":<rule>}]) resolves a series and GetMetricStatistics over the surrounding window reports Sum > 0. The rule's sibling in the same stack — a correctly permissioned action against a real function — succeeds and does not increment Failure.
Emulator: after messages are published to the rule's topic and the negative control is confirmed (the ghost function was never invoked; the in-stack positive control rule did invoke its function), the Failure series for the failing rule never appears: the checks poll ListMetrics/GetMetricStatistics for 240 s and end "RuleAction Failure metric for <rule> never incremented". Both failure modes — missing invoke permission and non-existent target function — are equally silent.
Why: rule-action failures are silent by design on the data plane (nothing is returned to the publisher), so these metrics are the only signal a client has that its rule is dropping every message. A caller that deploys a rule with a missing permission or a stale function ARN sees a healthy-looking stack, no error and no metric, and concludes its pipeline is delivering when nothing is. Any alarm built on AWS/IoT RuleAction Failure never fires.
Fix: the IoT rule-engine action-dispatch path — when an action is attempted and fails, publish the AWS/IoT RuleAction metrics (Failure, and correspondingly Success on the happy path) dimensioned by RuleName into the CloudWatch metric state that ListMetrics and GetMetricStatistics read.
Traces: iot__lambda.cfn-deploy-iot-rule-lambda-missing-permission-negative, iot__lambda.cfn-deploy-iot-rule-wrong-lambda-arn-negative.
Related open issues that do NOT cover this cause (they report the data-plane address and rule->Lambda invocation blocking OTHER checks, and carry neither of these check ids): 20261109T120740Z-iot-lambda-every-rule-lambda-check-is-blocked.md, 20261110T210703Z-iot-lambda-an-iot-data-publish-from-a.md, 20261112T065656Z-iot-lambda-the-in-function-iot-data-publish.md. In these two checks the rule path itself got far enough for the positive control and the no-invocation assertion to pass; only the metric is missing.

## Evidence
Replayed checks (both in the iot__lambda group of suite simparity-behavioral-aws-v1):
- iot__lambda.cfn-deploy-iot-rule-lambda-missing-permission-negative: "AssertionError: RuleAction Failure metric for <rule-a> never incremented" after _wait_for_rule_action_metric(..., "Failure", timeout_s=240.0). The stack deploys two identical rule->Lambda wirings sharing one execution role; RuleA/FnA lacks its AWS::Lambda::Permission, RuleB/FnB has one and is the in-file positive control.
- iot__lambda.cfn-deploy-iot-rule-wrong-lambda-arn-negative: "AssertionError: RuleAction Failure metric for <rule-bad> never incremented"; the immediately preceding assertion "a non-existent function was somehow invoked" passed, i.e. the ghost function really was not invoked.
Read path used by both: cloudwatch.list_metrics(Namespace="AWS/IoT", MetricName="Failure", Dimensions=[{"Name": "RuleName", "Value": <rule>}]) followed by cloudwatch.get_metric_statistics(Namespace="AWS/IoT", MetricName="Failure", Dimensions=<those>, Period=60, Statistics=["Sum"]) over a ~15-minute window.
Expected: ListMetrics resolves the series and Sum > 0. Actual: the series never resolves for the whole 240 s poll. The rule names above are one run's generated values.

## Steps to Reproduce
1. git_clone(); 2. run_behavioral_suite(); 3. replay iot__lambda.cfn-deploy-iot-rule-lambda-missing-permission-negative and iot__lambda.cfn-deploy-iot-rule-wrong-lambda-arn-negative.
Direct repro: deploy a stack with (a) an AWS::Lambda::Function and an AWS::IoT::TopicRule whose Lambda action targets it, WITHOUT an AWS::Lambda::Permission for iot.amazonaws.com, plus (b) the same pair WITH the permission as a control; publish several messages to the rule's topic; confirm the control function was invoked and the broken one was not; then poll cloudwatch:ListMetrics(Namespace="AWS/IoT", MetricName="Failure", Dimensions=[{"Name":"RuleName","Value":<the broken rule>}]) and cloudwatch:GetMetricStatistics(Statistics=["Sum"]) — expect a series with Sum > 0, observe no series at all. Repeat with a rule whose Lambda action ARN names a function that was never created.
