Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.awsjson-epoch-timestamp-survives-two-deserializers
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.awsjson-epoch-timestamp-survives-two-deserializers|run#68|AssertionError|assert js["epochFloat"]["ok"] is True and \
Issue-Commit: a28aa3cbb761848919217b218a8a95a512704746

# dynamodb: a Lambda handler's call to its OWN in-sandbox loopback endpoint is answered by the emulator's DynamoDB front door with UnrecognizedClientException instead of by the handler's server

> _Note: this issue was identified against the code base at commit `a28aa3cbb761848919217b218a8a95a512704746`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One cause (Cause 1 of 1).

AWS: recorded behavior only — nothing in DynamoDB's Smithy model (DescribeTable: input TableName, output Table/TableDescription, errors InternalServerError / InvalidEndpointException / ResourceNotFoundException) or in Lambda's model describes sandbox networking, so the requirement here is what the recording shows. A Node.js handler that starts an http.Server bound to 127.0.0.1 inside its own sandbox and then constructs a DynamoDBClient with an EXPLICIT `endpoint: "http://127.0.0.1:<ephemeral-port>"` plus throwaway credentials reaches THAT server: the SDK receives the server's own HTTP 200 with `content-type: application/x-amz-json-1.0` and exactly the body the handler wrote, and the awsJson1_0 deserializer turns `Table.CreationDateTime` (epoch-seconds encoding) into a JS Date. A client endpoint supplied in the constructor takes precedence over any endpoint environment variable, and loopback traffic never leaves the sandbox, so no SigV4 credential check happens at all on that request. `UnrecognizedClientException` with the message "The security token included in the request is invalid." is the SigV4 framework refusal AWS returns when the signing access key id is not one the account knows; its text is wholly fixed (no request- or state-derived parts), it is NOT one of DescribeTable's modeled errors, and it can only be produced by an AWS service front door — never by a handler's own in-process HTTP server serving a canned 200.

