Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: config.e2e-remediation-auto-execution-consequence config.remediation-exceptions-lifecycle
Issue-Shape-Id: com.amazonaws.configservice#StarlingDoveService
Issue-Key: config-service|PutRemediationConfigurations|InvalidParameterValueException
Issue-Commit: b77f722407769a9331a26c91bcc6a0d2954d312c

# config PutRemediationConfigurations refuses the AWS-owned SSM Automation document AWS-StartEC2Instance as a missing remediation target, so no remediation configuration can be created and the auto-execution and remediation-exception lifecycle flows never start

> _Note: this issue was identified against the code base at commit `b77f722407769a9331a26c91bcc6a0d2954d312c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRemediationConfigurations

## Description
One net-new root cause on the AWS Config surface, reached by two checks the suite lists under `### config`. Neither check was marked ALREADY FILED or OWNED by the replay report, so neither carries an open issue's replay key.

=== Cause 1 of 1: PutRemediationConfigurations treats an AWS-owned (`AWS-*`) SSM Automation document as a non-existent remediation target and refuses the write ===
Failing checks: config.e2e-remediation-auto-execution-consequence, config.remediation-exceptions-lifecycle

AWS: Smithy doc, PutRemediationConfigurations — "Adds or updates the remediation configuration with a specific Config rule with the selected target or action. ... The Config rule must already exist for you to add a remediation configuration. The target (SSM document) must exist and have permissions to use the target." Smithy signature: PutRemediationConfigurations(RemediationConfigurations @required, list @length 0..25) -> {FailedBatches: FailedRemediationBatches}; modeled errors are InsufficientPermissionsException and InvalidParameterValueException, both client / HTTP 400. AWS-owned public Automation documents — `AWS-StartEC2Instance`, `AWS-EnableS3BucketVersioning`, `AWS-ConfigureS3BucketLogging` and the rest of the `AWS-*` family — exist in every account and Region, so the target-existence gate passes and AWS answers HTTP 200 with `{"FailedBatches": []}` for an entry whose TargetType is `SSM_DOCUMENT` and TargetId is `AWS-StartEC2Instance`. AWS's refusal for a target that genuinely does not exist is HTTP 400, Error.Code `InvalidParameterValueException`, with a two-sentence template: fixed text `The remediation target for ` + <ConfigRuleName taken from the request entry> + ` could not be found. Specify the appropriate remediation target and try again.` The rule name is the only variable part; the id in the recorded line below is a per-run value, not part of the requirement.

Emulator: that missing-target refusal is returned for `AWS-StartEC2Instance` — i.e. for an AWS-managed Automation document that does exist. Recorded response: HTTP 400, Error.Code `InvalidParameterValueException`, Error.Message `The remediation target for sim2real-remrule-99f0f72ec7fd could not be found. Specify the appropriate remediation target and try again.` The wording and class are AWS's correct text for the missing-target class — the defect is the PREDICATE: it is applied to a request AWS accepts. Hypothesis: the target-existence check consults only customer-created SSM documents, and the emulator's SSM document catalog holds no AWS-owned (`AWS-*`) Automation documents.

Why: the documented remediation path for AWS Config is an AWS-managed SSM Automation document. With this gate, a client can create NO remediation configuration at all against the emulator — manual remediation (StartRemediationExecution), automatic remediation (`Automatic: true` with ExecutionControls), DescribeRemediationConfigurations round-trips, and every remediation-exception workflow (PutRemediationExceptions refuses a rule that has no remediation configuration) are all unreachable. The client is told a document that exists "could not be found", so correct client code cannot recover: retrying, re-creating the role, or re-creating the rule never clears it.

Fix: the PutRemediationConfigurations handler's remediation-target resolution in the Config service, and the shared SSM-document lookup it calls — AWS-owned Automation documents must resolve as existing (seed the SSM document catalog with the `AWS-*` Automation documents Config remediation uses, or accept an `AWS-` prefixed SSM_DOCUMENT TargetId whose version resolves). The refusal must remain, with the message template above unchanged, for a TargetId that resolves to nothing — the emulator already gets that negative case right elsewhere, so only the positive case needs to start passing.

Related but NOT covering: open issue 20261110T200127Z-config-4-net-new-behavioral-root-causes-remediation.md describes this same emulator predicate as its Cause 2, but it carries only the replay key of config.remediation-exceptions-empty-vs-absent-message-preserved; the replay report marked neither of the two checks below as owned by it, so they are not covered by it and are recorded here. Fixing the predicate once should clear all three checks.

Traces: config.e2e-remediation-auto-execution-consequence (the refusal surfaces directly as the client error); config.remediation-exceptions-lifecycle (the same Put is retried until the run budget is gone).

## Evidence
Cause 1 — config.e2e-remediation-auto-execution-consequence, verdict [error].
The check creates an IAM role assumable by ssm.amazonaws.com, launches and stops an EC2 instance, puts a Config rule scoped to AWS::EC2::Instance, and then calls PutRemediationConfigurations with one entry:
  {ConfigRuleName: <rule>, TargetType: "SSM_DOCUMENT", TargetId: "AWS-StartEC2Instance",
   Parameters: {"InstanceId": {"ResourceValue": {"Value": "RESOURCE_ID"}},
                "AutomationAssumeRole": {"StaticValue": {"Values": [<role arn>]}}},
   ResourceType: "AWS::EC2::Instance", Automatic: <true/false>}
