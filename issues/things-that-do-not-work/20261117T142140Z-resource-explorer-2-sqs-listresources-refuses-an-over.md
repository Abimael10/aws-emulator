Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: resource-explorer-2__sqs.list-resources-max-results-over-limit-rejected
Issue-Key: resource-explorer-2__sqs.list-resources-max-results-over-limit-rejected|run#16|AssertionError|assert got == _OVER_LIMIT, (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# resource-explorer-2__sqs: ListResources refuses an over-limit MaxResults with the Smithy constraint sentence instead of AWS's "Invalid request body"

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListResources

## Description
One net-new root cause.

Cause 1 of 1: the over-limit MaxResults refusal carries a generated constraint-violation sentence rather than the message AWS returns for this class.
AWS: Smithy resourceexplorer2#ListResources declares MaxResults with range {min: 1, max: 1000} and models ValidationException (httpError 400). The recorded refusal for MaxResults above that maximum is HTTP 400, Code 'ValidationException', Message 'Invalid request body' — the whole message is fixed text for this class; it names neither the member nor the submitted value.
Emulator: the status (400) and the code ('ValidationException') are right, but the message is '1 validation error detected: Value \'<value>\' at \'maxResults\' failed to satisfy constraint: Member must have value less than or equal to 1000' — the generic AWS-SDK constraint sentence, which Resource Explorer does not serve for this operation.
Why: 'ValidationException' is shared by every Resource Explorer input rejection, so the message is the only discriminator; a client (or a recorded-response fixture) asserting on AWS's text sees a mismatch, and the emulator leaks the request's value into a message AWS keeps value-free.
Fix: the ListResources handler's input validation — raise ValidationException with AWS's fixed 'Invalid request body' text for an out-of-range MaxResults, rather than letting the shared Smithy range-constraint message formatter author it.
Traces: resource-explorer-2__sqs.list-resources-max-results-over-limit-rejected. Related but NOT covering this check: open issues 20261110T193424Z-resource-explorer-2-listresources-response-bodies-diverge-from.md (success-path wire shape) and 20261110T225751Z-resource-explorer-2-listresources-refuses-with-401-unauthorizedexception-when-the-region.md (a 401 where a list is expected) — neither carries this check's replay key nor concerns the MaxResults refusal text.

## Evidence
Check resource-explorer-2__sqs.list-resources-max-results-over-limit-rejected (verdict: mismatch).

Expected (AWS): ListResources(MaxResults=1001) -> HTTP 400, ('ValidationException', 'Invalid request body').

Actual (emulator): `AssertionError: expected MaxResults=1001 to be rejected with ('ValidationException', 'Invalid request body'), got ('ValidationException', "1 validation error detected: Value '1001' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 1000")` (check file line 59). The code and status are correct; only the message diverges. 1001 is the value the check submits — the requirement is that ANY value above 1000 (and below 1) gets the fixed 'Invalid request body' text, not that 1001 appears in it.

## Steps to Reproduce
Against the emulator's resource-explorer-2 endpoint (any region/account, with an index and default view in place):
1. ListResources(MaxResults=1001) -> observe HTTP 400 with Code 'ValidationException' and Message "1 validation error detected: Value '1001' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 1000". AWS answers Message 'Invalid request body'.
2. ListResources(MaxResults=1000) must succeed (the boundary is inclusive).