Emulator: the handler's loopback request never reaches the server the handler just started. Every loopback arm of the check comes back as an SDK error object `{ok: false, ctor: "UnrecognizedClientException", name: "UnrecognizedClientException", message: "The security token included in the request is invalid."}` — i.e. the request addressed to 127.0.0.1:<ephemeral-port> was answered by the emulator's DynamoDB endpoint, which refused the handler's throwaway key ("loopbackonly"). In the same invocation the handler's real arm (DescribeTable against the run's table using the ambient credentials) succeeds and deserializes correctly, so credentials, IAM and routing to the emulator all work; what is broken is honoring an endpoint the handler itself chose. Hypothesis: the Lambda sandbox forces all outbound HTTP through the emulator's service address (a transparent proxy/redirect, or an endpoint override applied AFTER the client's explicit endpoint), so an in-sandbox loopback address is resolved to the emulator's service port rather than to the listener inside the sandbox.

Why: a client relying on AWS's behavior gets a request delivered to the wrong server and an authentication error for a call that was never meant to be authenticated. Any handler that talks to something it hosts or co-locates on loopback — a canned/stub HTTP server used by a test or a self-check, a sidecar or agent, a local cache, an extension-style endpoint, a client deliberately pointed at a private endpoint — silently has its traffic hijacked to the emulator and refused. Code that works on AWS fails inside the emulator, and the failure arrives as a credential error, which sends the user hunting a non-existent IAM/signing problem. It also destroys any handler-side control arm: the check here can no longer attribute the real-table timestamp result to the response encoding, because its in-sandbox control is not faithful.

Fix: the Lambda sandbox's network/egress handling in the Lambda runtime component (the part that injects or rewrites the endpoint for SDK traffic from a function). Requests addressed to an address the sandbox itself serves — 127.0.0.1/::1 and any port the handler bound — must be delivered inside the sandbox untouched, with no SigV4 handling and no endpoint substitution; only requests addressed to AWS service endpoints may be rewritten to the emulator, and an endpoint explicitly supplied by the SDK client must not be overridden.

Traces: dynamodb.awsjson-epoch-timestamp-survives-two-deserializers (the loopback control arms epochFloat / numericString / iso8601 / rfc1123; the check's real-AWS arm passes). No open issue covers this: the existing loopback-related issues (e.g. 20261117T055822Z-dynamodb-describeendpoints-publishes-a-bare-loopback-host-port.md) are about addresses a service PUBLISHES, not about a sandbox intercepting a handler's own loopback traffic.

## Evidence
Check: dynamodb.awsjson-epoch-timestamp-survives-two-deserializers [mismatch].

Expected (AWS): the handler's loopback DynamoDBClient, pointed at `http://127.0.0.1:<port>` where the handler's own http.Server answers HTTP 200 / `content-type: application/x-amz-json-1.0` with body `{"Table":{"TableName":"t","TableStatus":"ACTIVE","CreationDateTime":<epoch-seconds>,"ItemCount":0,"TableSizeBytes":0}}`, returns `{ok: true, isDate: true, epochMs: <epoch-seconds>*1000}`; the epochMs agrees with the CreateTable CreationDateTime to within 1 ms.

Actual (emulator): `{'ok': False, 'ctor': 'UnrecognizedClientException', 'name': 'UnrecognizedClientException', 'message': 'The security token included in the request is invalid.'}` — an error class only an AWS/emulator front door can produce, meaning the request left the sandbox and was answered by the emulator's DynamoDB endpoint after SigV4 rejection of the handler's throwaway key. Message text "The security token included in the request is invalid." is entirely fixed text of that error class; nothing in it is request- or state-derived, so the divergence is that the refusal happens at all, on a request that should have been served locally with 200.

Assertion raised, check_dynamodb_awsjson_epoch_timestamp_survives_two_deserializers.py line 376:
  AssertionError: JS misread the loopback epoch encoding: {'ok': False, 'ctor': 'UnrecognizedClientException', ...} -- the loopback arm is not a faithful control, so the failures below could not be attributed to the encoding

Corroborating signal from the same invocation: `js["real"]["ok"] is True` and the real-arm Date/epochMs/tableName/tableStatus assertions all pass, so the sandbox has working credentials and working routing to the emulator — only the handler-chosen loopback endpoint is not honored.

## Steps to Reproduce
1. CreateTable a table and wait for ACTIVE; note CreationDateTime from the CreateTable response.
2. Create an IAM role Lambda can assume, allowing dynamodb:DescribeTable on that table and logs:CreateLogGroup/CreateLogStream/PutLogEvents.
3. CreateFunction with Runtime=nodejs22.x, Handler=index.handler, and an index.mjs that:
   a. starts `http.createServer(...)` on 127.0.0.1 port 0 which answers every request with HTTP 200, `content-type: application/x-amz-json-1.0`, and body `{"Table":{"TableName":"t","TableStatus":"ACTIVE","CreationDateTime":<epoch seconds as a JSON number>,"ItemCount":0,"TableSizeBytes":0}}`;
   b. builds `new DynamoDBClient({region, endpoint: "http://127.0.0.1:" + srv.address().port, credentials: {accessKeyId: "loopbackonly", secretAccessKey: "loopback"}, maxAttempts: 1})` and sends `DescribeTableCommand({TableName: "t"})`;
   c. returns the result, or `{ok:false, ctor, name, message}` on error.
4. Wait for the function to reach Active and Invoke it.
5. Observe the loopback arm: on AWS it is `{ok: true, isDate: true, epochMs: ...}` served by the handler's own server; on the emulator it is `{ok: false, ctor: "UnrecognizedClientException", message: "The security token included in the request is invalid."}`, proving the request was routed to the emulator's DynamoDB endpoint instead of to 127.0.0.1:<port> inside the sandbox.