Expected (AWS): HTTP 200, body {"FailedBatches": []}.
Actual (emulator): HTTP 400, Error.Code="InvalidParameterValueException".
Replay output:
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.InvalidParameterValueException: An error occurred (InvalidParameterValueException) when calling the PutRemediationConfigurations operation: The remediation target for sim2real-remrule-99f0f72ec7fd could not be found. Specify the appropriate remediation target and try again.
Message anatomy: `The remediation target for ` and ` could not be found. Specify the appropriate remediation target and try again.` are AWS's FIXED text for the missing-target class; the ConfigRuleName from the request is the single variable part (`sim2real-remrule-99f0f72ec7fd` above is a per-run generated name and is not itself a requirement). Because the Put is refused, nothing after it in the check runs: no StartRemediationExecution, no SUCCEEDED execution status, no instance transition to running, and no DeleteRemediationConfiguration consequence.

Cause 1, second trace — config.remediation-exceptions-lifecycle, verdict [error].
The check reaches the identical call through its helper, which retries only while the service refuses:
  def _put_remediation_configuration_when_role_ready(config, rule_name, role_arn, *, timeout_s=90.0, interval_s=5.0, ctx):
      remediation_configuration = {"ConfigRuleName": rule_name, "TargetType": "SSM_DOCUMENT",
          "TargetId": "AWS-StartEC2Instance",
          "Parameters": {"InstanceId": {"ResourceValue": {"Value": "RESOURCE_ID"}},
                         "AutomationAssumeRole": {"StaticValue": {"Values": [role_arn]}}},
          "ResourceType": "AWS::EC2::Instance", "Automatic": False}
      while True:
          try:
              resp = config.put_remediation_configurations(RemediationConfigurations=[remediation_configuration])
              ... return
          except ClientError:
              if time.monotonic() >= deadline: ...
              time.sleep(interval_s)
Expected (AWS): the Put succeeds (200, FailedBatches: []) on the first attempt once the role has propagated, and the check goes on to PutRemediationExceptions / DescribeRemediationExceptions / DeleteRemediationExceptions and the NoSuchRemediationExceptionException contract.
Actual (emulator):
  File "check_config_remediation_exceptions_lifecycle.py", line 64, in run
    _put_remediation_configuration_when_role_ready(
  File "check_config_remediation_exceptions_lifecycle.py", line 420, in _put_remediation_configuration_when_role_ready
    raise TimeoutError("the remediation role's propagation: the run budget ended first; not graded") from None
  TimeoutError: the remediation role's propagation: the run budget ended first; not graded
The loop exits only on success or on a ClientError at the deadline, so the budget was consumed re-issuing a Put that kept being refused. Hypothesis (the refusal body is not echoed in this check's output): it is the same InvalidParameterValueException missing-target refusal shown verbatim above, since it is the same operation with the same `AWS-StartEC2Instance` SSM_DOCUMENT target. The harness itself labels this trace "not graded" — it is included as a second example of the one cause, not as independent grounding; the refusal above is the grounding.

## Steps to Reproduce
1. PutConfigRule a rule scoped to AWS::EC2::Instance (e.g. Source {Owner: "AWS", SourceIdentifier: "REQUIRED_TAGS"} with Scope.ComplianceResourceTypes=["AWS::EC2::Instance"]) and wait for ConfigRuleState ACTIVE.
2. CreateRole with a trust policy allowing ssm.amazonaws.com to sts:AssumeRole, for use as AutomationAssumeRole.
3. Call PutRemediationConfigurations with a single entry: ConfigRuleName=<rule from step 1>, TargetType="SSM_DOCUMENT", TargetId="AWS-StartEC2Instance", ResourceType="AWS::EC2::Instance", Automatic=false, Parameters={"InstanceId": {"ResourceValue": {"Value": "RESOURCE_ID"}}, "AutomationAssumeRole": {"StaticValue": {"Values": [<role arn from step 2>]}}}.
4. AWS answers HTTP 200 with {"FailedBatches": []}; the emulator answers HTTP 400 InvalidParameterValueException "The remediation target for <ConfigRuleName> could not be found. Specify the appropriate remediation target and try again." Retrying does not clear it.
5. Confirm the negative case still refuses after the fix: repeat step 3 with TargetId="sim2real-behav-NoSuchAutomationDocument" and check the same 400 / InvalidParameterValueException / same message template is returned.
6. Re-replay config.e2e-remediation-auto-execution-consequence and config.remediation-exceptions-lifecycle; the first must reach its StartRemediationExecution → SUCCEEDED / instance-running assertions and the DeleteRemediationConfiguration consequence, the second must reach its PutRemediationExceptions / DescribeRemediationExceptions / NoSuchRemediationExceptionException assertions.
