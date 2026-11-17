Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sqs.add-permission-nonexistent-queue-rejected sqs.batch-message-operations-on-a-deleted-queue-rejected sqs.list-dead-letter-source-queues-on-a-deleted-queue-rejected sqs.permission-operations-on-a-deleted-queue-rejected sqs.purge-queue-on-a-deleted-queue-rejected sqs.single-message-operations-on-a-deleted-queue-rejected sqs.add-permission-more-than-seven-actions-in-a-statement-rejected sqs.custom-type-message-attribute-value-errors-use-different-type-names sqs.receive-message-attribute-names-star-all-dot-star-select-the-same-set sqs.receive-message-attribute-names-wrong-json-type-rejected
Issue-Shape-Id: com.amazonaws.sqs#AmazonSQS
Issue-Key: sqs.add-permission-nonexistent-queue-rejected|run#8|AssertionError|assert err.get("Message") == _MSG_NONEXISTENT_QUEUE, f"unexpected refusal message {err!r}"
Issue-Key: sqs.batch-message-operations-on-a-deleted-queue-rejected|run#21|AssertionError|assert (read_err.get("Code"), read_err.get("Message")) == (_NOT_FOUND, _NOT_FOUND_MSG), (
Issue-Key: sqs.list-dead-letter-source-queues-on-a-deleted-queue-rejected|run#22|AssertionError|assert (read_err.get("Code"), read_err.get("Message")) == (_NOT_FOUND, _NOT_FOUND_MSG), (
Issue-Key: sqs.permission-operations-on-a-deleted-queue-rejected|run#22|AssertionError|assert (read_err.get("Code"), read_err.get("Message")) == (_NOT_FOUND, _NOT_FOUND_MSG), (
Issue-Key: sqs.purge-queue-on-a-deleted-queue-rejected|run#17|AssertionError|assert (read_err.get("Code"), read_err.get("Message")) == (_NOT_FOUND, _NOT_FOUND_MSG), (
Issue-Key: sqs.single-message-operations-on-a-deleted-queue-rejected|run#19|AssertionError|assert (read_err.get("Code"), read_err.get("Message")) == (_NOT_FOUND, _NOT_FOUND_MSG), (
Issue-Key: sqs.add-permission-more-than-seven-actions-in-a-statement-rejected|run#9|AssertionError|assert code == "OverLimit", f"AddPermission with 8 actions in one statement gave code {code!r}"
Issue-Key: sqs.custom-type-message-attribute-value-errors-use-different-type-names|run#4|AssertionError|assert err.get("Message") == want_message, (
Issue-Key: sqs.receive-message-attribute-names-star-all-dot-star-select-the-same-set|run#12|AssertionError|assert set(got) == set(_ATTRS), (
Issue-Key: sqs.receive-message-attribute-names-wrong-json-type-rejected|run#14|AssertionError|assert err_type == _SER, (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# sqs: five net-new causes — NonExistentQueue keeps the legacy WSDL sentence on six more operations, AddPermission's action cap is the wrong error class, a custom attribute type is truncated in the refusal, MessageAttributeNames '*' projects nothing, and a wrong-typed field is ValidationException instead of SerializationException

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AddPermission

## Description
Five net-new root causes in the SQS service, each grounded in failing checks of this run that are NOT marked ALREADY FILED and are NOT carried by any open issue's replay keys. They are bundled because they all belong to the `sqs` service; a fixer must close all five separately before submitting.

