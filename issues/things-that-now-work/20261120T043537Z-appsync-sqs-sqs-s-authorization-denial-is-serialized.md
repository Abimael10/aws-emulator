Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__sqs.http-datasource-role-without-sendmessage-denied-then-granted-delivers
Issue-Key: appsync__sqs.http-datasource-role-without-sendmessage-denied-then-granted-delivers|run#23|AssertionError|assert denied_body == {
Issue-Commit: c119c6290af0aef232d88082a679964269a74639

# appsync__sqs: SQS's authorization denial is serialized as __type 'AccessDenied' with a lowercase 'message' member instead of 'com.amazon.coral.service#AccessDeniedException' / 'Message'

> _Note: this issue was identified against the code base at commit `c119c6290af0aef232d88082a679964269a74639`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
One net-new root cause, observed through an AppSync HTTP data source that signs SigV4 to the SQS endpoint with a service role carrying no sqs:SendMessage grant.

Cause 1 of 1: the awsJson1_0 error document SQS returns for an IAM authorization denial uses an emulator-authored `__type` value and the wrong member name for the message.
Failing check id: appsync__sqs.http-datasource-role-without-sendmessage-denied-then-granted-delivers

AWS: Smithy: AmazonSQS (version 2012-11-05) is awsJson1_0 with the awsQueryCompatible protocol modifier; sigv4Name 'sqs'; AccessDeniedException is NOT a modeled SQS error and is not among the service's common_errors (InvalidAddress, InvalidSecurity, RequestThrottled, UnsupportedOperation), so it is the framework refusal and its wire form is recorded behavior only. An SQS SendMessage signed by a principal with no identity-based sqs:SendMessage grant is refused with HTTP 403 and the awsJson1_0 body {"__type": "com.amazon.coral.service#AccessDeniedException", "Message": "<denial sentence>"} — `__type` is the fully qualified coral shape id (fixed text), and the message member is capital-M `Message`, as framework errors of this protocol always are. The denial sentence itself is AWS's implicit-deny template: the fixed text "User: ", " is not authorized to perform: ", " on resource: ", " because no identity-based policy allows the ", " action", with the variable parts taken from the request and state — the caller's assumed-role ARN (arn:aws:sts::<account>:assumed-role/<role name>/<session name>, which for an AppSync data source role is session name APPSYNC_ASSUME_ROLE), the lowercased action 'sqs:sendmessage', and the target queue's ARN. Nothing is delivered to the queue.
Emulator: the HTTP status (403) and the denial sentence are both right, but the envelope is not: the body is {"__type": "AccessDenied", "message": "<the same sentence>"} — the `__type` is the bare, unqualified token "AccessDenied" instead of the qualified coral id "com.amazon.coral.service#AccessDeniedException", and the message is carried under lowercase `message` instead of `Message`.
Why: `__type` is how every awsJson1_0 client resolves a refusal to an error class, and the capitalised `Message` is where the SDKs read the text. With an unqualified, differently named `__type`, an SDK cannot match the error to AccessDeniedException and falls back to an unknown/generic error, while `exc.response['Error']['Message']` comes back empty because the member it reads is absent — so an application's `except AccessDeniedException` branch and all of its denial logging go dead against the emulator even though the server did refuse correctly. Read through a resolver, as here, the application sees a body it cannot parse into the denial it is written to handle.
Fix: the awsJson1_0 error serializer for framework/authorization refusals in the sqs service crate — emit the fully qualified `com.amazon.coral.service#AccessDeniedException` as `__type` and carry the sentence under `Message`, leaving the already-correct 403 and denial wording untouched.
Traces: appsync__sqs.http-datasource-role-without-sendmessage-denied-then-granted-delivers (403 and the denial sentence match; the body is {'__type': 'AccessDenied', 'message': '…'}).

## Evidence
Cause 1 — appsync__sqs.http-datasource-role-without-sendmessage-denied-then-granted-delivers
Expected (AWS): the mutation's SendMessage leg returns statusCode 403 and the body JSON
{"__type": "com.amazon.coral.service#AccessDeniedException",
 "Message": "User: arn:aws:sts::<account>:assumed-role/<role name>/APPSYNC_ASSUME_ROLE is not authorized to perform: sqs:sendmessage on resource: <queue arn> because no identity-based policy allows the sqs:sendmessage action"}
(the account, role name and queue ARN are the request's/state's values; the rest is fixed text).
Actual (emulator): statusCode 403 (correct) with
{"__type": "AccessDenied",
 "message": "User: arn:aws:sts::<account>:assumed-role/<role name>/APPSYNC_ASSUME_ROLE is not authorized to perform: sqs:sendmessage on resource: <queue arn> because no identity-based policy allows the sqs:sendmessage action"}
AssertionError: expected AccessDeniedException with the exact denial message, got {'__type': 'AccessDenied', 'message': 'User: arn:aws:sts::000000000000:assumed-role/sim2real-behav-role-049d6840a2c5/APPSYNC_ASSUME_ROLE is not authorized to perform: sqs:sendmessage on resource: arn:aws:sqs:us-west-2:000000000000:sim2real-behav-q-951c0a8921f1 because no identity-based policy allows the sqs:sendmessage action'} — the account id, role name and queue name in that line are this run's generated values, not part of the contract; the two divergences are the `__type` value and the `message` vs `Message` member name.

## Steps to Reproduce
1. sqs.CreateQueue(QueueName=<q>) and GetQueueAttributes(AttributeNames=['QueueArn']) to capture the queue ARN.
2. iam.CreateRole(RoleName=<r>, AssumeRolePolicyDocument=trust allowing appsync.amazonaws.com to sts:AssumeRole) with NO inline or managed policy attached.
3. appsync.CreateGraphqlApi(authenticationType='API_KEY'); StartSchemaCreation with a `Mutation { sendToQueue(queueUrl: String!, body: String!): SendResult }` / `type SendResult { statusCode: Int! body: String! }` schema; CreateApiKey.
4. appsync.CreateDataSource(type='HTTP', serviceRoleArn=<role arn>, httpConfig={endpoint: 'https://sqs.<region>.amazonaws.com', authorizationConfig: {authorizationType: 'AWS_IAM', awsIamConfig: {signingRegion: <region>, signingServiceName: 'sqs'}}}).
5. appsync.CreateResolver(Mutation.sendToQueue, runtime APPSYNC_JS 1.0.0) whose request() POSTs resourcePath '/' with headers {'Content-Type': 'application/x-amz-json-1.0', 'X-Amz-Target': 'AmazonSQS.SendMessage'} and body JSON.stringify({QueueUrl, MessageBody}), and whose response() returns {statusCode: ctx.result.statusCode, body: ctx.result.body}.
6. POST the mutation with x-api-key. The reply's statusCode is 403 (correct) — json.loads(body) must equal {'__type': 'com.amazon.coral.service#AccessDeniedException', 'Message': <the implicit-deny sentence>}; the emulator returns {'__type': 'AccessDenied', 'message': <same sentence>}.
7. (Rest of the scenario, unreached here) ReceiveMessage shows nothing arrived; iam.PutRolePolicy granting sqs:SendMessage on the queue ARN then makes the identical mutation return 200 and the message arrive.
