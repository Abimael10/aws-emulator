Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected
Issue-Key: appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected|run#43|AssertionError|assert re.sub(r'"requestId": "[0-9a-f-]+"', '"requestId": "<id>"', message) == (
Issue-Commit: 2714aa03aa1d60831b8d663aed1f4386c9b03868

# appconfig__lambda: the Lambda unhandled-error payload AppConfig relays for a refusing LAMBDA validator drops the source line from every stackTrace frame

> _Note: this issue was identified against the code base at commit `2714aa03aa1d60831b8d663aed1f4386c9b03868`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartDeployment

## Description
One net-new root cause, grounded in a replayed behavioral check. Numbered; close it before submitting.

=== Cause 1 of 1: the unhandled-exception payload of an invoked Lambda carries truncated stackTrace frames (header line only, source text dropped), and AppConfig's StartDeployment refusal relays that truncated payload ===

AWS: a LAMBDA validator that raises makes StartDeployment refuse the deployment with BadRequestException, HTTP 400 (Smithy: appconfig StartDeployment errors BadRequestException 400, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404), and AppConfig relays the Lambda invocation's own failure verbatim in the message. The message's fixed text is 'Function Error: ', ', Log Result: ', ', Payload: '; its variable parts are the invocation's FunctionError kind ('Unhandled' for an exception the handler did not catch), the log result, and the raw error Payload JSON produced by the Lambda managed runtime. For the Python managed runtime that payload is {"errorMessage", "errorType", "requestId", "stackTrace"}, where stackTrace is the exception's traceback rendered frame by frame the way traceback.format_exception renders it: each entry is the frame header '  File "<path>", line <n>, in <function>\n' FOLLOWED, in the same entry, by the offending source statement indented four spaces and a trailing newline. errorMessage/errorType come from the raised exception, requestId from the invocation, and each frame's path, line number, function name and source text from the deployed code. (recorded behavior for the relay wrapper; the payload shape is the documented Python-runtime error format.)

Emulator: the status, the error code BadRequestException, the 'Function Error: Unhandled, Log Result: null, Payload: ' wrapper, errorMessage and errorType all match — but every stackTrace entry stops at the frame header and the source statement is missing. The emulator serves
  "stackTrace": ["  File \"/var/task/handler.py\", line 8, in handler\n"]
where AWS serves
  "stackTrace": ["  File \"/var/task/handler.py\", line 8, in handler\n    raise Exception(\"schema_version must be 2, got %r\" % (doc.get(\"schema_version\"),))\n"]
The path, line number, function name and source text are values taken from the invoked function's own code, not literals of this recording; the requirement is that the frame's source line be present at all.

Why: a LAMBDA validator's traceback is the only channel AppConfig gives a caller for WHY the configuration was refused beyond the exception message, and tooling that surfaces or parses it (CI output, a deployment-gate report, anything that locates the rejecting statement) loses the statement that did the rejecting. More broadly, any client of the Lambda data plane that reads the error payload of an unhandled handler exception gets a traceback whose frames name a file and line but never show the code, so a failure cannot be diagnosed from the invocation result.

Fix: the Lambda emulator's managed-runtime error-payload builder — the component that formats the unhandled-exception Payload (errorMessage/errorType/requestId/stackTrace) must render each frame as the full traceback.format_exception frame, header line plus the indented source line read from the deployed package, and then AppConfig's StartDeployment validator-relay path carries the complete payload unchanged.

Traces: appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected.

## Evidence
Replay report (simparity-behavioral-aws-v1), check [mismatch] appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected:

  AssertionError: StartDeployment must relay the validator's own error, got 'Function Error: Unhandled, Log Result: null, Payload: {"errorMessage": "schema_version must be 2, got 1", "errorType": "Exception", "requestId": "<id>", "stackTrace": ["  File \"/var/task/handler.py\", line 8, in handler\n"]}'

Expected (the check masks only the per-invocation requestId, which is not part of the requirement):
  'Function Error: Unhandled, Log Result: null, Payload: {"errorMessage": "schema_version must be 2, got 1", "errorType": "Exception", "requestId": "<id>", "stackTrace": ["  File \"/var/task/handler.py\", line 8, in handler\n    raise Exception(\"schema_version must be 2, got %r\" % (doc.get(\"schema_version\"),))\n"]}'

The sole divergence is the missing '\n    <source statement>' tail of the one stackTrace frame. HTTP status and error code were correct (BadRequestException), the refusal left no deployment record, and the rejected bytes were not served — only the message text diverges.

## Steps to Reproduce
1. Create an IAM role assumable by lambda.amazonaws.com with AWSLambdaBasicExecutionRole attached.
2. lambda.create_function (Runtime python3.12, Handler handler.handler) whose handler base64-decodes event['content'], parses it, and `raise Exception("schema_version must be 2, got %r" % (doc.get("schema_version"),))` when the document's schema_version is not 2; wait for it to go Active and add_permission for Principal appconfig.amazonaws.com.
3. appconfig.create_application / create_environment / create_configuration_profile(LocationUri='hosted', Validators=[{'Type':'LAMBDA','Content':<function arn>}]) and a zero-duration, zero-bake deployment strategy.
4. create_hosted_configuration_version with compliant content and deploy it to COMPLETE (proves the validator is reachable).
5. create_hosted_configuration_version with content whose schema_version is 1, then appconfig.start_deployment for that version.
6. Read the BadRequestException's Error.Message. Emulator: the relayed Payload's stackTrace frame is the '  File "...", line N, in handler\n' header only. AWS: the same frame continues with the four-space-indented source statement and a trailing newline.
Replay appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected to confirm.
