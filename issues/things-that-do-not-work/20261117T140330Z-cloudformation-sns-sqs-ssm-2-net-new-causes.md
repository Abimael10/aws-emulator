Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sns__sqs__ssm.explicit-queue-name-collision-rejected-by-the-resource-handler cloudformation__sns__sqs__ssm.generated-resource-names-resolve-at-owning-service-and-release-on-delete
Issue-Key: cloudformation__sns__sqs__ssm.explicit-queue-name-collision-rejected-by-the-resource-handler|_reason#11|AssertionError|assert len(rows) == 1, f"expected one {status} row for {logical_id}, got {rows!r}"
Issue-Key: cloudformation__sns__sqs__ssm.generated-resource-names-resolve-at-owning-service-and-release-on-delete|run#26|AssertionError|assert m and _SUFFIX.match(m.group(1)), (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# cloudformation__sns__sqs__ssm: 2 net-new causes — a rolled-back stack records no CREATE_FAILED event for the resource that failed, and CFN-generated physical names do not use AWS's CFN-<logical>-<suffix> form

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Two independent net-new root causes under the cloudformation__sns__sqs__ssm heading. A fixer must close both before submitting.

Cause 1 of 2: a stack that rolls back because a resource handler failed records no CREATE_FAILED resource event for that resource.
AWS: recorded behavior only (stack events are not modeled members). When a resource handler fails during create, AWS writes exactly one CREATE_FAILED stack event for that logical resource whose ResourceStatusReason is the handler wrapper 'Resource handler returned message: "<service message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)' — for an AWS::SQS::Queue whose explicit QueueName is already taken by a queue with different attributes the pair is ("Queue creation failed because the queue already exists", "AlreadyExists") — and only then does the stack proceed to ROLLBACK_COMPLETE.
Emulator: the stack does reach ROLLBACK_COMPLETE, but DescribeStackEvents contains NO CREATE_FAILED event for the failing logical resource at all (the check's filter returns an empty list), so the handler message and HandlerErrorCode are nowhere in the event stream.
Why: the resource event and its reason are the only place a client learns WHY a stack rolled back and WHICH resource failed; without it a CI pipeline or the CDK sees a bare ROLLBACK_COMPLETE and cannot report, classify or retry the failure.
Fix: the stack-event recording in the cloudformation service crate's create/rollback path — emit the per-resource CREATE_FAILED event with the handler reason before transitioning the stack to rollback.
Traces: cloudformation__sns__sqs__ssm.explicit-queue-name-collision-rejected-by-the-resource-handler.

Cause 2 of 2: CloudFormation-generated physical resource names do not follow AWS's generated-name form.
AWS: recorded behavior only. When a template omits the name property, CloudFormation generates the physical name as "CFN-<LogicalResourceId>-<12-character mixed-case alphanumeric suffix>" for an AWS::SSM::Parameter — the fixed parts are the "CFN-" prefix, the logical id, and a 12-character [A-Za-z0-9] suffix; only the suffix varies per resource.
Emulator: the generated name is "<stack-name>-<LogicalResourceId>-<12 upper-case hex chars>" (e.g. a name of the form sim2real-behav-stk-<id>-AutoParameter-<HEX12>), which has no CFN- prefix, embeds the stack name and uses a different suffix alphabet.
Why: a client that resolves a generated name at the owning service, or that asserts/filters on the documented generated-name shape, matches nothing; names also inherit the stack name's length, so long stack names push generated names past service name limits that AWS's form would not.
Fix: the physical-id generator for unnamed resources in the cloudformation service crate.
Traces: cloudformation__sns__sqs__ssm.generated-resource-names-resolve-at-owning-service-and-release-on-delete.

## Evidence
Behavioral suite, service heading `### cloudformation__sns__sqs__ssm`.

Cause 1 — cloudformation__sns__sqs__ssm.explicit-queue-name-collision-rejected-by-the-resource-handler:
the preceding assertion (StackStatus == 'ROLLBACK_COMPLETE') passes, then
  File "...explicit_queue_name_collision...py", line 95, in run: got = _handler_reason(_reason(cf, bad_stack, "Queue"))
  AssertionError: expected one CREATE_FAILED row for Queue, got []
where _reason filters DescribeStackEvents for LogicalResourceId == "Queue" and ResourceStatus == "CREATE_FAILED". The expected reason pair is ("Queue creation failed because the queue already exists", "AlreadyExists").

Cause 2 — cloudformation__sns__sqs__ssm.generated-resource-names-resolve-at-owning-service-and-release-on-delete:
AssertionError: AutoParameter name 'sim2real-behav-stk-9537933210c9-AutoParameter-50C3396454E5' does not match 'CFN-AutoParameter-<12 char mixed-case alnum suffix>'
(the check's suffix pattern is ^[A-Za-z0-9]{12}$; the stack name and suffix in the observed value are per-run, the CFN- prefix + logical id + 12-char suffix shape is the requirement).

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. Cause 1: CreateQueue a queue with VisibilityTimeout 45, then CreateStack a template whose single AWS::SQS::Queue declares that same QueueName with VisibilityTimeout 99. Wait for ROLLBACK_COMPLETE, then DescribeStackEvents and look for a CREATE_FAILED event with LogicalResourceId "Queue": none exists. AWS records exactly one, with ResourceStatusReason 'Resource handler returned message: "Queue creation failed because the queue already exists" (RequestToken: <uuid>, HandlerErrorCode: AlreadyExists)'.
3. Cause 2: CreateStack a template with an AWS::SSM::Parameter that declares no Name. Read the resource's PhysicalResourceId: it is "<stack>-<logical>-<HEX12>" instead of "CFN-<logical>-<12-char mixed-case alnum>".
4. Re-replay the 2 checks listed in trace_ids.