Cause 1 of 5: the AWS.SimpleQueueService.NonExistentQueue refusal still carries the legacy WSDL sentence on the queue-resolving operations these checks exercise.
AWS: Smithy sqs#QueueDoesNotExist is modelled on AddPermission, RemovePermission, PurgeQueue, GetQueueAttributes, ListDeadLetterSourceQueues, SendMessage, SendMessageBatch, ReceiveMessage, DeleteMessage, DeleteMessageBatch and ChangeMessageVisibility(Batch) with httpError 400 and awsQueryError code 'AWS.SimpleQueueService.NonExistentQueue' (doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted."). The recorded refusal is HTTP 400, Code 'AWS.SimpleQueueService.NonExistentQueue', QueryErrorCode 'QueueDoesNotExist', Type 'Sender', Message 'The specified queue does not exist.' — the entire message is fixed text for this class; it carries no value from the request or from state.
Emulator: the status, Code, QueryErrorCode and Type are right, but the Message is 'The specified queue does not exist for this wsdl version.' — the legacy query-protocol clause.
Why: the code is shared by every queue-resolution failure, so the message is what a client logs and asserts on; AWS has not served the 'for this wsdl version' sentence on these paths, so fixtures and assertions written against AWS do not match.
Fix: the single NonExistentQueue message constant used by the queue-resolution helper these handlers share — one site, so the operations cannot drift apart.
Traces: sqs.add-permission-nonexistent-queue-rejected, sqs.batch-message-operations-on-a-deleted-queue-rejected, sqs.list-dead-letter-source-queues-on-a-deleted-queue-rejected, sqs.permission-operations-on-a-deleted-queue-rejected, sqs.purge-queue-on-a-deleted-queue-rejected, sqs.single-message-operations-on-a-deleted-queue-rejected. Open issues 20261115T171209Z-sqs-the-queuedoesnotexist-refusal-carries-the-legacy-wsdl.md, 20261115T165311Z-sqs-the-aws-simplequeueservice-nonexistentqueue-refusal-carries-the.md and 20261115T201003Z-sqs-nine-queue-resolving-wire-operations-refuse-with.md describe the same wording, but none of them carries these six checks' replay keys, so these checks are not covered by them; the single-constant fix closes all of them together and these six should be re-replayed with it.

Cause 2 of 5: AddPermission refuses a statement naming more than seven actions with the wrong error class and message.
AWS: Smithy sqs#AddPermission models OverLimit (httpError 403, awsQueryError code 'OverLimit'; doc: "The specified action violates a limit ... AddPermission returns this error if the maximum number of permissions for the queue is reached"). The recorded refusal for eight actions in one statement is Code 'OverLimit' with Message '8 Actions were found, maximum allowed is 7. ' — fixed text apart from the submitted action count; the trailing space is part of AWS's message.
Emulator: the call is refused, but with Code 'InvalidParameterValue' — a 400-class input rejection rather than the modelled 403 OverLimit, and not the message above.
Why: a client that handles limit violations (OverLimit) separately from malformed input (InvalidParameterValue) misclassifies the failure, and the response's HTTP status is wrong as well (400 vs 403). The seven-action maximum is a documented limit, not a syntax error.
Fix: the AddPermission handler's action-count validation — raise the modelled OverLimit error with AWS's message rather than the generic invalid-parameter-value path.
Traces: sqs.add-permission-more-than-seven-actions-in-a-statement-rejected.

Cause 3 of 5: SendMessage's missing-value refusal for a custom-typed message attribute reports only the base type, dropping the custom label.
AWS: a message attribute DataType may be a base type with a custom label ('String.custom', 'Binary.custom'). The model does not state the refusal text, so this is recorded behavior: for a missing value the message is "Message (user) attribute '<name>' must contain a non-empty value of type '<the FULL DataType as submitted>'." — so 'String.custom' and 'Binary.custom' appear in full; for a value placed in the wrong field the message is "Message (user) attribute '<name>' with type '<BASE type only>' must use field '<base type>'." The attribute name and the type string come from the request; the rest is fixed. Both are Code 'InvalidParameterValue'.
Emulator: the code is right, but the missing-value message names only the base type — "Message (user) attribute 'N' must contain a non-empty value of type 'String'." where AWS says 'String.custom'. The two refusal classes are therefore indistinguishable in the emulator, and a caller cannot see which DataType it actually sent.
Why: a client debugging a custom-typed attribute is shown a type it did not submit; and a test that distinguishes the two messages (full type for a missing value, base type for a misplaced field) cannot pass.
Fix: the SendMessage/SendMessageBatch message-attribute validation — carry the submitted DataType verbatim into the missing-value message, keeping the base-type-only form for the wrong-field message.
Traces: sqs.custom-type-message-attribute-value-errors-use-different-type-names.

