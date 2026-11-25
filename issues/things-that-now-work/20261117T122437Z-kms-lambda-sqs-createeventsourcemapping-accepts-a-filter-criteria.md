Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__lambda__sqs.esm-filter-criteria-kms-key-without-the-lambda-service-grant-is-rejected
Issue-Key: kms__lambda__sqs.esm-filter-criteria-kms-key-without-the-lambda-service-grant-is-rejected|_error#23|AssertionError|raise AssertionError(f"expected a rejection, call succeeded instead: {result!r}")
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__lambda__sqs: CreateEventSourceMapping accepts a filter-criteria KMSKeyArn whose key policy does not grant the Lambda service principal

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Cause 1 of 1: the filter-criteria encryption key is not authorization-checked at mapping creation. Distinct from the open issue 20261108T074808Z-kms-lambda-sqs-an-event-source-mapping-on.md, which concerns withholding invocation on an SSE-KMS queue.
  AWS: recorded behavior only — CreateEventSourceMapping with FilterCriteria and a KMSKeyArn whose key policy never names the regional Lambda service principal is refused 400 InvalidParameterValueException with the exact message 'Lambda service principal was denied access to decrypt your filter criteria. Please check your KMS key policy' (fixed text, no variable parts). No mapping is created. With the key policy granting the Lambda service principal kms:Decrypt/GenerateDataKey the identical call succeeds.
  Emulator: the call returns 202 and creates the mapping (UUID, State 'Creating', KMSKeyArn echoed), so an unusable key is accepted.
  Why: a client that depends on the create call to validate its key policy gets a mapping whose encrypted filter criteria can never be decrypted — the failure moves from a clear 400 at create time to silent non-delivery later.
  Fix: the CreateEventSourceMapping handler's KMSKeyArn validation — evaluate the key policy for the Lambda service principal (KMS policy evaluation) and refuse with InvalidParameterValueException before storing the mapping.
  Traces: kms__lambda__sqs.esm-filter-criteria-kms-key-without-the-lambda-service-grant-is-rejected.

## Evidence
esm-filter-criteria-kms-key-without-the-lambda-service-grant-is-rejected — AssertionError: expected a rejection, call succeeded instead: {'ResponseMetadata': {'HTTPStatusCode': 202, ...}, 'UUID': '<uuid>', 'EventSourceArn': 'arn:aws:sqs:us-west-2:<acct>:sim2real-xsvc-src-q-<id>', 'FilterCriteria': {'Filters': [{'Pattern': '{"body": {"kind": ["wanted"]}}'}]}, 'KMSKeyArn': 'arn:aws:kms:us-west-2:<acct>:key/<id>', 'State': 'Creating', ...}. Expected ('InvalidParameterValueException', 'Lambda service principal was denied access to decrypt your filter criteria. Please check your KMS key policy').

## Steps to Reproduce
1. Create a CMK whose key policy grants only the account root (no lambda.<region>.amazonaws.com service principal).
2. lambda.create_event_source_mapping(EventSourceArn=<sqs queue arn>, FunctionName=<fn>, FilterCriteria={'Filters': [{'Pattern': '{"body": {"kind": ["wanted"]}}'}]}, KMSKeyArn=<that key arn>) -> expect 400 InvalidParameterValueException with the message above and no mapping created.
3. Add the Lambda service principal grant to the key policy and repeat -> expect 202.
