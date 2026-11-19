Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: config__lambda__sqs.change-triggered-custom-rule-is-push-invoked-with-a-real-configuration-item config__lambda__sqs.change-triggered-rule-creation-is-denied-when-config-cannot-invoke-the-evaluator
Issue-Key: config__lambda__sqs.change-triggered-custom-rule-is-push-invoked-with-a-real-configuration-item|run#70|AssertionError|assert first, (
Issue-Key: config__lambda__sqs.change-triggered-rule-creation-is-denied-when-config-cannot-invoke-the-evaluator|run#78|Timeout|time.sleep(_POLL_INTERVAL_S)
Issue-Commit: dda73b1c89ff90e3a0e7799df332912882e239a1

# config__lambda__sqs: 2 net-new causes — a change-triggered custom Config rule is never push-invoked, and PutConfigRule never answers when Config cannot invoke the evaluator

> _Note: this issue was identified against the code base at commit `dda73b1c89ff90e3a0e7799df332912882e239a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutConfigRule

## Description
2 net-new root causes in this service. Both must be closed.

Cause 1 of 2: AWS Config never push-invokes a change-triggered custom rule — only the pull path is served.
AWS: recorded behavior only (the push plane is not in the Smithy model; PutConfigRule and DescribeConfigRuleEvaluationStatus model only the control plane). With a configuration recorder on (allSupported), AWS Config records a configuration item for a changed resource and then invokes a change-triggered custom (Lambda) rule by itself: the first push evaluation lands within about 20 s of PutConfigRule, with no StartConfigRulesEvaluation call, and the evaluator's invokingEvent carries a real ConfigurationItem for the resource. DescribeConfigRuleEvaluationStatus then reports a non-empty status for the rule.
Emulator: no evaluation ever happens. In 46 s no evaluation result reached the evaluator's queue for the rule, and DescribeConfigRuleEvaluationStatus returned an empty list ('Rule evaluation status: []'). The target only serves the pull path (StartConfigRulesEvaluation), so a change-triggered rule is inert.
Why: the whole point of a change-triggered rule is that compliance is evaluated without the client asking. A client that relies on AWS's push evaluation sees a rule that exists, reports no evaluation status, and never produces a compliance verdict, so nothing detects the non-compliant resource.
Fix: config's configuration-recorder / change-notification plane and the change-triggered rule dispatcher that invokes the evaluator Lambda and records the result.
Failing check ids for this cause: config__lambda__sqs.change-triggered-custom-rule-is-push-invoked-with-a-real-configuration-item.

Cause 2 of 2: PutConfigRule never answers when the named evaluator Lambda cannot be invoked by Config.
AWS: Smithy (configservice) models PutConfigRule with the client error InsufficientPermissionsException (HTTP 400, httpError defaulted). AWS validates at PutConfigRule time that it can invoke the rule's Source.SourceIdentifier Lambda and refuses the rule outright with InsufficientPermissionsException when the function's resource policy does not permit config.amazonaws.com; the rule is not created.
Emulator: the call produces no response at all — the check is abandoned with 'Error: Check timed out after 60.0s', so neither the InsufficientPermissionsException refusal nor a success is observed.
Why: a caller gets a hung request where AWS gives an immediate, modeled 400 refusal that tells it the Lambda permission is missing; worse, nothing refuses the rule, so a rule that can never be evaluated may be left in place.
Fix: the PutConfigRule handler's evaluator-invokability precondition (and the handler must always terminate with a response).
Failing check ids for this cause: config__lambda__sqs.change-triggered-rule-creation-is-denied-when-config-cannot-invoke-the-evaluator.

## Evidence
Cause 1 — config__lambda__sqs.change-triggered-custom-rule-is-push-invoked-with-a-real-configuration-item [mismatch]:
Expected (AWS): first push evaluation ~20 s after PutConfigRule, with a real ConfigurationItem in the evaluator's invokingEvent and a non-empty DescribeConfigRuleEvaluationStatus.
Actual (emulator): 'DATAPLANE_UNIMPLEMENTED: AWS Config never invoked the change-triggered rule. In 46s no evaluation result appeared for queue <queue url> under rule <rule name>, and this check never called StartConfigRulesEvaluation. Rule evaluation status: []. Real AWS pushed the first evaluation ~20 s after PutConfigRule. A target that only serves the PULL path has nothing here'.

Cause 2 — config__lambda__sqs.change-triggered-rule-creation-is-denied-when-config-cannot-invoke-the-evaluator [mismatch]:
Expected (AWS): PutConfigRule answers HTTP 400 InsufficientPermissionsException and creates no rule.
Actual (emulator): no response; 'Error: Check timed out after 60.0s'.

## Steps to Reproduce
Cause 1:
1. PutConfigurationRecorder with recordingGroup allSupported and StartConfigurationRecorder.
2. Create a Lambda evaluator that writes its invokingEvent to an SQS queue, and grant config.amazonaws.com lambda:InvokeFunction on it.
3. PutConfigRule with a change-triggered custom rule (Source.Owner CUSTOM_LAMBDA, a ConfigurationItemChangeNotification message type) scoped to the queue's resource type.
4. Change the resource and wait. Observe: no evaluation ever reaches the queue and DescribeConfigRuleEvaluationStatus stays empty, without calling StartConfigRulesEvaluation.

Cause 2:
1. Create the same evaluator Lambda but do NOT add a resource-policy statement allowing config.amazonaws.com to invoke it.
2. Call PutConfigRule naming that function.
3. Observe: the call never returns. AWS answers HTTP 400 InsufficientPermissionsException.
