Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pipes.tag-resource-invalid-key-rejected
Issue-Shape-Id: com.amazonaws.pipes#Pipes
Issue-Key: pipes.tag-resource-invalid-key-rejected|run#18|AssertionError|assert message == expected_message, f"unexpected TagResource message {message!r}"
Issue-Commit: 565cc62b717e712e0a961838cd34964d5eab23dc

# pipes TagResource tag-key-length refusal nests a JSON {"message": ...} document (with non-JSON \' escapes) inside Error.Message instead of carrying the bare validation sentence

> _Note: this issue was identified against the code base at commit `565cc62b717e712e0a961838cd34964d5eab23dc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Cause 1 of 1: the pipes tagging layer's constraint-validation refusal puts a whole serialized JSON error DOCUMENT into the error's `message` member, so a client reading the refusal's message gets a JSON object literal (further mangled by a non-standard apostrophe escape) rather than the validation sentence AWS words. Failing check: pipes.tag-resource-invalid-key-rejected.

AWS: Smithy (services/service-com-amazonaws-pipes-pipes/smithy-model/com-amazonaws-pipes-pipes.json) models TagResource with input `resourceArn` (PipeArn, required) and `tags` (TagMap, @length 1..50) whose key targets TagKey (@length 1..128), and declares only ValidationException (400), NotFoundException (404) and InternalException (500). The constraint refusal itself is recorded behavior only: the recording shows HTTP 400 with error code `BadRequestException` — that code and status are what the emulator already serves and they match. What diverges is the message. For a tag map carrying a key longer than TagKey's 128-character maximum, the refusal's message is the single validation sentence:
  1 validation error detected: Value '{<key>=<value>}' at 'addedTags' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]
Fixed text: `1 validation error detected: ` (the count is the number of violating members), `Value '`, `' at '`, `' failed to satisfy constraint: Map keys must satisfy constraint: [`, and the bound clauses `Member must have length less than or equal to <max>, Member must have length greater than or equal to <min>]`, whose numbers are TagKey's modelled @length bounds (128 and 1) and not request values. Variable parts, taken from the request: the echoed member value rendered `{k1=v1, k2=v2}` over every tag entry the request carried, and the member name `addedTags` (the tag-validation name for TagResource's tags, `removedTagKeys` for UntagResource). The apostrophes around the value and the member name are plain single quotes — AWS does not backslash-escape them. The sentence itself is the value of the error's `message` member; the `{"message": ...}` object is the response BODY frame the restJson error serializer emits around it, not a value nested inside the member.

Emulator: returns the right status (400) and the right code (BadRequestException), but the message is the frame text itself, nested one level too deep and with apostrophes escaped: Error.Message is literally
  {"message": "1 validation error detected: Value \'{<key>=<value>}\' at \'addedTags\' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]"}
The nesting is deliberate in the code: services/service-com-amazonaws-pipes-pipes/src/state.rs builds every tag refusal through `one_validation_error` -> `validation_document`, which serialises the sentence as a JSON string and then wraps it in `format!("{{\"message\": {}}}", quoted.replace('\'', "\\'"))`; `crate::state::tag_refusal` hands that string to `TagResourceError::bad_request` in src/ops/tag_resource.rs, and the error serializer then writes it as the value of the single `message` member. The `\'` substitution is not a JSON escape at all, so the nested document is not even parseable as JSON. Because `tag_refusal`/`one_validation_error` are shared, the same nesting reaches UntagResource's `removedTagKeys` refusal and CreatePipe's `tags` refusal, and every other bound/validation sentence routed through `validation_document`.

Why: the message is a requirement of the error class. A client that logs, matches on, or asserts the refusal text sees a JSON object literal where AWS puts a sentence, so human-readable error reporting shows an escaped blob; a client that (as the shape of this error class invites) parses the refusal body's `message` field to recover the sentence recovers another document instead and must unwrap twice, and a parse of that inner document fails outright because `\'` is an invalid JSON escape. The failure is silent: the call is correctly refused with the correct code, so only the text betrays it and error-branching code takes the wrong branch.

