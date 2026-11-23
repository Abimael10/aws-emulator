Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-runtime-invoke-guardrail-checks-error-paths
Issue-Step: task#3
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-runtime-invoke-guardrail-checks-error-paths
Issue-Shape-Id: com.amazonaws.bedrockruntime#AmazonBedrockFrontendService
Issue-Key: bedrock-runtime|InvokeGuardrailChecks|4xx->4xx|$.message
Issue-Commit: ddedefe2ba7626fc434a3b439556791987e3d078

# InvokeGuardrailChecks ValidationException lists the sensitive-information entity enum set in the wrong order

> _Note: this issue was identified against the code base at commit `ddedefe2ba7626fc434a3b439556791987e3d078`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InvokeGuardrailChecks

## Description
Cause 1 of 1: when InvokeGuardrailChecks is called with an unrecognized value for checks.sensitiveInformation.entities[i].type, the emulator rejects it with the right status and the right error shape, but renders the enumerated "Member must satisfy enum value set: [...]" list in a different order than AWS does, so the error message text does not match AWS byte-for-byte.

AWS: Smithy: InvokeGuardrailChecks (POST /guardrail-checks/invoke), input member checks -> GuardrailChecksConfig.sensitiveInformation -> GuardrailChecksSensitiveInformationConfig.entities (@length min 1 max 31) -> GuardrailChecksSensitiveInformationEntityConfig.type, required, targeting the enum GuardrailChecksSensitiveInformationEntityType (31 values); modeled error ValidationException, errorKind client, httpError 400, single member message -> NonBlankString. The model constrains the VALUE SET but says nothing about the order in which the service renders that set inside the message, so the ordering here is recorded behavior only. The recording shows AWS answering HTTP 400 ValidationException with the message template "1 validation error detected: Value '<rejected-value>' at '<member-path>' failed to satisfy constraint: Member must satisfy enum value set: [<value-set>]" — fixed text: "<N> validation error detected: Value '", "' at '", "' failed to satisfy constraint: Member must satisfy enum value set: [", "]"; variable parts taken from the request: the rejected value (the recording's 'NOT_A_REAL_ENTITY_TYPE' is one run's input, not part of the template) and the member path (the recording's 'checks.sensitiveInformation.entities.1.member.type' likewise reflects the index the client sent). The <value-set> part is a service-fixed listing of all 31 enum values; in the recording AWS emits it in its own non-alphabetical order, beginning US_INDIVIDUAL_TAX_IDENTIFICATION_NUMBER, US_BANK_ROUTING_NUMBER, MAC_ADDRESS, AWS_ACCESS_KEY, ... and ending ... CREDIT_DEBIT_CARD_NUMBER, CA_SOCIAL_INSURANCE_NUMBER, AGE.

Emulator: the emulator answers HTTP 400 with the same error shape and the same fixed text, and names the same rejected value and member path, but emits the value set in sorted/model-declaration order: ADDRESS, AGE, AWS_ACCESS_KEY, AWS_SECRET_KEY, ... US_SOCIAL_SECURITY_NUMBER, VEHICLE_IDENTIFICATION_NUMBER. The replay reports only $.message diverging (value_mismatch) with expected_status 400 and actual_status 400 — the same 31 members, a different order. Hypothesis: the emulator builds the list by iterating the enum as declared in the Smithy model (which is alphabetized) or by sorting it, rather than from the service's own fixed ordering.

Why: a client that asserts on the validation message — a test fixture, a golden-file comparison, a log scraper, or a user-facing surface that echoes the service message — sees a message that is not the message AWS returns for this error class, so an exact-match assertion against recorded AWS output fails even though the request was rejected for the right reason with the right status and error code. It also misreports to a human reader the order the service documents its accepted values in.

Fix: the InvokeGuardrailChecks input validation / error-message rendering for enum-constraint failures on GuardrailChecksSensitiveInformationEntityType — the value set must be emitted in the service's fixed order rather than sorted or model-declaration order, with the rejected value and member path still interpolated from the request.

Traces: bedrock-runtime-invoke-guardrail-checks-error-paths#task#3.

Note: the other bedrock-runtime divergence in this corpus (ApplyGuardrail omitting actionReason/output and the new usage counters) is already owned by open issue 20261111T062636Z-bedrock-runtime-applyguardrail-response-omits-actionreason-output-blockedresponse.md and is not re-filed here.

## Evidence
Fixture bedrock-runtime-invoke-guardrail-checks-error-paths, step task#3 (cluster member: bedrock-runtime-invoke-guardrail-checks-error-paths#task#3). verdict=mismatch, expected_status=400, actual_status=400, diverging key $.message.

Expected (recorded AWS): "1 validation error detected: Value 'NOT_A_REAL_ENTITY_TYPE' at 'checks.sensitiveInformation.entities.1.member.type' failed to satisfy constraint: Member must satisfy enum value set: [US_INDIVIDUAL_TAX_IDENTIFICATION_NUMBER, US_BANK_ROUTING_NUMBER, MAC_ADDRESS, AWS_ACCESS_KEY, VEHICLE_IDENTIFICATION_NUMBER, CREDIT_DEBIT_CARD_EXPIRY, NAME, CA_HEALTH_NUMBER, UK_UNIQUE_TAXPAYER_REFERENCE_NUMBER, INTERNATIONAL_BANK_ACCOUNT_NUMBER, CREDIT_DEBIT_CARD_CVV, PHONE, URL, UK_NATIONAL_INSURANCE_NUMBER, UK_NATIONAL_HEALTH_SERVICE_NUMBER, DRIVER_ID, AWS_SECRET_KEY, ADDRESS, US_PASSPORT_NUMBER, PIN, USERNAME, US_SOCIAL_SECURITY_NUMBER, LICENSE_PLATE, SWIFT_CODE, PASSWORD, IP_ADDRESS, EMAIL, US_BANK_ACCOUNT_NUMBER, CREDIT_DEBIT_CARD_NUMBER, CA_SOCIAL_INSURANCE_NUMBER, AGE]"

Actual (emulator): "1 validation error detected: Value 'NOT_A_REAL_ENTITY_TYPE' at 'checks.sensitiveInformation.entities.1.member.type' failed to satisfy constraint: Member must satisfy enum value set: [ADDRESS, AGE, AWS_ACCESS_KEY, AWS_SECRET_KEY, CA_HEALTH_NUMBER, CA_SOCIAL_INSURANCE_NUMBER, CREDIT_DEBIT_CARD_CVV, CREDIT_DEBIT_CARD_EXPIRY, CREDIT_DEBIT_CARD_NUMBER, DRIVER_ID, EMAIL, INTERNATIONAL_BANK_ACCOUNT_NUMBER, IP_ADDRESS, LICENSE_PLATE, MAC_ADDRESS, NAME, PASSWORD, PHONE, PIN, SWIFT_CODE, UK_NATIONAL_HEALTH_SERVICE_NUMBER, UK_NATIONAL_INSURANCE_NUMBER, UK_UNIQUE_TAXPAYER_REFERENCE_NUMBER, URL, USERNAME, US_BANK_ACCOUNT_NUMBER, US_BANK_ROUTING_NUMBER, US_INDIVIDUAL_TAX_IDENTIFICATION_NUMBER, US_PASSPORT_NUMBER, US_SOCIAL_SECURITY_NUMBER, VEHICLE_IDENTIFICATION_NUMBER]"

Both lists contain the same 31 members; only the order differs, and the emulator's order is exactly the order the Smithy enum GuardrailChecksSensitiveInformationEntityType declares its values in (verified via smithy_query closure on InvokeGuardrailChecks), which is alphabetized and is not the order AWS emits. Status (400) and the ValidationException shape match; no other response key diverges.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus), or hit it directly.
2. POST /guardrail-checks/invoke (bedrock-runtime, InvokeGuardrailChecks) with a valid messages list and checks.sensitiveInformation.entities containing one valid entity followed by one whose type is not a member of GuardrailChecksSensitiveInformationEntityType (e.g. entities[1].type = NOT_A_REAL_ENTITY_TYPE).
3. Observe HTTP 400 ValidationException and read $.message.
4. The bracketed enum value set is emitted in alphabetical (model-declaration) order; AWS emits the same 31 values in its own fixed, non-alphabetical order starting US_INDIVIDUAL_TAX_IDENTIFICATION_NUMBER, US_BANK_ROUTING_NUMBER, MAC_ADDRESS, AWS_ACCESS_KEY, ... and ending ... CA_SOCIAL_INSURANCE_NUMBER, AGE.
5. Re-replay bedrock-runtime-invoke-guardrail-checks-error-paths#task#3 to confirm; the step passes only when $.message matches byte-for-byte.
