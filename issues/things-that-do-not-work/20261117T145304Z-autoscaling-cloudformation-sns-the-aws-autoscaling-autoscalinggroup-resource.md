Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-invalid-notification-type-rejected autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-round-trip-update-and-removal
Issue-Key: autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-invalid-notification-type-rejected|_asg_handler_error#16|AssertionError|assert inner, f"handler message had an unexpected shape: {outer['msg']!r}"
Issue-Key: autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-round-trip-update-and-removal|run#14|AssertionError|assert status == "CREATE_COMPLETE", f"stack {stack_id} create ended {status!r}"
Issue-Commit: 6df92bd208b59385c9370bd54f69012d5d2b8998

# autoscaling__cloudformation__sns: the AWS::AutoScaling::AutoScalingGroup resource handler never provisions NotificationConfigurations, so the property fails the resource with emulator-authored prose and rolls the stack back

> _Note: this issue was identified against the code base at commit `6df92bd208b59385c9370bd54f69012d5d2b8998`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
ONE net-new root cause, grounded in both failing checks of the suite's `autoscaling__cloudformation__sns` service heading. Neither is marked ALREADY FILED or OWNED by the replay report.

Cause 1 of 1: CloudFormation's AWS::AutoScaling::AutoScalingGroup resource handler does not hand the template's NotificationConfigurations property to Auto Scaling at all.
Failing check ids: autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-invalid-notification-type-rejected, autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-round-trip-update-and-removal

AWS: AWS::AutoScaling::AutoScalingGroup's NotificationConfigurations is provisioned by the resource handler as a separate Auto Scaling call per entry (PutNotificationConfiguration with the group name, the entry's TopicARN and its NotificationTypes) made after CreateAutoScalingGroup. With a modelled notification type (e.g. autoscaling:TEST_NOTIFICATION) the stack reaches CREATE_COMPLETE and autoscaling:DescribeNotificationConfigurations for that group lists one (TopicARN, NotificationType) pair per declared type. With a type outside the enum the ASG resource reaches CREATE_FAILED and its ResourceStatusReason is the handler-wrapped service refusal: `Resource handler returned message: "<service message> (Service: AutoScaling, Status Code: 400, Request ID: <request id>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)`, where <service message> is Auto Scaling's own ValidationError text `"<rejected type>" is not a valid Notification Type.` — fixed text is the quoted value followed by ` is not a valid Notification Type.`, and the only variable part is the rejected type copied from the request; the request token, request id and attempt count are per-call values. Because the group itself is created by a different call than the notification configuration, the group still exists after that failure and simply has no notification configuration. Recorded behavior only for the wrapper shape (Smithy models no text for these reasons); the status/code class is HTTP 400 ValidationError inside HandlerErrorCode InvalidRequest.

Emulator: the handler refuses the property instead of forwarding it. The invalid-type stack's ASG CREATE_FAILED reason is the emulator's own sentence — `Resource handler returned message: "this emulator does not send AWS::AutoScaling::AutoScalingGroup's NotificationConfigurations property to the service that owns the type, so the resource was not created" (...)` — which carries no `(Service: AutoScaling, Status Code: 400, Request ID: ...) (SDK Attempt Count: ...)` SDK envelope and no Auto Scaling message, and it announces that the property is dropped rather than validated. The same gap fails the positive case: the identical template with the modelled type autoscaling:TEST_NOTIFICATION ends ROLLBACK_COMPLETE instead of CREATE_COMPLETE, so no group and no notification configuration are ever created.

Why: a client that deploys an Auto Scaling group with notifications through CloudFormation cannot deploy at all — a template AWS accepts rolls back. For the negative case, the failure reason is the channel CloudFormation uses to report WHICH property value the owning service rejected; prose that names the emulator instead of the service defeats every consumer that parses the handler wrapper (console, CLI, CDK, drift/failure triage) and hides the real validation rule, and it wrongly implies the group was not created when AWS leaves the group in place.

Fix: the CloudFormation resource provisioner for AWS::AutoScaling::AutoScalingGroup — provision each NotificationConfigurations entry through the Auto Scaling PutNotificationConfiguration path (shared Auto Scaling state that DescribeNotificationConfigurations reads) after group creation, and surface that call's refusal through the standard handler-wrapper/SDK-envelope formatter instead of an emulator-authored sentence. Related OPEN issue, named for context only and not covering these checks (the report marks neither as OWNED): 20261109T172356Z-autoscaling-cloudformation-2-net-new-causes-describestacks-outputs.md (stack-created ASG invisible to DescribeAutoScalingGroups, stack Outputs missing OutputValue).

Traces: autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-invalid-notification-type-rejected (the reason text), autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-round-trip-update-and-removal (the valid template rolling back).

## Evidence
Check autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-invalid-notification-type-rejected:
  AssertionError: handler message had an unexpected shape: "this emulator does not send AWS::AutoScaling::AutoScalingGroup's NotificationConfigurations property to the service that owns the type, so the resource was not created"
  (raised by _asg_handler_error after the outer `Resource handler returned message: "..." (RequestToken: <uuid>, HandlerErrorCode: <code>)` wrapper matched but the inner `<text> (Service: AutoScaling, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)` envelope did not)
  Expected: HandlerErrorCode InvalidRequest and inner message '"autoscaling:NOT_A_REAL_TYPE" is not a valid Notification Type.'
  Actual: no SDK envelope at all; the emulator's own sentence about not forwarding the property.

Check autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-round-trip-update-and-removal:
  AssertionError: stack arn:aws:cloudformation:<region>:<account>:stack/<name>/<id> create ended 'ROLLBACK_COMPLETE'
  Expected: CREATE_COMPLETE for a template whose ASG declares NotificationConfigurations with a modelled NotificationType, then DescribeNotificationConfigurations listing that (TopicARN, NotificationType) pair, surviving an update and removal.
  Actual: the create rolls back, so no group and no notification configuration exist.

## Steps to Reproduce
1. sns:CreateTopic -> <topic arn>. ec2:CreateLaunchTemplate (or let the template create one) and pick a default subnet.
2. cloudformation:CreateStack with a template whose AWS::AutoScaling::AutoScalingGroup has MinSize/MaxSize/DesiredCapacity 0, a LaunchTemplate reference, VPCZoneIdentifier, and Properties.NotificationConfigurations = [{"TopicARN": "<topic arn>", "NotificationTypes": ["autoscaling:NOT_A_REAL_TYPE"]}], DisableRollback=true.
3. Poll DescribeStacks until terminal: CREATE_FAILED. Read the ASG resource's CREATE_FAILED ResourceStatusReason from DescribeStackEvents. EXPECT `Resource handler returned message: "\"autoscaling:NOT_A_REAL_TYPE\" is not a valid Notification Type. (Service: AutoScaling, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)`. OBSERVE the emulator sentence "this emulator does not send AWS::AutoScaling::AutoScalingGroup's NotificationConfigurations property to the service that owns the type, so the resource was not created". Also EXPECT the group itself to exist (DescribeAutoScalingGroups returns it) with DescribeNotificationConfigurations == [].
4. CreateStack the identical template with NotificationTypes ["autoscaling:TEST_NOTIFICATION"]. EXPECT CREATE_COMPLETE and DescribeNotificationConfigurations == [(<topic arn>, "autoscaling:TEST_NOTIFICATION")]. OBSERVE ROLLBACK_COMPLETE.
Replay: autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-invalid-notification-type-rejected, autoscaling__cloudformation__sns.deploy-cfn-asg-notification-configuration-round-trip-update-and-removal