Fix: the pipes tagging validation-message layer — `validation_document` / `one_validation_error` in services/service-com-amazonaws-pipes-pipes/src/state.rs, which every tag refusal reaches through `tag_refusal` before `ops/tag_resource.rs` (and `ops/untag_resource.rs`, `ops/create_pipe.rs`) lifts it into `Scaffolded::BadRequest`. The sentence must travel as the message member's own value, with no `{"message": ...}` wrapper added by the handler and no apostrophe rewriting; the single-member `{"message": "..."}` body is produced once by the error serializer in src/scaffolded.rs (BAD_REQUEST_EXCEPTION_SCHEMA), which already escapes whatever the member needs escaped.

Note the conflicting prior work, so a fixer reconciles rather than flips the behavior back and forth: fixed issue 20261026T024031Z-pipes-tagging-refusals-the-badrequestexception-body-is-the.md and still-open issue 20261117T090851Z-pipes-the-badrequestexception-message-for-tag-key-length.md both assert the message must BE the `{"message": ...}` document, and the current nesting is that assertion implemented. The check filed here (pipes.tag-resource-invalid-key-rejected) reads `e.response["Error"]["Message"]` and requires the bare sentence. Hypothesis: the two groundings differ in where they read — the envelope is the recorded BODY and the sentence is the recorded `message` member — so emitting the sentence as the member and letting the serializer frame it satisfies both readings; it cannot be satisfied by nesting.

Traces: pipes.tag-resource-invalid-key-rejected (TagResource, 129-character map key refused at 'addedTags'). The check's other legs pass: the positive control tag persists, the code is BadRequestException, the oversized key is not persisted and the earlier valid tag survives — only the message assertion diverges.

## Evidence
Check pipes.tag-resource-invalid-key-rejected, step: TagResource(resourceArn=<pipe arn>, tags={<129-char key>: <value>}) on a STOPPED SQS->SQS pipe.

Expected (AWS, recorded) — code BadRequestException, HTTP 400, Error.Message:
  1 validation error detected: Value '{<key>=<value>}' at 'addedTags' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]

Actual (emulator) — code BadRequestException, HTTP 400 (both correct), Error.Message:
  {"message": "1 validation error detected: Value \'{<key>=<value>}\' at \'addedTags\' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]"}

  AssertionError: unexpected TagResource message '{"message": "1 validation error detected: Value \\'{kkkk…(129 k's)…=irrelevant}\\' at \\'addedTags\\' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]"}'
  (check_pipes_tag_resource_invalid_key_rejected.py line 65: assert message == expected_message)

Divergences: (a) the message is an entire `{"message": "..."}` document instead of the sentence; (b) inside it, every apostrophe is rewritten to `\'`, which is not a valid JSON escape, so the nested document cannot be parsed either. The 129-'k' key and the value 'irrelevant' are the check's probe values filling the template's variable slots; the 128/1 bounds, the member name 'addedTags' and all fixed clauses are the requirement. Code and status match AWS.

Source of the nesting: services/service-com-amazonaws-pipes-pipes/src/state.rs, `validation_document` (wraps the sentence in the document and applies `.replace('\'', "\\'")`), reached from `one_validation_error` -> `tag_refusal`, whose result src/ops/tag_resource.rs passes to `TagResourceError::bad_request`.

## Steps to Reproduce
1. Create an SQS source queue and an SQS target queue, and an IAM role trusted by pipes.amazonaws.com with sqs:ReceiveMessage/DeleteMessage/GetQueueAttributes on the source and sqs:SendMessage on the target.
2. CreatePipe(Name=<name>, Source=<source arn>, Target=<target arn>, RoleArn=<role arn>, DesiredState='STOPPED'); poll DescribePipe until CurrentState is STOPPED. Keep the returned Arn.
3. Positive control: TagResource(resourceArn=<pipe arn>, tags={'valid-key': 'valid-value'}) then ListTagsForResource — the tag is present (this passes today).
4. TagResource(resourceArn=<pipe arn>, tags={'k'*129: 'irrelevant'}). The call is refused 400 BadRequestException as AWS does, but read e.response['Error']['Message']: it is the whole `{"message": "..."}` document with every apostrophe written as \', instead of the bare sentence "1 validation error detected: Value '{<key>=<value>}' at 'addedTags' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]".
5. json.loads() on that message fails, because \' is not a valid JSON escape — so the nested document cannot even be unwrapped.
6. ListTagsForResource confirms the oversized key was not persisted and 'valid-key' survived (these already match AWS).
7. UntagResource(resourceArn=<pipe arn>, tagKeys=['k'*129]) and CreatePipe with an oversized tag key show the same nesting, since all three go through state.rs `tag_refusal`/`validation_document`.
