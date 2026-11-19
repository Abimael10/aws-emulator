Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.create-import-task-lifecycle logs.deploy-log-group-cfn-rename-replaces-group-and-drops-events
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.create-import-task-lifecycle|run#15|Timeout|time.sleep(5.0)
Issue-Key: logs.deploy-log-group-cfn-rename-replaces-group-and-drops-events|run#19|AssertionError|assert status == "UPDATE_COMPLETE", f"rename update ended {status!r}: {_failed_events(cfn, stack_id)!r}"
Issue-Commit: 8cbd005b36aa276ee7a2abb030c4a31aa4fd1cd2

# logs: 2 causes — CreateImportTask misclassifies a nonexistent import source as InvalidParameterException, and a CloudFormation LogGroup rename is refused by a custom-named-replacement guard on its LogStream

> _Note: this issue was identified against the code base at commit `8cbd005b36aa276ee7a2abb030c4a31aa4fd1cd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateImportTask, UpdateStack (AWS::Logs::LogGroup / AWS::Logs::LogStream)

## Description
Two independent net-new root causes in the logs service. Cause count: 2. Each cause lists its own failing check ids; both must be closed before submitting.

=== Cause 1 of 2: CreateImportTask refuses a well-formed but nonexistent importSourceArn with InvalidParameterException instead of ResourceNotFoundException ===
AWS: CreateImportTask starts an import from a CloudTrail Lake event data store. Smithy (services/service-com-amazonaws-cloudwatchlogs-logs-20140328, CreateImportTask) models both ResourceNotFoundException (HTTP 400, "The specified resource does not exist.") and InvalidParameterException (HTTP 400, "A parameter is specified incorrectly."); the operation doc states the constraint "The specified source must be in an ACTIVE state" and that the caller must have permission on the provided source. For a syntactically valid event-data-store ARN (arn:aws:cloudtrail:<region>:<account>:eventdatastore/<uuid>) that names no existing store, with an importRoleArn whose role exists and carries cloudtrail:GetEventDataStoreData plus logs:CreateLogGroup/CreateLogStream/PutResourcePolicy, AWS refuses with error code ResourceNotFoundException at HTTP 400. The MESSAGE is not a requirement for this class: AWS relays the downstream CloudTrail error text and embeds a per-request id in it, so only the status and the code are fixed.
Emulator: the request is refused with InvalidParameterException. services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/create_import_task.rs resolves the source and, on crate::state::import_source::ImportSource::Absent, returns InvalidParameterException carrying crate::state::invalid_event_data_store_arn_message(&input.import_source_arn) (the message echoes the rejected ARN back) — see the comment there, which deliberately reserves ResourceNotFoundException for "the resources this service itself owns". Because the check treats InvalidParameterException and AccessDeniedException as the transient, still-propagating IAM classes and retries CreateImportTask for up to 60 s while the code is one of those, the mandatory wrong class makes the check spin until it is killed; the replay verdict is "Check timed out after 60.0s". No later leg of the check (DescribeImportTasks / DescribeImportTaskBatches / CancelImportTask on an unknown importId, each expected to answer ResourceNotFoundException with the fixed message "No matching import found.") was reached, so nothing is claimed about those here; note CancelImportTask answering NotImplemented is already filed as cause 5 of 20261029T182816Z-logs-startquery-and-eight-other-logs-operations-accept.md.
Why: a client cannot tell a missing import source from a malformed request. InvalidParameterException/AccessDeniedException are the classes a caller is expected to retry through while a freshly created role propagates, so a permanent "source does not exist" condition is retried forever instead of surfacing immediately, and error-handling branches keyed on ResourceNotFoundException never run.
Fix: the source-resolution branch of the CreateImportTask handler (services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/create_import_task.rs) — classify an importSourceArn that resolves to no event data store as the modeled ResourceNotFoundException at HTTP 400, keeping InvalidParameterException for values that are not an event-data-store ARN at all.
Traces: logs.create-import-task-lifecycle.

=== Cause 2 of 2: changing LogGroupName is refused because the dependent custom-named AWS::Logs::LogStream must be replaced, so the whole update rolls back ===
AWS: recorded behavior only. LogGroupName is a create-only property of AWS::Logs::LogGroup, so changing it replaces the group; the dependent AWS::Logs::LogStream (whose LogGroupName is {"Ref": "LogGroup"} and whose LogStreamName is an explicit, unchanged custom name) is re-created against the new group and the stack reaches UPDATE_COMPLETE. A log stream is identified by its name WITHIN a log group, so re-creating it under the new group collides with nothing and CloudFormation's custom-named-resource guard does not apply. After the update the LogGroup PhysicalResourceId and the Fn::GetAtt Arn output are the new name, the old group is gone (reads against it answer ResourceNotFoundException "The specified log group does not exist."), the replacement group keeps retentionInDays 1, holds exactly the one stream and carries no events over.
Emulator: the update ends UPDATE_ROLLBACK_COMPLETE. The stack event is LogStream / UPDATE_FAILED with the reason "CloudFormation cannot update a stack when a custom-named resource requires replacing. Rename <log-stream-name> and update the stack again." (fixed text around one variable part: the LogStreamName from the template). The guard fires on any replacement of a resource with a user-supplied name, without checking whether the replacement would actually collide — here it cannot, because the stream moves into a different log group.
Why: a stack owner cannot rename a CloudFormation-provisioned log group whenever the stack also declares a named log stream in it — a legal template change is reported as a failed update and rolled back, so renaming automation can never converge, and the replacement semantics clients depend on (new ARN, old group deleted, no events carried over) are never exercised.
Fix: CloudFormation's update planner — scope the custom-named-resource replacement guard to replacements that would genuinely collide on the physical id, and let the AWS::Logs::LogStream resource handler re-create a named stream under a replaced parent log group. The open issue 20261119T062349Z-cloudformation-cloudwatch-logs-renaming-a-cloudformation-provisioned-aws.md records the same rename-rolls-back symptom under the service heading cloudformation__cloudwatch__logs and does NOT carry this replay key; the open issue 20261111T111216Z-cloudformation-logs-a-replacing-update-of-cloudwatch-logs-resources-is.md covers the event ORDERING/UpdateReplacePolicy of a replacement that does happen, not this refusal.
Traces: logs.deploy-log-group-cfn-rename-replaces-group-and-drops-events.

## Evidence
Cause 1 — check logs.create-import-task-lifecycle (replay verdict: mismatch, "Error: Check timed out after 60.0s").
Request: logs.CreateImportTask(importSourceArn="arn:aws:cloudtrail:<region>:<account>:eventdatastore/<uuid never created>", importRoleArn=<role created in-band with cloudtrail:GetEventDataStoreData on that ARN and logs:CreateLogGroup/CreateLogStream/PutResourcePolicy on *>).
Expected: HTTP 400 with Error.Code "ResourceNotFoundException" (message not asserted — AWS relays the CloudTrail error and embeds a per-request id).
Actual: HTTP 400 with Error.Code "InvalidParameterException" (message: the emulator's invalid-event-data-store-ARN text, which echoes the submitted ARN). Source: src/ops/create_import_task.rs returns InvalidParameterException for ImportSource::Absent. Because the check retries while the code is InvalidParameterException or AccessDeniedException for up to 60 s, the always-wrong class exhausts the budget and the check is killed at 60 s; the assertion it would otherwise raise is "expected ResourceNotFoundException for a non-existent import source, got ('InvalidParameterException', ...)".

Cause 2 — check logs.deploy-log-group-cfn-rename-replaces-group-and-drops-events (replay verdict: mismatch).
Template: AWS::Logs::LogGroup {LogGroupName: <old>, RetentionInDays: 1} + AWS::Logs::LogStream {LogGroupName: {"Ref": "LogGroup"}, LogStreamName: <stream>}, Outputs.GroupArn = Fn::GetAtt [LogGroup, Arn]. CREATE_COMPLETE and PutLogEvents/GetLogEvents against <old>/<stream> pass.
UpdateStack with only LogGroupName changed to <new>.
Expected: StackStatus "UPDATE_COMPLETE", LogGroup PhysicalResourceId == <new>, GroupArn output == the old ARN with :log-group:<old> replaced by :log-group:<new>, DescribeLogGroups(<old>) empty, GetLogEvents(<old>) -> ResourceNotFoundException "The specified log group does not exist.", DescribeLogGroups(<new>).retentionInDays == 1, streams of <new> == {<stream>}, <new>/<stream> holds no carried-over events and accepts a new PutLogEvents.
Actual: AssertionError: rename update ended 'UPDATE_ROLLBACK_COMPLETE': [('LogStream', 'UPDATE_FAILED', 'CloudFormation cannot update a stack when a custom-named resource requires replacing. Rename <log-stream-name> and update the stack again.')]. Fixed text = "CloudFormation cannot update a stack when a custom-named resource requires replacing. Rename " + <LogStreamName from the template> + " and update the stack again."; the name is the only request-derived part.

## Steps to Reproduce
Cause 1:
1. iam.CreateRole trusting cloudtrail.amazonaws.com and PutRolePolicy granting cloudtrail:GetEventDataStoreData on arn:aws:cloudtrail:<region>:<account>:eventdatastore/<fresh uuid> plus logs:CreateLogGroup, logs:CreateLogStream, logs:PutResourcePolicy on *.
2. logs.CreateImportTask(importSourceArn=that event-data-store ARN, importRoleArn=the role ARN).
3. Observe Error.Code InvalidParameterException; AWS answers ResourceNotFoundException (HTTP 400). Any retry loop that treats InvalidParameterException/AccessDeniedException as transient IAM propagation will never terminate.

Cause 2:
1. cloudformation.CreateStack with Resources = {LogGroup: AWS::Logs::LogGroup {LogGroupName: <old>, RetentionInDays: 1}, LogStream: AWS::Logs::LogStream {LogGroupName: {"Ref": "LogGroup"}, LogStreamName: <stream>}} and Outputs.GroupArn = Fn::GetAtt [LogGroup, Arn]; wait for CREATE_COMPLETE.
2. logs.PutLogEvents one event into <old>/<stream> and read it back.
3. cloudformation.UpdateStack with the same template but LogGroupName = <new>; poll DescribeStacks.
4. Observe UPDATE_ROLLBACK_COMPLETE with the LogStream UPDATE_FAILED custom-named-replacement reason; AWS reaches UPDATE_COMPLETE, the LogGroup physical id and GroupArn become <new>, the old group is deleted, and <new> holds the re-created stream with no carried-over events.
