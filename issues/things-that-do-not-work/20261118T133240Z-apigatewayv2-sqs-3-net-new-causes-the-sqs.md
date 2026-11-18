Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__sqs.aws-proxy-subtype-sqs-deletemessage-required-parameters-refused-else-deletes-the-message apigatewayv2__sqs.aws-proxy-subtype-sqs-purgequeue-required-queueurl-refused-else-empties-the-queue apigatewayv2__sqs.aws-proxy-subtype-sqs-receivemessage-required-queueurl-refused-else-reads-the-queue
Issue-Key: apigatewayv2__sqs.aws-proxy-subtype-sqs-deletemessage-required-parameters-refused-else-deletes-the-message|run#13|AssertionError|assert message == (
Issue-Key: apigatewayv2__sqs.aws-proxy-subtype-sqs-purgequeue-required-queueurl-refused-else-empties-the-queue|_poll_http#22|TimeoutError|raise TimeoutError(
Issue-Key: apigatewayv2__sqs.aws-proxy-subtype-sqs-receivemessage-required-queueurl-refused-else-reads-the-queue|_poll_until_message#22|TimeoutError|raise TimeoutError(
Issue-Commit: 1f604c3d80924acff5038a505405bc1871ebd9db

# apigatewayv2__sqs: 3 net-new causes — the SQS-DeleteMessage refusal lists its required parameters alphabetically, and the SQS-PurgeQueue and SQS-ReceiveMessage subtypes are never performed (500)

> _Note: this issue was identified against the code base at commit `1f604c3d80924acff5038a505405bc1871ebd9db`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
Three net-new root causes in the SQS AWS_PROXY integration subtypes of API Gateway v2. Each is numbered with its own failing check ids; all three must be closed.

Cause 1 of 3: the SQS-DeleteMessage required-parameter refusal lists the parameters in the wrong order. Failing check: apigatewayv2__sqs.aws-proxy-subtype-sqs-deletemessage-required-parameters-refused-else-deletes-the-message.
AWS: Smithy (apigatewayv2 CreateIntegration) models IntegrationSubtype and RequestParameters and declares BadRequestException (errorKind client, httpError 400); the refusal text is not modelled, so it is recorded behavior only. Recorded: HTTP 400, Error.Code `BadRequestException`, message `Operation: SQS-DeleteMessage requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [ReceiptHandle, QueueUrl]`. Fixed text: `Operation: ` … ` requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [` … `]`. Variable parts: the subtype the request named, and the bracketed list — the subtype's whole required set in AWS's own per-subtype order (for SQS-DeleteMessage: ReceiptHandle, QueueUrl; note AWS orders SQS-SendMessage the other way round, QueueUrl, MessageBody, so the order is per-subtype and never alphabetical).
Emulator: the status (400), the code (BadRequestException) and the sentence are right, the list is alphabetised: `... parameter mapping: [QueueUrl, ReceiptHandle]`.
Why: a client or IaC layer matching, logging or displaying the refusal reads a parameter list AWS never emits.
Fix: the first-class-subtype catalog that states each subtype's required set, and the single renderer of that sentence — hold AWS's per-subtype order, not a sorted one.
Traces: apigatewayv2__sqs.aws-proxy-subtype-sqs-deletemessage-required-parameters-refused-else-deletes-the-message (the check stopped here, so its positive control — the mapped route deleting the message — was not graded).

Cause 2 of 3: a correctly configured SQS-PurgeQueue subtype route is never performed; the invoke answers 500. Failing check: apigatewayv2__sqs.aws-proxy-subtype-sqs-purgequeue-required-queueurl-refused-else-empties-the-queue.
AWS: recorded behavior only — a route whose AWS_PROXY integration names IntegrationSubtype SQS-PurgeQueue with QueueUrl mapped performs SQS PurgeQueue under the integration's CredentialsArn role and answers the caller HTTP 200 carrying the queue api's own query-interface envelope for the action (`PurgeQueueResponse`/`ResponseMetadata`, as text/xml — the same form this emulator already relays for SQS-SendMessage), after which the queue's ApproximateNumberOfMessages falls to 0.
Emulator: every call to the route answers HTTP 500 with body `{"message":"Internal Server Error"}` for the whole 50s poll window — the data plane's "the integration produced no response" class — so the queue is never purged.
Why: a client fronting a queue with an HTTP API gets a server error for a correctly configured route; the purge silently never happens, and because 500 is also the emulator's generic no-response class the client cannot tell an unsupported subtype from a transient fault.
Fix: the execute-api data plane's first-class service-action dispatch for SQS — the sqs action module performs only SendMessage, so PurgeQueue has no implementation to reach; add the action (mapped parameters → in-process SQS PurgeQueue under the integration identity) and relay its answer in the query-envelope form, 200.
Traces: apigatewayv2__sqs.aws-proxy-subtype-sqs-purgequeue-required-queueurl-refused-else-empties-the-queue (its refusal leg passed; only the positive control fails).

