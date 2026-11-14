Issue-Source: trace-sweep-divergence
Issue-Trace: neptune-npt-traj-create-cluster-request-shape
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: neptune-npt-traj-create-cluster-request-shape
Issue-Shape-Id: com.amazonaws.neptune#AmazonRDSv19
Issue-Key: neptune|CreateDBCluster|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 36310639c648ae76daf622f5e9049d2412d0bf50

# Neptune awsQuery boolean coercion refusal carries emulator-authored message text instead of AWS's xsd1.1 sentence

> _Note: this issue was identified against the code base at commit `36310639c648ae76daf622f5e9049d2412d0bf50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBCluster

## Description
THIS ISSUE HAS 1 CAUSE.

Cause 1 of 1: a non-boolean value supplied for a modelled boolean member of a Neptune awsQuery request is refused with the emulator's own parser diagnostic rather than the service's fixed sentence.

AWS: CreateDBClusterMessage carries several BooleanOptional members — StorageEncrypted, DeletionProtection, CopyTagsToSnapshot, EnableIAMDatabaseAuthentication (Smithy: CreateDBCluster, input StorageEncrypted/DeletionProtection/CopyTagsToSnapshot/EnableIAMDatabaseAuthentication target BooleanOptional; CreateDBCluster models no generic input-validation fault, so the refusal comes from the awsQuery/XML value layer and is reported as the service-common InvalidParameterValue). On the awsQuery wire a boolean form value must be one of the xsd:boolean lexical forms; a value outside that set is rejected by the protocol layer, which answers HTTP 400 with Error Type 'Sender', Code 'InvalidParameterValue' and the FIXED message 'boolean must follow xsd1.1 definition' (recorded behavior: the recording shows exactly that status, code and message). The message is entirely fixed text — it names neither the member nor the offending value, so there is no variable part to interpolate.

Emulator: the same request is refused 400 (status matches) but with Error.Message 'expected `true`/`false`, got `yes`' — a parser-authored diagnostic whose fixed text differs from AWS's and which interpolates the request's supplied value (here the literal taken from the request, not part of AWS's template) and the emulator's own accepted-literal list.

Why: the error message is part of the contract for this refusal class. A client, test harness or operator that matches or surfaces the service's refusal text (log assertions, user-facing error reporting, SDK error-message pattern matching) sees a sentence AWS never emits, and the emulator's wording additionally advertises an accepted-value set ('true'/'false' only) narrower than xsd1.1 boolean, so a caller reading it concludes that forms AWS does accept (e.g. '1'/'0') are invalid.

Fix: the awsQuery/XML boolean deserializer shared by the Neptune front door's request parsing (the layer that converts form values to modelled boolean members) — its rejection must raise InvalidParameterValue carrying the fixed sentence 'boolean must follow xsd1.1 definition', not a value-interpolated parser message. CreateDBCluster's own validation is not the right place: the refusal is protocol-level and applies to every boolean member of every Neptune awsQuery operation.

Related open issue: 20261103T175309Z-neptune-cluster-lookup-and-request-shape-refusals-carry.md covers step task#1 of this same fixture (Error.Code MalformedInput vs InvalidParameterValue) and, as its cause 5, ModifyDBClusterParameterGroup's 'Invalid boolean value: <value>' parameter-value message. Neither covers this cluster — replay reports it with owned_by=[] and stub_path="" — and the sentence required here is the protocol-level xsd1.1 one, a different template from the ModifyDBClusterParameterGroup parameter-value message.

Traces: neptune-npt-traj-create-cluster-request-shape#task#2.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, bootstrap_signal=false, steps_total=181, steps_passed=149, steps_mismatched=32, score 82.3%, 19 divergence clusters, all service=neptune. This is the only cluster reported with owned_by=[] and stub_path=""; the other 18 are owned by existing open issues.

Cause 1 — neptune-npt-traj-create-cluster-request-shape#task#2, CreateDBCluster, verdict mismatch, expected_status 400 / actual_status 400 (status agrees). divergence_key service=neptune|CreateDBCluster|status=400->400|keys=$.ErrorResponse.Error.Message. Detail: $.ErrorResponse.Error.Message: value_mismatch (expected 'boolean must follow xsd1.1 definition', got 'expected `true`/`false`, got `yes`'). Error.Code and the rest of the ErrorResponse envelope match. In AWS's message every word is fixed text; in the emulator's message '`yes`' is the value echoed from the request and '`true`/`false`' is the emulator's own accepted-literal list.

## Steps to Reproduce
1. Clone the repo, boot the emulator built from it, and replay the simparity-wire-aws-v1 wire corpus.
2. Replay fixture neptune-npt-traj-create-cluster-request-shape, step task#2: a CreateDBCluster awsQuery POST that supplies a non-xsd-boolean form value (the recording uses 'yes') for a BooleanOptional member of CreateDBClusterMessage.
3. Compare $.ErrorResponse.Error.Message against AWS's fixed sentence 'boolean must follow xsd1.1 definition'. The emulator answers 400/InvalidParameterValue with 'expected `true`/`false`, got `yes`'.
4. Repeat with another boolean member of the same operation (and any other Neptune awsQuery operation with a boolean member) to confirm the fix lands in the shared boolean deserializer rather than one handler.
