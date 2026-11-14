Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connectcases.domain-name-conflict-rejected connectcases.field-value-type-mismatch-rejected
Issue-Shape-Id: com.amazonaws.connectcases#AmazonConnectCases
Issue-Key: connectcases.domain-name-conflict-rejected|run#7|AssertionError|assert error.get("Message") == expected_message, (
Issue-Key: connectcases.field-value-type-mismatch-rejected|run#10|AssertionError|assert error.get("Message") == expected_message, (
Issue-Commit: 1f5d5a2e3955ce51e23f36cd9ec02202b9df7d93

# connectcases refusal messages diverge: CreateDomain duplicate-name conflict and CreateCase field-value type mismatch use emulator wording

> _Note: this issue was identified against the code base at commit `1f5d5a2e3955ce51e23f36cd9ec02202b9df7d93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
Two net-new behavioral divergences in connectcases. In both the emulator reaches the RIGHT refusal (correct HTTP status and error class) but words the message in its own idiom instead of AWS's, so every message-pinned client path breaks. The two refusals are raised in two different handlers and need two different sentences. THIS ISSUE HAS TWO CAUSES - close both before submitting.

=== Cause 1 of 2: CreateDomain for a name that already exists (check connectcases.domain-name-conflict-rejected) ===
AWS: Smithy: CreateDomain = POST /domains, input name -> DomainName @length{min:1,max:100} @pattern ^.*[\S]$; modeled error ConflictException (errorKind client, httpError 409, single required member `message`, doc "The requested operation would cause a conflict with the current state of a service resource associated with the request..."). The message text is recorded behavior only: AWS answers 409 ConflictException with `Domain name already exists for account: <accountId>`. Fixed text: `Domain name already exists for account: `. ONE variable part: the caller's 12-digit AWS account id, resolved from the request's credentials/identity (the recording shows 000000000000 because that is this environment's account - the account id is the template slot, not a literal requirement). Note what AWS does NOT include: the domain name itself never appears in the sentence.
Emulator: HTTP 409 / ConflictException (both correct) with message `Domain creation failed: domain resource with the name <name> already exists!` - a different sentence that interpolates the requested domain name and omits the account id entirely.
Why: idempotent provisioning code matches this message to decide "this domain is already mine, continue", and account-scoped tooling parses the account id out of it to report which account owns the collision. Neither works, so re-running a domain bootstrap fails instead of converging, and the operator is shown the wrong fact (a name, not an account).
Fix: CreateDomain's duplicate-name branch - the ConflictException construction in that operation's handler. It must emit the fixed text with the resolved account id substituted, not the request's name.
Traces: behavioral check connectcases.domain-name-conflict-rejected (asserts Code == ConflictException, which passes, then the exact message, which fails). NOTE: open issue 20261109T181134Z-connectcases-conflict-and-field-reference-refusal-messages-use.md describes the same CreateDomain wording from the WIRE corpus as its Cause 1; it does not carry this behavioral check's replay key, so this check is filed here and must be re-replayed to confirm. Fixing that issue's Cause 1 should also close this one.

=== Cause 2 of 2: CreateCase with a field value whose union variant does not match the field's declared FieldType (check connectcases.field-value-type-mismatch-rejected) ===
AWS: Smithy: CreateCase = POST /domains/{domainId}/cases, required members domainId/templateId/fields (FieldValueList @length max 220); each FieldValue = {id: FieldId required, value: FieldValueUnion required}; FieldValueUnion variants are stringValue | doubleValue | booleanValue | emptyValue | userArnValue. The union itself does not encode which variant a given field accepts - the binding to the field's FieldType (Number here) is a semantic check against stored field state, so the message text is recorded behavior only: AWS answers 400 ValidationException with `Field '<fieldId>' value should be a doubleValue`. Fixed text: `Field '` + `' value should be a ` + the expected variant name; variable parts are (a) the offending field's id, taken from the request, and (b) the union variant that the field's stored FieldType requires (doubleValue for a Number field). AWS names the EXPECTED variant, not the supplied one, and does not name the field's type. The recorded id 36062768-b1a4-4746-b370-6ec400849457 is a per-run value filling the slot.
Emulator: HTTP 400 / ValidationException (both correct) with message `fields: 36062768-b1a4-4746-b370-6ec400849457 is a Number field and cannot hold stringValue` - prefixes the input member name, states the field's TYPE and the SUPPLIED variant, and never states the variant the caller should have sent.
Why: a client that reads this message to repair and retry the call (the normal use for a per-field validation failure) is never told which union variant to send, and contract tests or operator-facing copy pinned to AWS's sentence never match. The message also reads as a member-level complaint about `fields` rather than a per-field type error, so a client cannot distinguish it from a malformed-body refusal.
Fix: CreateCase's field-value type check in its handler/validation - when a FieldValueUnion variant does not match the resolved field definition's FieldType, raise ValidationException with AWS's sentence, interpolating the field id from the request and the EXPECTED variant derived from the stored field type. Related but distinct: open issue 20261109T181134Z-connectcases-validationexception-bodies-use-an-enumerated-constraint-template.md covers CreateCase's MISSING-required-field message (`Field(s) <names> are required`) and the generic enumerated-constraint template; it does not cover the variant/FieldType mismatch described here.
Traces: behavioral check connectcases.field-value-type-mismatch-rejected (Code == ValidationException passes; the exact message fails). The same check then asserts no case was created and that the matching doubleValue persists - those assertions are not reached.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 14/25 passed, 3 mismatched, 8 errored. Both failures below are listed under '### connectcases' and are marked neither 'ALREADY FILED' nor 'OWNED by'.

Cause 1 - [mismatch] connectcases.domain-name-conflict-rejected:
  check_connectcases_domain_name_conflict_rejected.py line 27:
  AssertionError: expected message 'Domain name already exists for account: 000000000000', got 'Domain creation failed: domain resource with the name sim2real-cc-domain-1971a8777ae8 already exists!'
  The preceding assertion error.get("Code") == "ConflictException" passed, so status 409 and the error class are already right; only $.message diverges. In the expected sentence the fixed text is 'Domain name already exists for account: ' and '000000000000' is this environment's account id (a request/identity value filling the slot). In the actual sentence 'sim2real-cc-domain-1971a8777ae8' is the per-run domain name the emulator wrongly interpolates.

Cause 2 - [mismatch] connectcases.field-value-type-mismatch-rejected:
  check_connectcases_field_value_type_mismatch_rejected.py line 39:
  AssertionError: expected message "Field '36062768-b1a4-4746-b370-6ec400849457' value should be a doubleValue", got 'fields: 36062768-b1a4-4746-b370-6ec400849457 is a Number field and cannot hold stringValue'
  The preceding assertion error.get("Code") == "ValidationException" passed, so status 400 and the error class are right; only $.message diverges. The field id is a per-run value present in BOTH strings and is the request-supplied variable part; the fixed frame is "Field '" ... "' value should be a " ... and the second variable part is the expected union variant ('doubleValue', derived from the field's stored Number type).

Smithy (services/service-com-amazonaws-connectcases-amazonconnectcases/smithy-model/com-amazonaws-connectcases-amazonconnectcases.json): ConflictException errorKind client, httpError 409, required member message; ValidationException errorKind client, httpError 400, required member message, doc "The request isn't valid. Check the syntax and try again."; ConflictException + ValidationException both bound on CreateDomain, ValidationException bound on CreateCase; FieldValueUnion = stringValue|doubleValue|booleanValue|emptyValue|userArnValue; FieldValue = {id required, value required}.

## Steps to Reproduce
Replay the two behavioral checks named in trace_ids against the connectcases emulator. Manually:

Cause 1 (CreateDomain conflict wording):
1. cc.create_domain(name=N) with a unique N; poll cc.get_domain(domainId=...) until domainStatus == 'Active'.
2. cc.get_domain(domainId=...)['name'] -> N.
3. cc.create_domain(name=N) again.
   Expect: ClientError with Code 'ConflictException' (HTTP 409) and Message exactly 'Domain name already exists for account: <your 12-digit account id>'.
   Actual: Code 'ConflictException' (correct) but Message 'Domain creation failed: domain resource with the name N already exists!'.
4. cc.list_domains (all pages) must still show exactly one domain named N - that part passes.

Cause 2 (CreateCase field-value variant mismatch wording):
1. cc.create_domain(name=<unique>) -> poll Active.
2. cc.create_field(domainId, name=<unique>, type='Number') -> numberFieldId.
3. cc.create_template(domainId, name=<unique>, status='Active') -> templateId; resolve the System 'title' fieldId via cc.list_fields.
4. cc.create_case(domainId, templateId, fields=[{'id': titleFieldId, 'value': {'stringValue': <unique>}}, {'id': numberFieldId, 'value': {'stringValue': '42'}}]).
   Expect: ClientError with Code 'ValidationException' (HTTP 400) and Message exactly "Field '<numberFieldId>' value should be a doubleValue".
   Actual: Code 'ValidationException' (correct) but Message 'fields: <numberFieldId> is a Number field and cannot hold stringValue'.
5. Re-issue the same CreateCase with {'doubleValue': 42.0} for numberFieldId - it must succeed and GetCase must return {'doubleValue': 42.0}.
