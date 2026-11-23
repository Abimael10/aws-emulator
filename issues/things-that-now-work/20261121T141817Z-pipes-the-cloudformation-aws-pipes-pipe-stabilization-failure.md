Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pipes.deploy-cfn-pipe-whose-role-cannot-read-its-source-fails-the-stack-and-rolls-back
Issue-Shape-Id: com.amazonaws.pipes#Pipes
Issue-Key: pipes.deploy-cfn-pipe-whose-role-cannot-read-its-source-fails-the-stack-and-rolls-back|run#16|AssertionError|assert tail.endswith(", HandlerErrorCode: NotStabilized)"), (
Issue-Commit: 4aa4e6ed21a2abfb110c20fb2864559e28b54b24

# pipes: the CloudFormation AWS::Pipes::Pipe stabilization failure is classified HandlerErrorCode: GeneralServiceException instead of NotStabilized

> _Note: this issue was identified against the code base at commit `4aa4e6ed21a2abfb110c20fb2864559e28b54b24`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
This bundle has 1 net-new root cause of service `pipes`; it must be closed before submitting. It is grounded in a failing behavioral check of this run that the replay report did NOT mark ALREADY FILED and did NOT mark OWNED by an open issue.

Related open issue that does NOT cover this cause: 20261111T093805Z-pipes-4-net-new-causes-pipe-failure-statereason.md. Its Cause 1 and Cause 3 are about the pipe's own StateReason wording and about the `Resource handler returned message: "...did not stabilize. Status Reason is ..."` envelope being absent entirely, and it carries this check under the replay key `assert head == expected_head and sep, (`. In this run that assertion PASSES: the envelope and the inner AWS StateReason are now produced verbatim. The surviving divergence is a different assertion (the trailing handler-error-code classification, replay key `assert tail.endswith(", HandlerErrorCode: NotStabilized)"), (`), which that issue never describes.

Cause 1 of 1: the CREATE_FAILED stack event for an AWS::Pipes::Pipe that never reached RUNNING is tagged with the wrong HandlerErrorCode.
AWS:      recorded behavior only — the CloudFormation resource provider for AWS::Pipes::Pipe is not in any Smithy model; the pipes model only supplies what the provider reads (Smithy com.amazonaws.pipes DescribePipe output members CurrentState: PipeState and StateReason: PipeStateReason, @length 0..512, pattern ^.*$). When the declared pipe cannot stabilize, the stack emits exactly one CREATE_FAILED StackEvent for that logical resource (create path; UPDATE_FAILED on the update path) whose ResourceStatusReason is
          Resource handler returned message: "Resource of type 'AWS::Pipes::Pipe' with identifier '<pipe-name>' did not stabilize. Status Reason is <pipe StateReason>" (RequestToken: <token>, HandlerErrorCode: NotStabilized)
          Everything outside the angle brackets is FIXED text of this error class and required verbatim — including the `, HandlerErrorCode: NotStabilized)` suffix inside the same parenthesis as the request token. The variable parts come from the request and from state: <pipe-name> is the template's pipe Name property, <pipe StateReason> is the pipe's own StateReason, and <token> is a per-run request token that is NOT part of the requirement. The CODE is not variable: a resource that was accepted but never reached its stable state is classified NotStabilized, not GeneralServiceException (which is the classification for an unexpected service-side error during the call itself).
