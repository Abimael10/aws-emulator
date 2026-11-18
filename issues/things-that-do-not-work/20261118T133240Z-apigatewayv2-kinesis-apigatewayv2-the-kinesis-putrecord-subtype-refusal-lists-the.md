Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__kinesis.aws-proxy-subtype-kinesis-putrecord-required-parameters-refused-else-lands-mapped-data-and-key
Issue-Key: apigatewayv2__kinesis.aws-proxy-subtype-kinesis-putrecord-required-parameters-refused-else-lands-mapped-data-and-key|run#15|AssertionError|assert message == (
Issue-Commit: 1f604c3d80924acff5038a505405bc1871ebd9db

# apigatewayv2: the Kinesis-PutRecord subtype refusal lists the required parameters in alphabetical order instead of AWS's order

> _Note: this issue was identified against the code base at commit `1f604c3d80924acff5038a505405bc1871ebd9db`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
Cause 1 of 1: CreateIntegration refuses an AWS_PROXY integration whose IntegrationSubtype is Kinesis-PutRecord with no required RequestParameters mapped, but the bracketed parameter list in the refusal message is emitted alphabetically instead of in the order API Gateway words it for that subtype. Failing check: apigatewayv2__kinesis.aws-proxy-subtype-kinesis-putrecord-required-parameters-refused-else-lands-mapped-data-and-key.

AWS: Smithy (apigatewayv2 CreateIntegration) models IntegrationSubtype and RequestParameters and declares BadRequestException (errorKind client, httpError 400); the model does not state the refusal text, so the wording is recorded behavior only. The recording shows HTTP 400, Error.Code `BadRequestException`, message `Operation: Kinesis-PutRecord requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [StreamName, Data, PartitionKey]`. Fixed text: `Operation: ` … ` requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [` … `]`. Variable parts: the subtype the request named, and the bracketed list — that subtype's complete required set in AWS's own per-subtype order, comma-and-space separated. For Kinesis-PutRecord that order is StreamName, Data, PartitionKey. It is not alphabetical and differs per subtype.

Emulator: right status (400) and right code (BadRequestException), right sentence, wrong list order: `... parameter mapping: [Data, PartitionKey, StreamName]`. The subtype catalog stores the required set pre-sorted and the message renders it in storage order.

Why: a client or IaC layer that matches on, logs or displays the refusal gets a parameter list in an order AWS never produces; the operator is not shown the subtype's parameters in the order its integration-subtype reference lists them, and exact-match assertions on the message fail.

Fix: the first-class-subtype catalog that states each subtype's required parameters plus the single refusal-message renderer — store and render AWS's per-subtype order for Kinesis-PutRecord (StreamName, Data, PartitionKey) rather than an alphabetised one, for CreateIntegration, UpdateIntegration and import alike.

Note (Hypothesis, not observed by this check): the check stopped at this refusal, so the positive control (a fully mapped Kinesis-PutRecord route landing a record with the mapped data and partition key) was not graded; the data plane's subtype dispatcher mounts only eventbridge, sqs and stepfunctions action modules, so a mapped Kinesis-PutRecord route is likely answered as an integration that produced no response once the message is fixed.

Traces: apigatewayv2__kinesis.aws-proxy-subtype-kinesis-putrecord-required-parameters-refused-else-lands-mapped-data-and-key. Related open issue (describes the EARLIER failure mode of this same check — the subtype being accepted with nothing mapped — and so does not cover this wording divergence): 20261109T154749Z-apigatewayv2-kinesis-createintegration-accepts-a-kinesis-putrecord-aws.md.

## Evidence
Expected (recorded AWS): HTTP 400, `BadRequestException`, `Operation: Kinesis-PutRecord requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [StreamName, Data, PartitionKey]`.
Actual (emulator): HTTP 400, `BadRequestException`, `Operation: Kinesis-PutRecord requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [Data, PartitionKey, StreamName]`.
Replay: `AssertionError: unexpected message 'Operation: Kinesis-PutRecord requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [Data, PartitionKey, StreamName]'` (check line 96).
Source of the ordering: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/state/integration_subtype.rs:69-72 (`required_parameters: &["Data", "PartitionKey", "StreamName"]`) rendered by :152-158 `unmapped_required_parameters_message`.

## Steps to Reproduce
1. Create an ON_DEMAND Kinesis stream and wait for ACTIVE; create an IAM role API Gateway can assume.
2. Create an HTTP API.
3. Call CreateIntegration with IntegrationType=AWS_PROXY, IntegrationSubtype="Kinesis-PutRecord", PayloadFormatVersion="1.0", CredentialsArn=<role>, RequestParameters={}.
Observed: BadRequestException whose bracketed list is `[Data, PartitionKey, StreamName]`.
Expected: the same sentence with `[StreamName, Data, PartitionKey]`.