Cause 4 of 5: ReceiveMessage with MessageAttributeNames=['*'] projects no attributes at all.
AWS: Smithy sqs#ReceiveMessage takes MessageAttributeNames (a list of MessageAttributeName). Recorded behavior: '*', 'All' and '.*' all select EVERY user message attribute, and a name prefix or exact name selects just that subset (with MD5OfMessageAttributes recomputed over the projected subset, not echoed from SendMessage).
Emulator: '*' selects nothing — a message sent with attributes 'order.id' and 'status' comes back with an empty attribute map, while 'All' works.
Why: '*' is the form many SDK samples and generated clients use; a consumer written against it silently loses every message attribute, so routing, filtering and tracing that depend on attributes break with no error anywhere.
Fix: the ReceiveMessage handler's attribute-name selector — treat '*', 'All' and '.*' as the select-all forms alongside prefix and exact-name selection.
Traces: sqs.receive-message-attribute-names-star-all-dot-star-select-the-same-set.

Cause 5 of 5: a JSON-protocol ReceiveMessage whose MessageAttributeNames entries are not strings is refused as ValidationException instead of SerializationException.
AWS: MessageAttributeNameList's member is a string, so a request body in which an entry (or the field itself) has the wrong JSON type fails DESERIALIZATION, before input validation. Recorded behavior: HTTP 400 with __type '...#SerializationException' and a message naming the deserialization fault — 'NUMBER_VALUE cannot be converted to String' for a numeric entry, 'Start of structure or map found where not expected' for a nested-structure entry, and 'Unexpected field type' when MessageAttributeNames itself is not a JSON array — and the refusal body carries exactly the two members __type and Message, nothing else.
Emulator: the status is 400 but __type is 'ValidationException', so the refusal is classified as an input-validation failure rather than a wire deserialization failure (and the message/body shape for this class is therefore not AWS's either).
Why: clients and SDKs distinguish a malformed wire payload (SerializationException — a bug in the caller's serializer, never retryable, never a per-member validation report) from a well-formed payload with a bad value (ValidationException). Mapping the former onto the latter misleads the caller about where the fault is, and the refusal body's members diverge.
Fix: the JSON-protocol request deserializer for the SQS wire (the shared deserialization-error path) — report a type mismatch in the request body as SerializationException with AWS's message and a body of only __type and Message, rather than routing it into the operation's validation.
Traces: sqs.receive-message-attribute-names-wrong-json-type-rejected.

## Evidence
Cause 1 — six checks, all reporting the same wording on the deleted/nonexistent queue:
  sqs.add-permission-nonexistent-queue-rejected: `AssertionError: unexpected refusal message {'Message': 'The specified queue does not exist for this wsdl version.', 'Code': 'AWS.SimpleQueueService.NonExistentQueue', 'QueryErrorCode': 'QueueDoesNotExist', 'Type': 'Sender'}` (expected Message 'The specified queue does not exist.').
  sqs.batch-message-operations-on-a-deleted-queue-rejected, sqs.list-dead-letter-source-queues-on-a-deleted-queue-rejected, sqs.permission-operations-on-a-deleted-queue-rejected, sqs.purge-queue-on-a-deleted-queue-rejected, sqs.single-message-operations-on-a-deleted-queue-rejected: all report `GetQueueAttributes on the deleted queue answered {'Message': 'The specified queue does not exist for this wsdl version.', 'Code': 'AWS.SimpleQueueService.NonExistentQueue', ...} within 60.0s`, each check asserting (Code, Message) == ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.').

Cause 2 — sqs.add-permission-more-than-seven-actions-in-a-statement-rejected (mismatch):
  Expected (AWS): Code 'OverLimit' (HTTP 403) and Message '8 Actions were found, maximum allowed is 7. ' (trailing space included), the statement NOT added to the queue's policy.
  Actual (emulator): `AssertionError: AddPermission with 8 actions in one statement gave code 'InvalidParameterValue'` (check file line 57).

Cause 3 — sqs.custom-type-message-attribute-value-errors-use-different-type-names (mismatch):
  Expected (AWS): for {'DataType': 'String.custom'} with no value -> Code 'InvalidParameterValue', Message "Message (user) attribute 'N' must contain a non-empty value of type 'String.custom'."
  Actual (emulator): Message "Message (user) attribute 'N' must contain a non-empty value of type 'String'." (check file line 72). The same check also grades 'Binary.custom' and the two wrong-field variants, which must keep the base-type-only wording.

Cause 4 — sqs.receive-message-attribute-names-star-all-dot-star-select-the-same-set (mismatch):
  Expected (AWS): ReceiveMessage(MessageAttributeNames=['*']) projects both attributes ['order.id', 'status'].
  Actual (emulator): `AssertionError: MessageAttributeNames=['*'] returned [], expected ['order.id', 'status']` (check file line 92).

Cause 5 — sqs.receive-message-attribute-names-wrong-json-type-rejected (mismatch):
  Expected (AWS): a SigV4-signed POST with content-type application/x-amz-json-1.0, x-amz-target AmazonSQS.ReceiveMessage and body {"QueueUrl": ..., "MessageAttributeNames": [5]} -> HTTP 400, __type ending '#SerializationException', Message 'NUMBER_VALUE cannot be converted to String', body members exactly {__type, Message}; [{"a":"b"}] -> 'Start of structure or map found where not expected'; MessageAttributeNames: 42 -> 'Unexpected field type'.
  Actual (emulator): `AssertionError: MessageAttributeNames=[5] (a number entry) gave __type 'ValidationException', expected 'SerializationException'` (check file line 88).

## Steps to Reproduce
Against the emulator's SQS endpoint (any region/account):

Cause 1: CreateQueue then DeleteQueue (or use a never-created queue URL). Call AddPermission, RemovePermission, PurgeQueue, GetQueueAttributes, ListDeadLetterSourceQueues, SendMessage/SendMessageBatch, ReceiveMessage, DeleteMessage/DeleteMessageBatch and ChangeMessageVisibility(Batch) on it: each must answer Code 'AWS.SimpleQueueService.NonExistentQueue' with Message exactly 'The specified queue does not exist.'; the emulator appends ' for this wsdl version'.

Cause 2: CreateQueue. AddPermission(QueueUrl, Label='L8', AWSAccountIds=[<account>], Actions=['SendMessage','ReceiveMessage','DeleteMessage','ChangeMessageVisibility','GetQueueAttributes','GetQueueUrl','PurgeQueue','DeleteQueue']) -> must answer Code 'OverLimit' / HTTP 403 with '8 Actions were found, maximum allowed is 7. '; the emulator answers 'InvalidParameterValue'. The same label with the first seven actions must be granted and appear in the queue's Policy with actions 'SQS:<name>'.

Cause 3: CreateQueue. SendMessage(MessageAttributes={'N': {'DataType': 'String.custom'}}) -> Message must name 'String.custom'; the emulator says 'String'. Repeat with 'Binary.custom'. Then SendMessage(MessageAttributes={'N': {'DataType': 'Binary.custom', 'StringValue': 'abc'}}) -> message must name only the base type 'Binary'. A well-formed {'DataType': 'String.custom', 'StringValue': 'ok-value'} must be accepted and round-trip unchanged.

Cause 4: CreateQueue; SendMessage with MessageAttributes {'order.id': String 'A-1', 'status': String 'open'}. ReceiveMessage(MessageAttributeNames=['*']) -> the emulator returns no MessageAttributes; AWS returns both. Repeat with ['All'] and ['.*'] — all three must select the same set, and a prefix such as ['order.'] must select only 'order.id' with MD5OfMessageAttributes recomputed over that subset.

Cause 5: CreateQueue; send one message. POST a SigV4-signed request to the SQS endpoint with content-type 'application/x-amz-json-1.0', x-amz-target 'AmazonSQS.ReceiveMessage' and body {"QueueUrl": "<url>", "MessageAttributeNames": [5]} -> must answer 400 with __type '...#SerializationException' and Message 'NUMBER_VALUE cannot be converted to String', the body carrying only __type and Message. Repeat with [{"a":"b"}] and with the field set to 42. A well-formed ReceiveMessage must still return the message afterwards.
