Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sqs.sse-rejects-anonymous-sendmessage-that-an-unencrypted-queue-accepts sqs.send-message-malformed-awstraceheader-accepted-then-dropped sqs.query-protocol-refusals-are-xml-error-responses-rejected
Issue-Shape-Id: com.amazonaws.sqs#AmazonSQS
Issue-Key: sqs|SendMessage|UnknownService
Issue-Key: sqs.send-message-malformed-awstraceheader-accepted-then-dropped|_error#4|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: sqs.query-protocol-refusals-are-xml-error-responses-rejected|run#19|AssertionError|assert got == (400, "ErrorResponse", "Sender", "InvalidAction",
Issue-Commit: 2172728595665e881665fe4cb0e962a1a0f3fd3b

# 3 causes left open by the SQS protocol-layer fix: an anonymous SendMessage never reaches a handler, one step of the trace-header check expects a refusal this emulator serves, and the Query protocol's InvalidAction sentence is not AWS's

> _Note: this report is the remainder of
> `issues/things-that-now-work/20261109T185037Z-sqs-5-net-new-causes-the-query-protocol-does.md`,
> whose other causes (the Query protocol answering XML, the raw-JSON unresolvable-target
> refusal and CreateQueue's idempotence against current attributes) are fixed and tested. It
> was filed by the fixer of that issue so the causes that still diverge are not lost with it.
> The code base can have drifted since commit `2172728595665e881665fe4cb0e962a1a0f3fd3b`._

## Operation
SendMessage

## Description

Cause 1 of 3 — an anonymous (unsigned) SendMessage never reaches a handler, so the queue
policy cannot decide it. AWS: an anonymous SendMessage to a queue whose policy permits the
anonymous principal SUCCEEDS, while the same call to an SSE-KMS queue is refused by SQS
itself; which of the two happens is the queue's own resource policy (and its KMS key policy)
to decide, in the handler. Emulator: the request is refused BEFORE any handler runs, with
`AccessDenied — "Sign the request with the emulator credentials"`, whenever request
authentication is on. The refusal is the framework's, not this service's:
`vendor/smithy-server-runtime/src/multi_service_router.rs::MultiServiceRouter::authenticate`
returns early only when the modelled operation's auth-scheme list is EMPTY, and
`vendor/server/src/request_authentication.rs::authenticator_for` refuses every request no
registered authenticator `recognizes` — which is every unsigned request. The SQS model
declares `aws.auth#sigv4` service-wide with no `@optionalAuth` on any operation
(`smithy_query(query="digest")`), and `AGENTS.md` forbids a service from answering
`auth_schemes()` with an empty list ("an operation is unsigned only when the model makes it
so"), so no seam inside `services/` can admit such a request. Required core change: a seam
on `Service`/`ErasedServiceDefinition` by which a service whose handlers authorize from a
RESOURCE policy admits an unsigned request — marking the caller anonymous for the
authorization gate to judge — instead of the authenticator refusing it before dispatch.
Until that exists the behaviour cannot be emulated at all; nothing in the SQS crate should
paper over it by re-wording the refusal. Traces:
sqs.sse-rejects-anonymous-sendmessage-that-an-unencrypted-queue-accepts.

Cause 2 of 3 — one step of the trace-header check expects a refusal this emulator serves.
`SendMessage` now accepts a `MessageSystemAttributes` `AWSTraceHeader` whose value it cannot
read as a trace header and does not propagate it (the first steps of the check pass: the
send succeeds and a later receive reports no `AWSTraceHeader`). A LATER step of the same
check (`_error#4`) expects some call to be REFUSED and the emulator answers it 200. The
recording withholds the request; the answer it prints carries neither
`MD5OfMessageAttributes` nor `MD5OfMessageSystemAttributes`, so that step carries no
propagated attribute, and the refusal AWS makes there could not be identified from the
evidence. Expected: the step's call refused as AWS refuses it. Actual: HTTP 200 with a
`MessageId`. Whoever picks this up needs the request that step sends — it decides whether
the rule is a bound on the value's text, on its size, or on something else entirely — and
must not invent a refusal to satisfy the assertion. Traces:
sqs.send-message-malformed-awstraceheader-accepted-then-dropped.

Cause 3 of 3 — the AWS Query protocol's `InvalidAction` sentence is not the one AWS words.
AWS: a form-encoded request naming an `Action` this API does not publish is refused HTTP 400
with `<ErrorResponse><Error><Type>Sender</Type><Code>InvalidAction</Code><Message>…`.
Emulator: the status, the envelope, the fault side and the code all match; only the sentence
differs. It currently answers the AWS Query protocol's documented wording, the same sentence
the tree's other AWS Query surface answers
(`services/service-com-amazonaws-simpledbv2-simpledbv2/src/service/sdb_query_api/envelope.rs::invalid_action`):
"The action &lt;Action&gt; is not valid for this web service." Three candidate sentences were
tried against the recording ("…is not valid for this web service.", "Could not find
operation &lt;Action&gt; for version 2012-11-05", "…is not valid for this wsdl version.") and
none matched, and the corpus withholds the expected text, so the wording needs the recorded
response to settle. Fix:
`services/service-com-amazonaws-sqs-amazonsqs/src/service/query_protocol.rs::no_such_action_response`.
Every other row of that check passes (a queue that does not exist by url and by name, an
attribute value, an unknown attribute name, a missing parameter, an invalid body character,
and a purge already running at 403). Traces:
sqs.query-protocol-refusals-are-xml-error-responses-rejected.

## Evidence
Cause 1: botocore.exceptions.ClientError: An error occurred (AccessDenied) when calling the SendMessage operation: Sign the request with the emulator credentials
Cause 2: AssertionError: expected the call to be refused, but it succeeded: {'MD5OfMessageBody': '…', 'MessageId': '…', 'ResponseMetadata': {…}}
Cause 3: AssertionError: Query with an unknown Action answered (400, 'ErrorResponse', 'Sender', 'InvalidAction', 'The action NoSuchAction is not valid for this web service.')

## Steps to Reproduce
Cause 1: SendMessage anonymously (an unsigned client) to an unencrypted queue whose policy allows the anonymous principal, with request authentication on — expect the send to succeed; then the same call to an SSE-KMS queue — expect SQS's own authorization refusal.
Cause 2: the steps of sqs.send-message-malformed-awstraceheader-accepted-then-dropped past the receive that checks the attribute was dropped.
Cause 3: POST a form-encoded `Action=NoSuchAction&Version=2012-11-05` to the SQS endpoint and read `<Error><Message>`.
