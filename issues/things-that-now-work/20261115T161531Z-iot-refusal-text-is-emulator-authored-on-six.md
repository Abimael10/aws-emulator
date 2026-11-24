Issue-Source: trace-sweep-divergence
Issue-Trace: iot-vpce-traj-policy-document-refusals
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-vpce-traj-policy-document-refusals iot-account-audit-config-negative-paths iot-audit-task-and-finding-not-found iot-audit-task-and-finding-not-found-errors iot-audit-task-lookup-and-cancel-not-found iot-authorizer-lifecycle iot-custom-authorizer-lifecycle iot-job-execution-workflow iot-stream-error-paths iot-stream-lifecycle iot-stream-lifecycle-and-errors
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|CancelAuditTask|4xx->4xx|$.message
Issue-Key: iot|TestInvokeAuthorizer|4xx->4xx|$.message
Issue-Key: iot|TestInvokeAuthorizer|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: iot|AssociateTargetsWithJob|4xx->4xx|$.message
Issue-Key: iot|CreateStream|4xx->4xx|$.message
Issue-Related: 20261107T022027Z-iot-refusal-envelopes-8-causes-wrong-x-amzn.md
Issue-Related: 20261115T161531Z-iot-refusals-are-never-raised-default-policy-version.md
Issue-Commit: 4167e7cf5381813eabd70d9b700d048404e5c90a

# iot refusal text is emulator-authored on six operations, and TestInvokeAuthorizer also returns the wrong error code

