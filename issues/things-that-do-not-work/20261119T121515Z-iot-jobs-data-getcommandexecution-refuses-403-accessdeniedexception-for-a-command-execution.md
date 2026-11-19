Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot-jobs-data.start-command-execution-client-token-idempotent-and-absent-target-over-permissive
Issue-Shape-Id: com.amazonaws.iotjobsdataplane#IotLaserThingJobManagerExternalService
Issue-Key: iot-jobs-data.start-command-execution-client-token-idempotent-and-absent-target-over-permissive|_poll#26|Timeout|time.sleep(interval_s)
Issue-Commit: d1977ef89f0c5c4dfc7f04e8d3c082237c1f7677

# GetCommandExecution refuses 403 AccessDeniedException for a command execution whose target thing is not in the registry, so an absent-target execution StartCommandExecution accepted is never readable and never observed TIMED_OUT

> _Note: this issue was identified against the code base at commit `d1977ef89f0c5c4dfc7f04e8d3c082237c1f7677`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandExecution

## Description
One root cause, grounded in the single surviving failing check of this service.

Cause 1 of 1: a command execution started against a targetArn whose thing is NOT in the IoT thing registry is accepted and persisted by iot-jobs-data StartCommandExecution, but the iot control plane's GetCommandExecution refuses every read of it as an authorization failure instead of returning the record, so the execution can never be read back and its timeout transition to TIMED_OUT is never observable. Failing check: iot-jobs-data.start-command-execution-client-token-idempotent-and-absent-target-over-permissive.

