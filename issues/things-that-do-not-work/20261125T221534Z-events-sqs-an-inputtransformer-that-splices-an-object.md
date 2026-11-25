Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs.events-delivery-and-failure-object-in-quoted-template-string-dead-letters-as-invalid-json
Issue-Key: events__sqs.events-delivery-and-failure-object-in-quoted-template-string-dead-letters-as-invalid-json|_drain#14|Timeout|resp = sqs.receive_message(
Issue-Commit: 611dea4dfec80462d5a317d136054351d69dd557

# events__sqs: an InputTransformer that splices an object into a quoted JSON string is never refused at delivery nor dead-lettered as ERROR_CODE=INVALID_JSON

> _Note: this issue was identified against the code base at commit `611dea4dfec80462d5a317d136054351d69dd557`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTargets

## Description
One net-new root cause.

Cause 1 of 1: EventBridge's target-delivery path does not produce the invalid-JSON refusal + dead-letter outcome for a target whose InputTransformer renders a non-JSON payload. Failing check: events__sqs.events-delivery-and-failure-object-in-quoted-template-string-dead-letters-as-invalid-json

AWS: Smithy (com.amazonaws.eventbridge): Target.InputTransformer is an InputTransformer with InputPathsMap (map, @length 0..100, values are JSON paths) and InputTemplate (@required, @length 1..8192). The InputTemplate doc states the template is a placeholder-substitution template and that when it "is a JSON object (surrounded by curly braces)" the only stated restriction is that "The placeholder cannot be used as an object key" — a placeholder appearing INSIDE a quoted string value is therefore accepted by PutTargets, and the model declares no delivery-time validation error for it. Target.DeadLetterConfig is a DeadLetterConfig whose Arn "is the ARN of the SQS queue specified as the target for the dead-letter queue", and its doc points at "Using dead-letter queues to process undelivered events"; Target.RetryPolicy is "the retry policy configuration to use for the dead-letter queue".

Recorded behavior only, for what happens at delivery: EventBridge substitutes the extracted value verbatim, with no JSON escaping. So for InputPathsMap {"order": "$.detail.order", "n": "$.detail.n"} and InputTemplate '{"objq":"<order>","n":<n>}', the object placeholder inside the quotes renders as the object's compact JSON spliced between the template's quote characters, i.e. a payload of the shape '{"objq":"{<the order object as compact JSON>}","n":<n>}', which is not valid JSON. AWS then:
  1. ACCEPTS the PutTargets call (FailedEntryCount 0) — the failure belongs to delivery, not to registration;
  2. delivers NOTHING to that target's queue;
  3. writes the rendered (malformed) payload as the body of one message on that target's DeadLetterConfig queue, carrying message attributes RULE_ARN, TARGET_ARN, ERROR_CODE and ERROR_MESSAGE. ERROR_CODE is the fixed string "INVALID_JSON" and ERROR_MESSAGE is the fixed string "Invalid input for target." (fixed text, verbatim, including the trailing period). RULE_ARN and TARGET_ARN are values taken from state/request: the ARN of the rule carrying the target, and the ARN of the refused target respectively — never a literal from any one run. The dead-letter body is the RENDERED payload, not the source event envelope (it contains no "source"/"detail" members);
  4. still delivers the same event to a SIBLING target on the SAME rule whose InputTemplate is well formed (e.g. '{"obj":<order>,"n":<n>}' renders the object unquoted and valid), so one broken target does not suppress the rule's other targets;
  5. once the template is repaired in place by a second PutTargets for the same target Id (placeholder unquoted), the SAME queue begins receiving the rendered object again — no replacement of the target or the rule is needed.

Emulator: none of 2-5 is observable. The check's barrier — one emitted event that reaches BOTH the healthy sibling queue and the shared dead-letter queue — is never satisfied; the poll makes no progress on repeated PutEvents and the check is cut off by the harness budget with "Error: Check timed out after 60.0s". Consequently no DLQ message is produced for the broken target, no TARGET_ARN / RULE_ARN / ERROR_CODE=INVALID_JSON / ERROR_MESSAGE="Invalid input for target." attributes are ever served, the rendered-payload-vs-envelope distinction cannot be observed, and the repair leg (PutTargets with the unquoted placeholder, then the target receiving again) is never reached. The check never asserts, so the emulator is diverging at or before the first delivery of the invalid-JSON rendering.

Why: a client relying on AWS's behavior loses its entire error-propagation contract for InputTransformer rendering. A consumer polling the dead-letter queue sees nothing, so an undeliverable transformed event is silently lost rather than surfaced with the INVALID_JSON classification operators triage on; alerting keyed on the ERROR_CODE / ERROR_MESSAGE / TARGET_ARN attributes never fires; and because the broken target is indistinguishable from an idle one, a template bug that AWS reports within seconds is invisible in the emulator. Conversely, any code that asserts the broken target receives nothing cannot verify that either.

Fix: EventBridge's rule target-dispatch path — (a) the InputTransformer rendering step, which must substitute the extracted value verbatim (no escaping) and then validate the rendered payload as JSON before handing it to the target; (b) the per-target refusal/dead-letter step, which on a rendering that is not valid JSON must skip the target delivery, honour the target's RetryPolicy, and send the rendered payload to the target's DeadLetterConfig queue stamped with the RULE_ARN/TARGET_ARN/ERROR_CODE/ERROR_MESSAGE message attributes; and (c) per-target isolation in that dispatcher, so a refused target does not affect sibling targets of the same rule. Registration-time validation in PutTargets must NOT be tightened: the template has to keep being accepted.

