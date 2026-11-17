Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__kinesis.rest-api-aws-integration-kinesis-putrecord-with-credentials-role-puts-the-record
Issue-Key: apigateway__kinesis.rest-api-aws-integration-kinesis-putrecord-with-credentials-role-puts-the-record|_poll_http#24|TimeoutError|raise TimeoutError(
Issue-Commit: 5782d860642bee99336bbb1ce42eb572c637eba4

# apigateway__kinesis: the AccessDenied a REST API AWS-integration surfaces from Kinesis names the role ARN instead of the assumed-role session, drops the policy clause, and is keyed 'message'/'com.amazonaws.kinesis#AccessDeniedException'

> _Note: this issue was identified against the code base at commit `5782d860642bee99336bbb1ce42eb572c637eba4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRecord

## Description
Cause 1 of 1: when a REST API `AWS` integration calls Kinesis PutRecord with a credentials role that grants no kinesis:PutRecord, the AccessDeniedException Kinesis returns diverges from AWS in three ways at once — the principal it names, the reason clause it omits, and the JSON keys it is serialized under. The HTTP status (200, because the integration's catch-all selectionPattern "" maps it) and the error class are right; the body is not.
AWS: recorded behavior of the Kinesis authorization refusal reached through the integration (the Smithy model declares the error shape but not its message text). The body is the AWS-JSON error object `{"__type": "AccessDeniedException", "Message": "User: <principal> is not authorized to perform: kinesis:PutRecord on resource: <stream ARN> because no identity-based policy allows the kinesis:PutRecord action"}`:
  - `__type` is the bare error code `AccessDeniedException`;
  - the message key is `Message` (capital M);
  - the fixed text is "User: ", " is not authorized to perform: ", " on resource: " and the trailing " because no identity-based policy allows the <action> action"; the variable parts are the PRINCIPAL, the action and the resource ARN, all taken from the request and from state;
  - the principal is the ASSUMED-ROLE session the integration's credentials role was assumed into — `arn:aws:sts::<account>:assumed-role/<role-name>/<session-name>` — not the role's own IAM ARN.
Emulator: the body is `{"__type": "com.amazonaws.kinesis#AccessDeniedException", "message": "User: arn:aws:iam::<account>:role/<path>/<role-name> is not authorized to perform: kinesis:PutRecord on resource: arn:aws:kinesis:<region>:<account>:stream/<stream>"}` — the `__type` is namespace-qualified, the message key is lower-case `message`, the principal is the IAM role ARN (with its path) instead of the assumed-role session ARN, and the "because no identity-based policy allows the kinesis:PutRecord action" clause is absent. The check therefore never converges and times out on the deterministic denial.
Why: a client reading the denial gets no usable principal — the assumed-role session identity AWS reports is what tells an operator WHICH role session was denied, and tooling that parses `Message` finds nothing under that key. SDK/CLI error handling keyed on `__type == "AccessDeniedException"` (botocore resolves the error code from `__type`) classifies the namespace-qualified value differently, and the missing reason clause removes the statement that distinguishes "no identity-based policy allows it" from an explicit deny or a resource-policy denial. A retry loop that waits for the deterministic denial (as this check does) never terminates.
Fix: the Kinesis authorization-denial path and its error serializer — the AccessDeniedException message assembly (principal rendering must use the assumed-role session ARN of the credentials role, and the message must carry the trailing no-identity-based-policy clause) and the AWS-JSON error body keys (`__type` bare, `Message` capitalized) in the kinesis service crate's error channel.
Traces: apigateway__kinesis.rest-api-aws-integration-kinesis-putrecord-with-credentials-role-puts-the-record.

## Evidence
Expected (AWS), for a credentials role with no kinesis:PutRecord: HTTP 200 (catch-all integration response) with body `{"__type":"AccessDeniedException","Message":"User: arn:aws:sts::<account>:assumed-role/<role-name>/<session-name> is not authorized to perform: kinesis:PutRecord on resource: <stream ARN> because no identity-based policy allows the kinesis:PutRecord action"}`.

Actual (emulator), from the replay: "TimeoutError: POST /put-denied converging on the deterministic AccessDeniedException (<stage url>/put-denied) never reached an accepted status within 47s. Last: 200 '{\"__type\":\"com.amazonaws.kinesis#AccessDeniedException\",\"message\":\"User: arn:aws:iam::<account>:role/sim2real/<role-name> is not authorized to perform: kinesis:PutRecord on resource: arn:aws:kinesis:<region>:<account>:stream/<stream-name>'".

Fixed vs variable: the fixed text is "User: ", " is not authorized to perform: ", " on resource: " and " because no identity-based policy allows the kinesis:PutRecord action". The account id, role name and path, stream name/ARN and region in the recording are per-run values that fill the template; the divergences are (a) `__type` namespace-qualified instead of the bare code, (b) the message under `message` instead of `Message`, (c) the principal rendered as `arn:aws:iam::<account>:role/<path>/<role-name>` instead of `arn:aws:sts::<account>:assumed-role/<role-name>/<session-name>`, and (d) the missing trailing reason clause.

## Steps to Reproduce
1. `kinesis.create_stream(StreamName=<s>, StreamModeDetails={"StreamMode":"ON_DEMAND"})` and wait for ACTIVE; note the StreamARN.
2. Create two IAM roles trusted by apigateway.amazonaws.com: one allowing kinesis:PutRecord on that stream ARN, one with no permissions at all.
3. CreateRestApi; add POST /put-denied with `authorizationType=AWS_IAM` and an `AWS` integration: `integrationHttpMethod=POST`, `uri=arn:aws:apigateway:<region>:kinesis:action/PutRecord`, `credentials=<the permissionless role ARN>`, a requestTemplates mapping to {StreamName, Data, PartitionKey}, `passthroughBehavior=NEVER`, plus an integration response with `selectionPattern=""` for 200 and a 200 method response. Deploy a stage.
4. SigV4-sign and POST `{"data":"...","pk":"..."}` to `<stage url>/put-denied`.
5. Inspect the 200 body: AWS carries `__type: "AccessDeniedException"` and `Message` naming `arn:aws:sts::<account>:assumed-role/<role>/<session>` and ending "...because no identity-based policy allows the kinesis:PutRecord action"; the emulator carries `__type: "com.amazonaws.kinesis#AccessDeniedException"`, a lower-case `message`, the IAM role ARN as the principal, and no reason clause.
Full scenario: replay the check id in trace_ids.
