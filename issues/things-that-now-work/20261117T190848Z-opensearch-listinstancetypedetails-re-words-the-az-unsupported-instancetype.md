Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.instance-type-catalog-format-validation-rejected
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.instance-type-catalog-format-validation-rejected|run#17|AssertionError|assert message == expected, f"ListInstanceTypeDetails: got message {message!r}"
Issue-Commit: 5f72b15ceea68fb83a63d5bc988a846400d3d8ef

# opensearch ListInstanceTypeDetails re-words the AZ-unsupported InstanceType refusal: it answers an emulator-authored 'is not a known instance type' message instead of AWS's "The AZ doesn't support instance type :<type>"

> _Note: this issue was identified against the code base at commit `5f72b15ceea68fb83a63d5bc988a846400d3d8ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListInstanceTypeDetails

## Description
Cause 1 of 1: the InstanceType refusal message of ListInstanceTypeDetails is emulator-authored.
AWS: recorded behavior only for the message text; the model (smithy_query operation=ListInstanceTypeDetails on services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json) states that InstanceType is an optional httpQuery member of type InstanceTypeString with @length 10..40 and @pattern ^.*\..*\.search$, that RetrieveAZs is an optional httpQuery boolean, and that ValidationException (HTTP 400) is a modeled error. The recording shows that a well-formed `<family>.<size>.search` value that no Availability Zone offers is graded only once RetrieveAZs asks the service to resolve it against a zone, and is then refused with 400 ValidationException whose message is `The AZ doesn't support instance type :<InstanceType as sent>` — fixed text `The AZ doesn't support instance type :` (one space before the colon-prefixed value is absent; the colon is immediately followed by the value), with the instance type taken verbatim from the request.
Emulator: the refusal code is right (ValidationException) but the message is its own: `InstanceType is not a known instance type: <InstanceType>`. The handler at services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/list_instance_type_details.rs grades the InstanceType against crate::state::publishes_instance_type and raises crate::state::unknown_instance_type("InstanceType", instance_type) unconditionally — i.e. without regard to RetrieveAZs and with its own wording.
Why: a client that matches or surfaces the AWS message (log scraping, user-facing error text, or a parity assertion on the refusal) sees text AWS never emits; and because the grading is unconditional, a request WITHOUT RetrieveAZs that AWS answers 200 for (the parameter being unused there) is refused instead.
Fix: the ListInstanceTypeDetails handler's InstanceType validation — the message construction in crate::state (unknown_instance_type) and the condition that gates it on RetrieveAZs.
Traces: opensearch.instance-type-catalog-format-validation-rejected. The same check's DescribeInstanceTypeLimits EngineVersion pattern refusal passes, so this is narrowly the InstanceType/AZ path. The open issue 20261114T093806Z-opensearch-instance-type-engine-version-and-reserved-instance.md is about the CONTENT of the instance-type catalog rows, not this refusal's wording, so it does not cover this.

## Evidence
Expected (AWS, recorded): ListInstanceTypeDetails(EngineVersion='OpenSearch_2.11', InstanceType='<family>.<size>.search' not offered in any AZ, RetrieveAZs=True) -> HTTP 400 ValidationException, Message == "The AZ doesn't support instance type :<InstanceType>".

Actual (emulator replay): code ValidationException but Message == 'InstanceType is not a known instance type: not.a.real.search'.
Failing assertion: AssertionError: ListInstanceTypeDetails: got message 'InstanceType is not a known instance type: not.a.real.search' (check_opensearch_instance_type_catalog_format_validation_rejected.py line 68).

Source: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/list_instance_type_details.rs lines 206-213 refuse with crate::state::unknown_instance_type("InstanceType", instance_type) whenever crate::state::publishes_instance_type is false, independently of input.retrieve_a_zs (read at lines 236-239).

## Steps to Reproduce
1. Boot the emulator and point an opensearch client at it.
2. Positive control: ListInstanceTypeDetails(EngineVersion='OpenSearch_2.11') must list t3.small.search, and DescribeInstanceTypeLimits(InstanceType='t3.small.search', EngineVersion='OpenSearch_2.11') must return LimitsByRole.
3. Call ListInstanceTypeDetails(EngineVersion='OpenSearch_2.11', InstanceType='not.a.real.search', RetrieveAZs=True).
4. Expect 400 ValidationException with Message "The AZ doesn't support instance type :not.a.real.search"; observe Message 'InstanceType is not a known instance type: not.a.real.search'.
5. Also call the same request WITHOUT RetrieveAZs: AWS does not grade the unused parameter there, the emulator still refuses.
6. Re-replay: opensearch.instance-type-catalog-format-validation-rejected.