Related open issues that do NOT cover this cause (different root causes, named here only to avoid duplicate triage): 20261124T132051Z-events-sqs-5-causes-puttargets-refusal-messages-for.md covers PutTargets REGISTRATION-time refusal wording for a non-JSON InputTemplate/Input (this cause is the opposite — the template must be accepted and fail at delivery); 20261123T065749Z-events-s3-sqs-eventbridge-dead-letters-a-delivery-to-an-sqs.md and 20261124T195017Z-elbv2-events-sqs-an-eventbridge-sqs-target-whose.md cover the ERROR_CODE chosen for a PERMISSION/NO_RESOURCE dead-letter, not an INVALID_JSON rendering failure.

Traces: events__sqs.events-delivery-and-failure-object-in-quoted-template-string-dead-letters-as-invalid-json (verdict: mismatch; detail "Error: Check timed out after 60.0s").

## Evidence
Expected (AWS):
- PutTargets with two targets on one rule — target Id "bad" using InputTemplate '{"objq":"<order>","n":<n>}' and target Id "good" using '{"obj":<order>,"n":<n>}', both with the same DeadLetterConfig.Arn and RetryPolicy {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60} — returns FailedEntryCount 0.
- After PutEvents of an event matching the rule ({"order": {...}, "n": <k>} in Detail):
  * the "good" target's queue receives the valid rendering (its "obj" member equals the order object);
  * the "bad" target's queue receives NOTHING;
  * the dead-letter queue receives one message whose Body is the malformed rendering ('{"objq":"<order object as compact JSON>","n":<k>}', containing no "source"/"detail" member) and whose MessageAttributes are RULE_ARN = the rule's ARN, TARGET_ARN = the "bad" target's queue ARN, ERROR_CODE = "INVALID_JSON", ERROR_MESSAGE = "Invalid input for target.".
- After a repair PutTargets that re-registers Id "bad" with the unquoted template, the same "bad" queue receives the valid rendering.

Actual (emulator):
- The check never reaches any of those assertions. Its barrier poll — "one event reaching the healthy sibling AND the DLQ" — never becomes true, so the check is terminated by the harness: `Error: Check timed out after 60.0s` (suite verdict: mismatch). No dead-letter message for the broken target, and therefore none of the four message attributes (RULE_ARN, TARGET_ARN, ERROR_CODE, ERROR_MESSAGE) is ever observed; the repaired-target leg is never exercised.

Fixed text vs. variable parts of the diverging message: "INVALID_JSON" (ERROR_CODE) and "Invalid input for target." (ERROR_MESSAGE) are FIXED text and must match character for character. RULE_ARN and TARGET_ARN are VARIABLE — taken from the rule named in PutTargets and from the refused target's Arn in the request/state. The DLQ body is likewise variable: it is the template rendered against the event just published, not any recorded literal.

## Steps to Reproduce
1. events.PutRule(Name=<unique>, EventPattern={"source": ["<unique-source>"]}) and keep the returned RuleArn.
2. Create three SQS queues (bad, good, dlq); on each, SetQueueAttributes with a Policy allowing Principal {"Service": "events.amazonaws.com"} Action sqs:SendMessage on the queue ARN with Condition ArnEquals aws:SourceArn = [RuleArn].
3. events.PutTargets(Rule=<rule>, Targets=[
     {Id: "bad",  Arn: <bad queue ARN>,  DeadLetterConfig: {Arn: <dlq ARN>}, RetryPolicy: {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60},
      InputTransformer: {InputPathsMap: {"order": "$.detail.order", "n": "$.detail.n"}, InputTemplate: '{"objq":"<order>","n":<n>}'}},
     {Id: "good", Arn: <good queue ARN>, DeadLetterConfig: {Arn: <dlq ARN>}, RetryPolicy: {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60},
      InputTransformer: {InputPathsMap: {"order": "$.detail.order", "n": "$.detail.n"}, InputTemplate: '{"obj":<order>,"n":<n>}'}}])
   -> expect FailedEntryCount 0.
4. events.PutEvents(Entries=[{Source: "<unique-source>", DetailType: "Splice", Detail: '{"order": {"id": "o-17", "qty": 3}, "n": 1}'}]) -> expect FailedEntryCount 0. Repeat with an incrementing n while draining.
5. ReceiveMessage (MaxNumberOfMessages=10, MessageAttributeNames=["All"], AttributeNames=["All"]) on all three queues.
   -> good queue: one message whose body's "obj" equals the order object.
   -> bad queue: empty.
   -> dlq: one message per emitted event, body = the malformed rendering, MessageAttributes RULE_ARN / TARGET_ARN = bad queue ARN / ERROR_CODE = "INVALID_JSON" / ERROR_MESSAGE = "Invalid input for target.".
   Observed on the emulator: the dlq never yields a message, so this step never converges and the check is killed at the 60 s harness cap.
6. events.PutTargets(Rule=<rule>, Targets=[{Id: "bad", Arn: <bad queue ARN>, DeadLetterConfig: {Arn: <dlq ARN>}, RetryPolicy: {...}, InputTransformer: {InputPathsMap: {...}, InputTemplate: '{"obj":<order>,"n":<n>}'}}]), then PutEvents again -> the bad queue must now receive the valid rendering.