Cause 3 of 3: a correctly configured SQS-ReceiveMessage subtype route is never performed; the invoke answers 500. Failing check: apigatewayv2__sqs.aws-proxy-subtype-sqs-receivemessage-required-queueurl-refused-else-reads-the-queue.
AWS: recorded behavior only — a route whose AWS_PROXY integration names IntegrationSubtype SQS-ReceiveMessage with QueueUrl mapped performs SQS ReceiveMessage under the integration's role and answers HTTP 200 carrying the queue api's query-interface `ReceiveMessageResponse` envelope (text/xml) whose message carries the queue's own Body, the MessageId SendMessage minted for it, and a usable ReceiptHandle.
Emulator: every call answers HTTP 500 `{"message":"Internal Server Error"}` for the whole poll window; no message is ever returned.
Why: the route cannot be used to read a queue at all, and the failure is reported as a server fault rather than as anything a client can act on.
Fix: same component as Cause 2 — the SQS action module of the data plane's subtype dispatch; add ReceiveMessage and relay its answer as the 200 query envelope. (SQS-DeleteMessage shares this gap in the same module and will need the same treatment once Cause 1 lets its positive control run.)
Traces: apigatewayv2__sqs.aws-proxy-subtype-sqs-receivemessage-required-queueurl-refused-else-reads-the-queue.

Related open issues that do NOT cover these causes: 20261109T154749Z-apigatewayv2-sqs-2-net-new-causes-createintegration-accepts.md (the earlier failure mode: the subtypes being accepted with nothing mapped) and 20261110T170829Z-apigatewayv2-sqs-the-sqs-sendmessage-aws-proxy-subtype.md (the response FORM of SQS-SendMessage, a different subtype).

## Evidence
Cause 1 — expected `Operation: SQS-DeleteMessage requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [ReceiptHandle, QueueUrl]`; actual `... parameter mapping: [QueueUrl, ReceiptHandle]`. Replay: `AssertionError: unexpected message 'Operation: SQS-DeleteMessage requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [QueueUrl, ReceiptHandle]'` (check line 86). Ordering source: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/state/integration_subtype.rs:88-91, rendered by :152-158.

Cause 2 — expected HTTP 200 and the queue emptied (ApproximateNumberOfMessages → 0). Actual: `TimeoutError: GET /purge reaching the SQS-PurgeQueue integration (<stage url>/purge) never reached an accepted status within 50s. Last: 500 '{"message":"Internal Server Error"}'` (check line 130/212).

Cause 3 — expected HTTP 200 with a parseable ReceiveMessageResponse whose body equals the sent marker and whose MessageId equals SendMessage's. Actual: `TimeoutError: <stage url>/recv never returned a message within 50s. Last: 500 '{"message":"Internal Server Error"}'` (check line 120/186).

Causes 2 and 3 root: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/service/invoke/service_action/sqs.rs implements `send_message` only (module doc: "The `SQS-SendMessage` action"), and src/service/invoke.rs:123-134 documents 500 `{"message":"Internal Server Error"}` as the "integration produced no response" class a subtype with nothing behind it falls into.

## Steps to Reproduce
Cause 1:
1. Create a queue and an IAM role API Gateway can assume; create an HTTP API.
2. CreateIntegration IntegrationType=AWS_PROXY, IntegrationSubtype="SQS-DeleteMessage", PayloadFormatVersion="1.0", CredentialsArn=<role>, RequestParameters={}.
Observed: BadRequestException with `[QueueUrl, ReceiptHandle]`; expected `[ReceiptHandle, QueueUrl]`.

Cause 2:
1. Same queue/role/API. CreateIntegration IntegrationType=AWS_PROXY, IntegrationSubtype="SQS-PurgeQueue", CredentialsArn=<role>, RequestParameters={"QueueUrl": <queue url>}.
2. CreateRoute "GET /purge" targeting that integration with AuthorizationType=AWS_IAM; CreateStage "$default" with AutoDeploy.
3. Send 3 messages to the queue, then make a SigV4-signed GET to <stage url>/purge.
Observed: 500 `{"message":"Internal Server Error"}`, queue depth unchanged. Expected: 200 with the PurgeQueueResponse envelope and the queue emptied.

Cause 3:
1. Same setup with IntegrationSubtype="SQS-ReceiveMessage", RequestParameters={"QueueUrl": <queue url>}, route "GET /recv" (AWS_IAM), stage "$default".
2. SendMessage a marker body, then make a SigV4-signed GET to <stage url>/recv.
Observed: 500 `{"message":"Internal Server Error"}`. Expected: 200 with a ReceiveMessageResponse carrying that body, its MessageId and a ReceiptHandle.