Emulator: the envelope, the identifier, the `did not stabilize. Status Reason is ` join and the inner pipes StateReason are all correct, but the parenthesised suffix ends `, HandlerErrorCode: GeneralServiceException)`. Observed reason (per-run names/tokens are this recording's values, not requirements):
          Resource handler returned message: "Resource of type 'AWS::Pipes::Pipe' with identifier '<pipe-name>' did not stabilize. Status Reason is Input parameter is invalid from the request due to : The provided customer role does not have permissions to call ReceiveMessage on SQS" (RequestToken: <token>, HandlerErrorCode: GeneralServiceException)
Why:      HandlerErrorCode is the machine-readable classification a client uses to decide what a stack failure means. NotStabilized tells tooling the resource was created but never converged — retry/extend-the-timeout/inspect-the-resource is the right response, and CDK, Cloud Control API callers and CI gates branch on exactly this token. GeneralServiceException says the API call itself failed unexpectedly, so a client sees a transient service fault where AWS reports a convergence failure: retry loops, rollback-classification dashboards and alarms keyed on NotStabilized all mis-route this stack, even though the status (ROLLBACK_COMPLETE), the single CREATE_FAILED event and the reason text are otherwise right.
Fix:      the CloudFormation AWS::Pipes::Pipe resource provider's stabilization-failure reporting (create and update paths) — the same component as Cause 3 of 20261111T093805Z-pipes-4-net-new-causes-pipe-failure-statereason.md, which now formats the envelope correctly: emit HandlerErrorCode: NotStabilized when the resource was provisioned but never reached its terminal stable state, reserving GeneralServiceException for failures of the handler call itself.
Traces:   pipes.deploy-cfn-pipe-whose-role-cannot-read-its-source-fails-the-stack-and-rolls-back (create path; the companion update-path check pipes.deploy-cfn-pipe-update-to-a-role-without-source-read-fails-and-rolls-back-to-the-working-pipe is not in this run's failing list, so only the create path is grounded here — a fix should keep both paths consistent).

## Evidence
Behavioral suite (simparity-behavioral-aws-v1), this run: Total 160/171 passed (93.6%); Mismatched 11, Errored 0, Skipped 0, Unloadable 0. No bootstrap signal — the emulator was serving: pipes 91/96 passed, and inside this very check SQS CreateQueue/SendMessage/ReceiveMessage, IAM CreateRole/PutRolePolicy, CloudFormation CreateStack/DescribeStacks/DescribeStackEvents and pipes CreatePipe/DescribePipe all answered.

pipes.deploy-cfn-pipe-whose-role-cannot-read-its-source-fails-the-stack-and-rolls-back:
  File "check_pipes_deploy_cfn_pipe_whose_role_cannot_read_its_source_fails_the_stack_and_rolls_back.py", line 62, in run
    assert tail.endswith(", HandlerErrorCode: NotStabilized)"), (
  AssertionError: ThePipe's failure must be classified NotStabilized, reason was 'Resource handler returned message: "Resource of type \'AWS::Pipes::Pipe\' with identifier \'sim2real-pipes-cfn-pipe-71d285ccc36a\' did not stabilize. Status Reason is Input parameter is invalid from the request due to : The provided customer role does not have permissions to call ReceiveMessage on SQS" (RequestToken: 5d172e00-c561-46fe-2a66-cd396359edc4, HandlerErrorCode: GeneralServiceException)'

Expected (AWS): the identical reason ending ', HandlerErrorCode: NotStabilized)'.
Actual (emulator): the identical reason ending ', HandlerErrorCode: GeneralServiceException)'.

Fixed text of the class (required verbatim): 'Resource handler returned message: "Resource of type \'AWS::Pipes::Pipe\' with identifier \'', '\' did not stabilize. Status Reason is ', '" (RequestToken: ', ', HandlerErrorCode: NotStabilized)'.
Values from the request/state (NOT requirements, filled per run): the pipe name 'sim2real-pipes-cfn-pipe-71d285ccc36a' (the template's ThePipe Name), the inner Status Reason (the pipe's own StateReason), and the request token '5d172e00-c561-46fe-2a66-cd396359edc4'.
The check's earlier assertions passed in this run: StackStatus reached ROLLBACK_COMPLETE, exactly one CREATE_FAILED event exists for LogicalResourceId 'ThePipe', and the reason head (envelope + identifier + the AWS pipe StateReason) matched — only the HandlerErrorCode token diverges.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite (run_behavioral_suite).
2. CreateStack with Capabilities=[CAPABILITY_IAM] and a template declaring: SourceQueue and TargetQueue (AWS::SQS::Queue); PipeRole (AWS::IAM::Role trusting pipes.amazonaws.com) with a policy granting ONLY sqs:SendMessage on TargetQueue (no sqs:ReceiveMessage on SourceQueue); ThePipe (AWS::Pipes::Pipe) with Name=<pipe name>, RoleArn=PipeRole.Arn, Source=SourceQueue.Arn, SourceParameters={SqsQueueParameters:{BatchSize:1}}, Target=TargetQueue.Arn, DesiredState=RUNNING.
3. Poll DescribeStacks until the status is not *_IN_PROGRESS; it reaches ROLLBACK_COMPLETE (correct).
4. Paginate DescribeStackEvents for the stack and select the event with LogicalResourceId='ThePipe' and ResourceStatus='CREATE_FAILED'; read ResourceStatusReason.
5. Split it on ' (RequestToken: '. The head matches AWS. Observe the tail ends ', HandlerErrorCode: GeneralServiceException)' instead of AWS's ', HandlerErrorCode: NotStabilized)'.
6. After fixing, re-replay pipes.deploy-cfn-pipe-whose-role-cannot-read-its-source-fails-the-stack-and-rolls-back.