> _Note: this issue was identified against the code base at commit `4167e7cf5381813eabd70d9b700d048404e5c90a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePolicy

## Description
Seven recorded refusals carry emulator-authored prose instead of the message AWS sends for that error class; one of them also carries the wrong modeled error, and one refuses on the wrong precondition entirely. The HTTP status, the error code and the message text are all part of the contract, so each of these is a defect even where the status matches. 7 causes.

Cause 1 of 7: CreatePolicy with a syntactically invalid policy document leaks the JSON parser's internals.
AWS: Smithy: CreatePolicy errors include MalformedPolicyException (client, httpError 400); policyDocument is @required with @length 0..404600. For a document that is not valid JSON, AWS answers 400 with the fixed message 'Policy document is Malformed: Invalid policy syntax.' — the whole string is fixed text, with no request-derived parts.
Emulator: answers 400 with 'policyDocument is not a valid JSON document: EOF while parsing a list at line 1 column 40', i.e. the serde parser's own diagnostic including a byte offset.
Why: a client matching or logging AWS's refusal text sees an unrecognised message, and the offset/parser wording is not reproducible across inputs.
Fix: the CreatePolicy handler's policy-document validation and its MalformedPolicyException serializer.
Traces: iot-vpce-traj-policy-document-refusals#task#1.

Cause 2 of 7: CreatePolicy's null-member validation message interpolates the null value.
AWS: Smithy: policyDocument is @required. AWS's constraint report for a missing required member reads "1 validation error detected: Value at 'policyDocument' failed to satisfy constraint: Member must not be null" — fixed text with only the member name ('policyDocument') taken from the request shape; AWS does not render the value for a null member.
Emulator: answers "1 validation error detected: Value null at 'policyDocument' failed to satisfy constraint: Member must not be null", inserting the literal 'null' where AWS has nothing.
Why: clients and tests that assert AWS's constraint-report wording fail on an otherwise correct refusal.
Fix: the shared constraint-violation report serializer used by the CreatePolicy input validation (omit the value segment for a null member).
Traces: iot-vpce-traj-policy-document-refusals#task#7, #task#8.

Cause 3 of 7: CancelAuditTask's not-found message is reworded.
AWS: Smithy: CancelAuditTask errors include ResourceNotFoundException (client, httpError 404); taskId is an @httpLabel with @length 1..40, pattern ^[a-zA-Z0-9\-]+$. For an unknown task AWS answers 404 with message 'Audit task with TaskId=<taskId> was not found' — fixed text 'Audit task with TaskId=' and ' was not found' around the taskId taken from the request path.
Emulator: answers 404 with 'Audit task <taskId> does not exist.'
Why: the status matches but the text does not, so message assertions and operator-facing logs diverge. (Related open issue 20261114T091403Z-iot-device-defender-audit-plane-5-causes-cancelaudittask.md covers other audit-plane causes and owns the sibling DescribeAuditTask message, but it does not carry this step's replay key.)
Fix: the CancelAuditTask handler's not-found path.
Traces: iot-account-audit-config-negative-paths#task#8, iot-audit-task-and-finding-not-found#task#2, iot-audit-task-and-finding-not-found-errors#task#2, iot-audit-task-lookup-and-cancel-not-found#task#2.

Cause 4 of 7: TestInvokeAuthorizer's lambda-invocation failure is reported as emulator prose that names the function ARN.
AWS: Smithy: TestInvokeAuthorizer (POST /authorizer/{authorizerName}/test) errors include InvalidRequestException (client, httpError 400). When the authorizer's Lambda cannot be invoked because the service is not permitted to, AWS answers 400 with 'AccessDeniedException encountered when invoking lambda; requestId: <requestId>' — the fixed text is 'AccessDeniedException encountered when invoking lambda; requestId: ', and the trailing id is the per-call request id (the recorded id itself is a per-run value, not a requirement).
Emulator: answers 400 with 'Authorizer <authorizerName> could not be invoked: Function not found: <lambda function ARN>:$LATEST' — a different failure class, worded by the emulator, that leaks the resolved function ARN.
Why: a client cannot tell an authorization failure from a missing function, and the recorded refusal class (access denied when invoking the authorizer's lambda) is unobservable.
Fix: the TestInvokeAuthorizer handler's lambda-invocation failure path and its message.
Traces: iot-authorizer-lifecycle#task#4.

Cause 5 of 7: TestInvokeAuthorizer with no Http/Mqtt/Tls context returns InvalidResponseException instead of validating the request.
AWS: Smithy: TestInvokeAuthorizerRequest has optional httpContext/mqttContext/tlsContext members and the operation models both InvalidRequestException and InvalidResponseException (both client, httpError 400). AWS validates the request before invoking anything: with none of the three contexts supplied it answers 400 with x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/' and the fixed message 'Request must have Http/Mqtt/Tls context.' (no variable parts).
Emulator: answers 400 with x-amzn-errortype 'InvalidResponseException' and 'Authorizer <authorizerName> could not be invoked: Function not found: <lambda function ARN>:$LATEST' — it skips input validation, invokes, and reports the invoke failure under the wrong modeled error.
Why: the error code is what SDK callers branch on; a caller handling InvalidRequestException (fix the request) instead sees InvalidResponseException (the authorizer misbehaved) and retries or escalates wrongly.
Fix: the TestInvokeAuthorizer handler's input validation, which must run before authorizer invocation, plus the error classification.
Traces: iot-custom-authorizer-lifecycle#task#3.

Cause 6 of 7: AssociateTargetsWithJob's SNAPSHOT refusal does not name the job.
AWS: Smithy: AssociateTargetsWithJob (POST /jobs/{jobId}/targets) errors include InvalidRequestException (client, httpError 400); jobId is an @httpLabel. Targets can only be added to a CONTINUOUS job, and AWS's refusal names the job: 'Targets cannot be associated with a job <jobId> with SNAPSHOT target selection.' — fixed text 'Targets cannot be associated with a job ' and ' with SNAPSHOT target selection.' around the jobId from the request path.
Emulator: answers 400 with 'The job must have been created with the targetSelection field set to CONTINUOUS', which names neither the job nor its actual target selection.
Why: in a batch of associations the client cannot tell which job was refused, and message assertions fail. (An open issue 'Implement iot AssociateTargetsWithJob: unimplemented scaffold stub', 20261114T082236Z-iot-associatetargetswithjob-stub.md, exists, but the operation now answers 400 and that issue does not carry this step's replay key.)
Fix: the AssociateTargetsWithJob handler's target-selection precondition message.
Traces: iot-job-execution-workflow#task#4.

Cause 7 of 7: CreateStream refuses on the wrong precondition — S3 object existence instead of role assumption.
AWS: Smithy: CreateStream (POST /streams/{streamId}) errors include InvalidRequestException (client, httpError 400); roleArn and files are @required. AWS's first pre-flight is assuming the supplied role; when that role cannot be assumed it answers 400 with 'Permission denied when attempting to assume role <roleArn>.' — fixed text 'Permission denied when attempting to assume role ' and the trailing '.' around the roleArn taken from the request body.
Emulator: answers 400 with 'The S3 object s3://<bucket>/<key> does not exist', i.e. it never attempts the role pre-flight and reports a different precondition.
Why: the operator is pointed at the wrong misconfiguration (object vs. trust policy), and a stream whose role is unassumable but whose object exists would be accepted. (Related open issues 20261114T082236Z-iot-refusal-message-fidelity-7-causes-addthingtothinggroup-createstream.md and 20261114T091403Z-iot-dependent-resource-pre-flight-is-missing-on.md describe adjacent CreateStream causes but do not carry this step's replay key.)
Fix: the CreateStream handler's role-assumption pre-flight, which must precede the file/object check, and its refusal message.
Traces: iot-stream-error-paths#task#4, iot-stream-lifecycle#task#3, iot-stream-lifecycle-and-errors#task#3.

## Evidence
Cause 1 (iot-vpce-traj-policy-document-refusals#task#1, 400 vs 400): $.message expected 'Policy document is Malformed: Invalid policy syntax.', got 'policyDocument is not a valid JSON document: EOF while parsing a list at line 1 column 40'.
Cause 2 (iot-vpce-traj-policy-document-refusals#task#7, also #task#8): $.message expected "1 validation error detected: Value at 'policyDocument' failed to satisfy constraint: Member must not be null", got "1 validation error detected: Value null at 'policyDocument' failed to satisfy constraint: Member must not be null".
Cause 3 (iot-account-audit-config-negative-paths#task#8, 404 vs 404; cluster also covers iot-audit-task-and-finding-not-found#task#2, iot-audit-task-and-finding-not-found-errors#task#2, iot-audit-task-lookup-and-cancel-not-found#task#2): $.message expected 'Audit task with TaskId=<taskId> was not found', got 'Audit task <taskId> does not exist.'
Cause 4 (iot-authorizer-lifecycle#task#4, 400 vs 400): $.message expected 'AccessDeniedException encountered when invoking lambda; requestId: <requestId>', got 'Authorizer <authorizerName> could not be invoked: Function not found: arn:aws:lambda:<region>:<account>:function:<fn>:$LATEST'.
Cause 5 (iot-custom-authorizer-lifecycle#task#3, 400 vs 400): $.message expected 'Request must have Http/Mqtt/Tls context.', got 'Authorizer <authorizerName> could not be invoked: Function not found: ...:$LATEST'; $headers.x-amzn-errortype expected 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', got 'InvalidResponseException'.
Cause 6 (iot-job-execution-workflow#task#4, 400 vs 400): $.message expected 'Targets cannot be associated with a job <jobId> with SNAPSHOT target selection.', got 'The job must have been created with the targetSelection field set to CONTINUOUS'.
Cause 7 (iot-stream-error-paths#task#4, also iot-stream-lifecycle#task#3 and iot-stream-lifecycle-and-errors#task#3, 400 vs 400): $.message expected 'Permission denied when attempting to assume role <roleArn>.', got 'The S3 object s3://<bucket>/<key> does not exist'.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: POST /policies/{policyName} with a policyDocument that is not valid JSON (fixture iot-vpce-traj-policy-document-refusals, step task#1).
3. Cause 2: POST /policies/{policyName} with policyDocument null (iot-vpce-traj-policy-document-refusals#task#7).
4. Cause 3: PUT /audit/tasks/{unknownTaskId}/cancel (iot-account-audit-config-negative-paths#task#8).
5. Cause 4: POST /authorizer/{authorizerName}/test for an authorizer whose Lambda cannot be invoked (iot-authorizer-lifecycle#task#4).
6. Cause 5: POST /authorizer/{authorizerName}/test with no httpContext, mqttContext or tlsContext (iot-custom-authorizer-lifecycle#task#3) and inspect x-amzn-errortype.
7. Cause 6: create a SNAPSHOT job, then POST /jobs/{jobId}/targets (iot-job-execution-workflow#task#4).
8. Cause 7: POST /streams/{streamId} with a roleArn the service cannot assume (iot-stream-error-paths#task#4).
