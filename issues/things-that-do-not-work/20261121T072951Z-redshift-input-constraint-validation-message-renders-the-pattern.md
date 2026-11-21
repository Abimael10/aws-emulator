Issue-Source: trace-sweep-divergence
Issue-Trace: redshift-integration-and-idc-application-preconditions
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: redshift-integration-and-idc-application-preconditions
Issue-Shape-Id: com.amazonaws.redshift#RedshiftServiceVersion20121201
Issue-Key: redshift|CreateIntegration|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 28e13556b346bec2a9903044f83d66021b33e840

# Redshift input-constraint validation message renders the @pattern without its ^/$ anchors

> _Note: this issue was identified against the code base at commit `28e13556b346bec2a9903044f83d66021b33e840`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
One net-new root cause in the redshift (AWS query protocol) control plane. A fixer must close ONE cause before submitting.

=== Cause 1 of 1: the 'failed to satisfy constraint: Member must satisfy regular expression pattern:' sentence is built from the modeled regex with the ^ and $ anchors stripped ===

AWS: CreateIntegration whose SourceArn does not match the member's modeled regular expression is refused with HTTP 400, Error.Type 'Sender', Error.Code 'InvalidParameterValue' and the standard AWS constraint-violation diagnostic:
"<N> validation error detected: Value '<rejected value>' at '<member name>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>".
The fixed text is '<N> validation error detected: Value ', "' at '", "' failed to satisfy constraint: Member must satisfy regular expression pattern: "; the variable parts are the error count, the rejected value taken verbatim from the request, the wire member name ('sourceArn' for CreateIntegration$SourceArn) and the pattern — and the pattern AWS quotes is the member's @pattern trait EXACTLY as modeled, anchors included. Smithy (services/service-com-amazonaws-redshift-redshiftserviceversion20121201): CreateIntegration input member SourceArn (shape SourceArn) carries @pattern "^arn:aws[a-z\\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\\-\\/]+$" and @length min 1/max 255; the operation's refusal class for a constraint violation is the query-protocol InvalidParameterValue (Sender) at HTTP 400. The recorded response confirms status 400 and that the quoted pattern is the anchored form.

Emulator: answers the right status (400), the right error code and the right sentence structure, but emits the pattern with the leading '^' and trailing '$' removed: 'Member must satisfy regular expression pattern: arn:aws[a-z\\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\\-\\/]+' where AWS emits '... pattern: ^arn:aws[a-z\\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\\-\\/]+$'. Everything else in the message (count, rejected value, member name) matches, so the defect is in how the pattern is rendered into the diagnostic, not in the detection of the violation.

Why: the error message is a requirement of this error class. The quoted pattern is the only remediation information the caller gets, and an unanchored regex has a different meaning from an anchored one — it reads as a substring rule, so a client (or an operator/SDK test that re-applies the quoted pattern to repair its input) concludes that any ARN merely CONTAINING an s3/dynamodb ARN would be accepted. Transcript/golden-output tests and error-mapping layers that compare the documented sentence diverge on every constraint refusal built this way, and because the renderer is shared, the same anchor loss affects the other @pattern-constrained members of this service (TargetArn, IntegrationName, cluster/parameter-group identifiers), not just SourceArn.

Fix: the shared constraint-violation diagnostic builder used by this service's request validation — it must interpolate the member's @pattern trait verbatim as taken from the Smithy model instead of trimming the anchors before formatting the 'Member must satisfy regular expression pattern:' clause. No change is needed to the match decision itself or to the status/code mapping.

Traces: redshift-integration-and-idc-application-preconditions#task#1 (CreateIntegration). Related open issue 20261109T041550Z-redshift-failoverprimarycompute-clusternotfound-wording-enablelogging-cloudwatch-creates-no.md covers a DIFFERENT CreateIntegration defect (the classification of an absent target namespace) and carries behavioral check keys, not this fixture step; it does not cover this message rendering, and the replay did not mark this cluster owned, so it is filed here and that issue is named for context.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator (316/353 steps passed, 25 divergence clusters). Cluster: service=redshift|CreateIntegration|status=400->400|keys=$.ErrorResponse.Error.Message, fixture redshift-integration-and-idc-application-preconditions, step task#1, verdict mismatch, expected_status 400, actual_status 400, affected_count 1.

$.ErrorResponse.Error.Message: value_mismatch
  expected: "1 validation error detected: Value '<source-arn from the request>' at 'sourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: ^arn:aws[a-z\\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\\-\\/]+$"
  actual:   "1 validation error detected: Value '<source-arn from the request>' at 'sourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:aws[a-z\\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\\-\\/]+"

The recorded run sent an RDS-cluster ARN as SourceArn; that ARN value is a per-run input that fills the template and is not itself the requirement. The requirement is the fixed sentence text plus the anchored pattern string, which is byte-for-byte the SourceArn @pattern in the service's Smithy model (query=operation on CreateIntegration: SourceArn pattern "^arn:aws[a-z\\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\\-\\/]+$"). Status (400), Error.Type (Sender) and Error.Code (InvalidParameterValue) matched; only the pattern rendering diverged.

## Steps to Reproduce
1. Call redshift CreateIntegration with a well-formed but non-s3/non-dynamodb SourceArn (e.g. an RDS cluster ARN), any valid TargetArn and IntegrationName.
2. Read the ErrorResponse: expect HTTP 400, Error.Type 'Sender', Error.Code 'InvalidParameterValue'.
3. Compare Error.Message against the AWS diagnostic and confirm the quoted regular expression is the model's @pattern INCLUDING the leading '^' and trailing '$'. The emulator omits both anchors.
4. Repeat with a TargetArn / IntegrationName that violates their @pattern to confirm the same renderer drops the anchors there too.

Replay: fixture redshift-integration-and-idc-application-preconditions, step task#1.
