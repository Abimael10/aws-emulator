Issue-Source: trace-sweep-divergence
Issue-Trace: sqs-fifo-queue-type-immutability-and-suffix-rules
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sqs-fifo-queue-type-immutability-and-suffix-rules
Issue-Shape-Id: com.amazonaws.sqs#AmazonSQS
Issue-Key: sqs|SetQueueAttributes|4xx->4xx|$headers.x-amzn-query-error
Issue-Related: 20261029T180649Z-sqs-validation-refusals-carry-the-wrong-error-class.md
Issue-Commit: 3a3c6349a202cf2f5d6f86d40ee3e2557e11f042

# SQS SetQueueAttributes rejects the FifoQueue attribute as an unknown attribute name instead of refusing the queue-type change as an invalid attribute value

> _Note: this issue was identified against the code base at commit `3a3c6349a202cf2f5d6f86d40ee3e2557e11f042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetQueueAttributes

## Description
This issue carries 1 cause.

Cause 1 of 1 — SetQueueAttributes carrying the FifoQueue attribute against an existing queue (an attempt to change the queue's type).

AWS: the queue type is immutable after creation, and AWS refuses the attempt as an invalid attribute VALUE, not as an unknown attribute NAME. HTTP 400, __type 'com.amazonaws.sqs#InvalidAttributeValue', query-error header 'InvalidAttributeValue;Sender', message 'Invalid value for the parameter FifoQueue. Reason: Modifying queue type is not supported.'. Smithy: SetQueueAttributes binds both com.amazonaws.sqs#InvalidAttributeValue (httpError 400 defaulted, doc 'A queue attribute value is invalid.') and its sibling com.amazonaws.sqs#InvalidAttributeName (httpError 400 defaulted, doc "The specified attribute doesn't exist."); SetQueueAttributesRequest.Attributes targets QueueAttributeMap and FifoQueue is absent from the operation's documented set of settable attribute names, which is precisely why the emulator's name-based validator trips on it. The message wording is recorded behavior only (the model carries no message text): its FIXED text is the prefix 'Invalid value for the parameter ', the separator '. Reason: ' and the Reason sentence 'Modifying queue type is not supported.'; its only VARIABLE part is the attribute name, taken verbatim from the request's Attributes map. Nothing from the queue's state or from the run (name, id, timestamp) is interpolated.

Emulator: answers HTTP 400 (status is correct) but with the wrong error class and a wholly different message — __type 'com.amazonaws.sqs#InvalidAttributeName', header 'InvalidAttributeName;Sender', message 'Unknown Attribute FifoQueue.'. That is the generic unknown-attribute-name template, so the emulator never reaches an immutability check at all: FifoQueue is simply not in its set of attribute names recognised by SetQueueAttributes, and the refusal is produced by name lookup rather than by a queue-type rule.

Why: AWS deliberately separates 'this attribute does not exist' (InvalidAttributeName) from 'this value is not permitted' (InvalidAttributeValue), and queue-type immutability is the latter. A client that branches on the error class mis-files a legitimate immutability refusal as a typo/unsupported-attribute bug, and the Reason clause it would match to confirm 'you cannot convert a standard queue to FIFO (or vice versa)' is absent entirely, so there is no way to distinguish this case from any other unrecognised attribute name. An emulator-trained matcher keyed on 'Unknown Attribute ' also fails against real AWS.

Fix: the SetQueueAttributes handler's attribute validation must recognise FifoQueue as a known-but-immutable attribute and raise the invalid-attribute-value refusal with AWS's Reason clause, ahead of (not via) the unknown-attribute-name branch; the serializer must then emit __type com.amazonaws.sqs#InvalidAttributeValue and the matching 'InvalidAttributeValue;Sender' query-error header.

Not covered by the open bundle 20261029T180649Z-sqs-validation-refusals-carry-the-wrong-error-class.md: its Cause 5 names the same fixture step but records the emulator answering 'Attribute FifoQueue can be provided only when the queue is created.' (an immutability-aware sentence under the wrong class). The emulator's reply has since drifted to the generic 'Unknown Attribute FifoQueue.', the immutability check is now gone rather than merely misclassified, and the diverging field set has grown to include $headers.x-amzn-query-error — so this cluster's replay key is not carried by that issue and the replay reports it unowned.

Traces: sqs-fifo-queue-type-immutability-and-suffix-rules#task#7.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 304 steps total, 277 passed, 27 mismatched, 13 divergence clusters, score 91.1%. This cluster is the only one the report leaves unowned (owned_by: [], stub_path: ""), affected_count 1.

Divergence key: service=sqs|SetQueueAttributes|status=400->400|keys=$.__type,$.message,$headers.x-amzn-query-error
Fixture/step: sqs-fifo-queue-type-immutability-and-suffix-rules#task#7
Status: expected 400, actual 400 (status agrees)

Expected -> actual, verbatim:
  $.__type: 'com.amazonaws.sqs#InvalidAttributeValue' -> 'com.amazonaws.sqs#InvalidAttributeName'
  $.message: 'Invalid value for the parameter FifoQueue. Reason: Modifying queue type is not supported.' -> 'Unknown Attribute FifoQueue.'
  $headers.x-amzn-query-error: 'InvalidAttributeValue;Sender' -> 'InvalidAttributeName;Sender'

In the expected message, 'Invalid value for the parameter ', '. Reason: ' and 'Modifying queue type is not supported.' are fixed text; 'FifoQueue' is the attribute name echoed from the request's Attributes map. No per-run value (request id, clock time, generated queue name or url, account id) participates in any of the three assertions, so none of the divergence is environmental.

## Steps to Reproduce
1. CreateQueue a FIFO queue (a name ending in .fifo with Attributes={'FifoQueue': 'true'}), or equivalently create a standard queue for the inverse direction.
2. Call SetQueueAttributes on the resulting QueueUrl with Attributes={'FifoQueue': <the opposite value>}, i.e. attempt to change the queue's type after creation.
3. Expect HTTP 400 with __type 'com.amazonaws.sqs#InvalidAttributeValue', header x-amzn-query-error 'InvalidAttributeValue;Sender' and message 'Invalid value for the parameter FifoQueue. Reason: Modifying queue type is not supported.'
4. Observe instead HTTP 400 with __type 'com.amazonaws.sqs#InvalidAttributeName', header 'InvalidAttributeName;Sender' and message 'Unknown Attribute FifoQueue.'
5. Re-replay sqs-fifo-queue-type-immutability-and-suffix-rules#task#7 to confirm the fix; the cluster is a single step, so that one binding covers it.
