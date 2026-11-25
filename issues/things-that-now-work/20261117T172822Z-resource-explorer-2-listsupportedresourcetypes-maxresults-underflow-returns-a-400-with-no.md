Issue-Source: trace-sweep-divergence
Issue-Trace: resource-explorer-2-list-supported-resource-types-maxresults-bounds
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resource-explorer-2-list-supported-resource-types-maxresults-bounds
Issue-Shape-Id: com.amazonaws.resourceexplorer2#ResourceExplorer
Issue-Key: resource-explorer-2|ListSupportedResourceTypes|4xx->4xx|$.FieldList
Issue-Key: resource-explorer-2|ListSupportedResourceTypes|4xx->4xx|$.Message
Issue-Commit: 21d28fce2924b432da3229c23b4c690c62601190

# ListSupportedResourceTypes MaxResults underflow returns a 400 with no ValidationException Message or FieldList

> _Note: this issue was identified against the code base at commit `21d28fce2924b432da3229c23b4c690c62601190`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListSupportedResourceTypes

## Description
One net-new root cause, affecting both steps of the MaxResults-bounds fixture (task#1 and task#2, the below-minimum MaxResults values).

Cause 1 of 1: the below-minimum MaxResults refusal is returned as an empty-bodied 400 instead of a modeled ValidationException payload.
AWS: Smithy com.amazonaws.resourceexplorer2#ListSupportedResourceTypes declares MaxResults as Integer with @range {min: 1, max: 1000} and models ValidationException (errorKind client, httpError 400). ValidationException has member Message (@required, String) and member FieldList (ValidationExceptionFieldList); each ValidationExceptionField carries Name (@required) and ValidationIssue (@required). The recording shows the refusal of a MaxResults below the modeled minimum as HTTP 400 with Message 'MaxResults must be greater than 0.' and FieldList = [{Name: <the offending input member's name, here 'MaxResults'>, ValidationIssue: '< 1'}]. Fixed text for this class: the whole sentence 'MaxResults must be greater than 0.' and the ValidationIssue token '< 1'; the only value taken from the request is the member name reported in FieldList[].Name. The submitted MaxResults value itself does not appear in AWS's message.
Emulator: the HTTP status is right (400), but the response body carries neither member - replay reports '$.Message: missing (expected \'MaxResults must be greater than 0.\', got None)' and '$.FieldList: missing (expected [{\'Name\': \'MaxResults\', \'ValidationIssue\': \'< 1\'}], got None)'. So the 400 arrives with no error message and no field-level detail at all.
Why: ValidationException is shared by every Resource Explorer input rejection, so Message is the only discriminator between 'bad MaxResults', 'bad Filters' and 'bad NextToken'. A client gets an exception whose required Message is absent (SDK error.message is empty / None) and whose FieldList is absent, so it can neither log nor branch on which parameter it got wrong, and any retry/repair logic keyed on the field name has nothing to read.
Fix: the ListSupportedResourceTypes handler's input validation plus the ValidationException serializer for this service - reject MaxResults < 1 by raising a ValidationException populated with the fixed 'MaxResults must be greater than 0.' text and a FieldList entry {Name: 'MaxResults', ValidationIssue: '< 1'}, and make sure the error serializer actually emits the Message and FieldList members in the 400 body rather than an empty object.
Traces: resource-explorer-2-list-supported-resource-types-maxresults-bounds#task#1 and #task#2. Related but NOT covering this cause: open issue 20261117T142140Z-resource-explorer-2-sqs-listresources-refuses-an-over.md concerns the MESSAGE TEXT of an OVER-limit MaxResults refusal on a different operation (ListResources) and carries that behavioral check's replay key, not this fixture step; here the operation is ListSupportedResourceTypes, the bound is the minimum, and the defect is that Message and FieldList are missing entirely. Open issues 20261108T081111Z-resource-explorer-2-listresources-stub.md, 20261110T193424Z-resource-explorer-2-listresources-response-bodies-diverge-from.md and 20261110T225751Z-resource-explorer-2-listresources-refuses-with-401-unauthorizedexception-when-the-region.md are all about ListResources and do not cover this operation's validation body.

## Evidence
Fixture resource-explorer-2-list-supported-resource-types-maxresults-bounds, steps task#1 and task#2 (verdict: mismatch; cluster key service=resource-explorer-2|ListSupportedResourceTypes|status=400->400|keys=$.FieldList,$.Message).

Expected (AWS, from the recording + Smithy model): HTTP 400, ValidationException body with
  $.Message  = 'MaxResults must be greater than 0.'
  $.FieldList = [{'Name': 'MaxResults', 'ValidationIssue': '< 1'}]

Actual (emulator): HTTP 400, but
  $.Message: missing (got None)
  $.FieldList: missing (got None)

The status matches; only the body diverges. The requirement is that ANY MaxResults below the modeled minimum of 1 produces that fixed message and a FieldList entry naming the offending member - not that one particular submitted value appears anywhere in the body.

## Steps to Reproduce
Against the emulator's resource-explorer-2 endpoint (any region/account):
1. POST /ListSupportedResourceTypes with a body whose MaxResults is below the modeled minimum (e.g. MaxResults=0, then MaxResults=-1, the two values the fixture's task#1 and task#2 submit).
2. Observe HTTP 400 whose JSON body contains neither 'Message' nor 'FieldList'. AWS answers HTTP 400 with Message 'MaxResults must be greater than 0.' and FieldList [{"Name": "MaxResults", "ValidationIssue": "< 1"}].
3. ListSupportedResourceTypes(MaxResults=1) must still succeed with HTTP 200 (the minimum is inclusive).