AWS: StartCommandExecution is over-permissive about its target — a targetArn naming a thing the registry does not hold is ACCEPTED (200, an executionId is minted), and the recorded behavior is that the execution is then a first-class record: GetCommandExecution(executionId, targetArn=<absent thing's ARN>) answers 200 with that execution, echoes targetArn verbatim, and — because no device can ever pick the work up — the executionTimeoutSeconds timer moves it to the terminal status TIMED_OUT (Smithy: StartCommandExecution$executionTimeoutSeconds — "A timer is started as soon as the command execution is created. If the command execution status is not set to another terminal state before the timer expires, it will automatically update to TIMED_OUT"; CommandExecutionStatus = CREATED | FAILED | IN_PROGRESS | REJECTED | SUCCEEDED | TIMED_OUT). GetCommandExecution models exactly four errors — InternalServerException (500), ResourceNotFoundException (404), ThrottlingException (400), ValidationException (400); it models NO access-denied error at all, so a 403 AccessDeniedException is not an answer this operation has. Accepting the start must not create a thing in the registry either (ListThings must still not report it), which is why the reader cannot be gated on the registry.

Emulator: GetCommandExecution resolves targetArn against this account's THING REGISTRY before it reads the execution store, and a well-formed thing ARN of this account whose name the registry does not hold is classified NotInAccount and answered HTTP 403 AccessDeniedException with the fixed message "targetArn not found in account" (no variable part). The status, the error code and the message are all wrong for this request: AWS answers 200 with the execution document. Because the refusal is permanent — nothing ever puts the ghost thing into the registry — the check's poll for a terminal status never advances and the check hangs until the harness kills it at 60s ("Check timed out after 60.0s"). The execution does exist: StartCommandExecution minted and inserted the JobExecutionRecord for the thing name parsed out of the ARN and armed its timeout deadline, and ListCommandExecutions does project records of things the registry does not hold, so the single-execution reader and the lister disagree about the same record.

Why: a client that uses the documented absent-target flow (fire a command at a device that has not been registered yet, then poll for its outcome) gets a 403 for an execution it holds a valid id for. It can neither read the execution's status, statusReason, parameters or timeToLive, nor ever see it reach TIMED_OUT, nor delete it (DeleteCommandExecution classifies targetArn the same way), so the record is permanently unreachable and the 403 misreports a data question as a permissions problem — SDK retry/credential-refresh logic reacts to it instead of surfacing the execution.

Fix: the targetArn resolution shared by the command-execution readers must stop requiring registry membership — only the ARN's form, service, partition and account decide which device a command-execution request names; whether that device exists is the EXECUTION store's question, and an executionId that names no execution of that thing is the modeled 404 ResourceNotFoundException. The gate lives in the iot crate's command-execution target resolution and the GetCommandExecution handler that maps its refusal (see Traces for the files); DeleteCommandExecution shares the same resolution and must change with it.

Traces: iot-jobs-data.start-command-execution-client-token-idempotent-and-absent-target-over-permissive. Code: services/service-com-amazonaws-iot-awsiotservice/src/state/command_execution_access.rs lines 96-105 (command_execution_device returns NotInAccount when self.things does not contain the parsed thing name) and services/service-com-amazonaws-iot-awsiotservice/src/ops/get_command_execution.rs lines 128-141 and 248-251 (NotInAccount is mapped to the 403 access-denied answer before the execution store is read); services/service-com-amazonaws-iotjobsdataplane-iotlaserthingjobmanagerexternalservice/src/ops/start_command_execution.rs lines 464 and 594-615 (the start parses the thing name out of targetArn, never checks the registry, and persists the record with its timeout deadline).

## Evidence
Check: iot-jobs-data.start-command-execution-client-token-idempotent-and-absent-target-over-permissive — verdict mismatch, "Check timed out after 60.0s".

Sequence the check performs (abridged): CreateThing(<thing>); CreateCommand(<commandId>, namespace=AWS-IoT, payload static); StartCommandExecution(targetArn=<thing ARN>, commandArn, clientToken=<tok>) twice (same executionId — passes); StartCommandExecution with no clientToken (new executionId — passes); ListCommandExecutions(commandArn, startedTimeFilter.after=<now-1h, minute precision>) shows both (passes); then ghost_arn = the thing ARN with the thing name replaced by a name ListThings does not report, and StartCommandExecution(targetArn=ghost_arn, commandArn, executionTimeoutSeconds=10) is ACCEPTED and returns a fresh executionId (passes — the emulator is correctly over-permissive here); then the check polls GetCommandExecution(executionId=<ghost execution>, targetArn=ghost_arn) for a terminal status.

Expected (AWS, recorded): GetCommandExecution answers HTTP 200 with executionId=<ghost execution>, targetArn=ghost_arn verbatim, status progressing CREATED -> TIMED_OUT within ~executionTimeoutSeconds, and ListThings still does not report the ghost thing.

Actual (emulator): every GetCommandExecution for that executionId answers HTTP 403, error code AccessDeniedException, message "targetArn not found in account". Message anatomy: the whole sentence is fixed text with no variable part — it interpolates neither the ARN nor the account — and AWS sends no such error for this operation at all (GetCommandExecution's modeled errors are InternalServerException/ResourceNotFoundException/ThrottlingException/ValidationException). The poll swallows the ClientError and retries, so the check never reaches its assertions and is killed by the 60s harness budget rather than failing an assertion.

## Steps to Reproduce
1. Boot the emulator and point an iot client and an iot-jobs-data client at the published iot:Data-ATS endpoint (DescribeEndpoint(endpointType="iot:Data-ATS")).
2. iot:CreateThing(thingName=T) and iot:CreateCommand(commandId=C, namespace="AWS-IoT", payload={content, contentType}); note commandArn.
3. Build GHOST_ARN by taking DescribeThing(T).thingArn and replacing the thing name with a name no thing has (confirm with ListThings that it is absent).
4. iot-jobs-data:StartCommandExecution(targetArn=GHOST_ARN, commandArn=<C's ARN>, executionTimeoutSeconds=10) -> observe 200 and an executionId E.
5. iot:GetCommandExecution(executionId=E, targetArn=GHOST_ARN) -> observe HTTP 403 AccessDeniedException "targetArn not found in account". AWS answers 200 with the execution.
6. Repeat step 5 for >10s: the status never becomes TIMED_OUT because the record is never readable. iot:ListCommandExecutions(commandArn=<C's ARN>, startedTimeFilter={after: <now-1h, 'YYYY-MM-DDThh:mm'>}) does list the same execution, showing the reader/lister disagreement.
7. iot:DeleteCommandExecution(executionId=E, targetArn=GHOST_ARN) -> same 403, so the record cannot be removed either.
