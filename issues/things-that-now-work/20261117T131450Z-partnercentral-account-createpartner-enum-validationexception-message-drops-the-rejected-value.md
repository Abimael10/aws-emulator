Issue-Source: trace-sweep-divergence
Issue-Trace: partnercentral-account-partner-onboarding-attempt
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: partnercentral-account-partner-onboarding-attempt
Issue-Shape-Id: com.amazonaws.partnercentralaccount#PartnerCentralAccount
Issue-Key: partnercentral-account|CreatePartner|4xx->4xx|$.Message
Issue-Commit: 205fbc57e4a61a31c8ee6f7a22b3e8f9329e8aec

# CreatePartner enum ValidationException message drops the rejected value clause AWS emits

> _Note: this issue was identified against the code base at commit `205fbc57e4a61a31c8ee6f7a22b3e8f9329e8aec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePartner

## Description
Cause 1 of 1: for an out-of-enum `PrimarySolutionType` on CreatePartner, the emulator's multi-error ValidationException message renders the enum clause WITHOUT the offending value, so the clause reads "Value at 'primarySolutionType' failed to satisfy constraint: ..." where AWS emits "Value '<supplied value>' at 'primarySolutionType' failed to satisfy constraint: ...".

AWS: Smithy (com.amazonaws.partnercentralaccount#PartnerCentralAccount): CreatePartner.PrimarySolutionType is @required and targets the enum shape `PrimarySolutionType` (values SOFTWARE_PRODUCTS, CONSULTING_SERVICES, PROFESSIONAL_SERVICES, MANAGED_SERVICES, HARDWARE_PRODUCTS, COMMUNICATION_SERVICES, VALUE_ADDED_RESALE_AWS_SERVICES, TRAINING_SERVICES); CreatePartner.ClientToken is @length(min 1, max 64) @pattern(^[A-Za-z0-9-_]+$); CreatePartner declares ValidationException (errorKind client, httpError 400). The recorded response is HTTP 400 with a message that aggregates both violations of the one request:
  "2 validation errors detected: Value '<clientToken value>' at 'clientToken' failed to satisfy constraint: Member must have length less than or equal to 64; Value '<primarySolutionType value>' at 'primarySolutionType' failed to satisfy constraint: Member must satisfy enum value set: [TRAINING_SERVICES, COMMUNICATION_SERVICES, CONSULTING_SERVICES, PROFESSIONAL_SERVICES, VALUE_ADDED_RESALE_AWS_SERVICES, HARDWARE_PRODUCTS, SOFTWARE_PRODUCTS, MANAGED_SERVICES]".
Fixed text of that template: "<n> validation errors detected: ", then per violation "Value '", "' at '", "' failed to satisfy constraint: ", and the per-constraint clause "Member must have length less than or equal to <max>" / "Member must satisfy enum value set: [<values>]", violations joined by "; ". Variable parts taken from the request and the model: the violation count, each member path (the lowerCamel member name, here `clientToken` and `primarySolutionType`), the rejected VALUE supplied for that member, the length bound from the member's @length, and the enum value set in the order AWS lists it for this member. The quoted token in the recording is that run's own input filling the template, not part of it.

Emulator: returns HTTP 400 / ValidationException with the same count, the same member paths, the same two constraint clauses and the same enum value set, but the enum violation is rendered value-free: "... Value at 'primarySolutionType' failed to satisfy constraint: Member must satisfy enum value set: [...]". The `clientToken` length violation in the SAME message does carry its "Value '<v>' " clause, so the formatter is inconsistent between its length path and its enum path.

Why: a client, SDK test, log scraper or support tool that matches AWS's exact validation text for an enum member never matches the emulator's, so enum-rejection handling that is correct against AWS reports an unrecognized validation failure against the emulator; and a caller that parses the message to learn WHICH value it sent was rejected (the only place that information appears, since ValidationException.ErrorDetails is not populated here) gets nothing back for the enum member.

Note on an adjacent open issue: 20261116T185926Z-partnercentral-account-listconnectioninvitations-enum-validationexception-message-includes-the-rejected-value.md asserts the opposite for ListConnectionInvitations.Status (AWS value-free there, emulator interpolating). That issue does not cover this one — different operation, different member, opposite direction — so whichever formatter serves both paths must be made per-member/per-constraint faithful rather than flipped globally: CreatePartner's enum clause MUST carry the value.

Fix: the ValidationException message formatter on CreatePartner's input validation in the partnercentral-account crate — specifically the enum-constraint branch that builds a violation clause — must emit the "Value '<supplied value>' at '<memberPath>' failed to satisfy constraint: Member must satisfy enum value set: [...]" form, matching the form its own length-constraint branch already emits in the same aggregated message.

Traces: partnercentral-account-partner-onboarding-attempt#task#3 (CreatePartner with an over-length clientToken and an out-of-enum primarySolutionType).

## Evidence
Fixture: partnercentral-account-partner-onboarding-attempt, step task#3 (service partnercentral-account, operation CreatePartner, verdict mismatch).

Expected status 400, actual status 400 — the status and the error code agree; the divergence is confined to $.Message.

Expected (recorded AWS):
  "2 validation errors detected: Value 'sim2real-probe-partnercentral-account-create-partner-bad-076ba00b' at 'clientToken' failed to satisfy constraint: Member must have length less than or equal to 64; Value 'NOT_A_SOLUTION_TYPE' at 'primarySolutionType' failed to satisfy constraint: Member must satisfy enum value set: [TRAINING_SERVICES, COMMUNICATION_SERVICES, CONSULTING_SERVICES, PROFESSIONAL_SERVICES, VALUE_ADDED_RESALE_AWS_SERVICES, HARDWARE_PRODUCTS, SOFTWARE_PRODUCTS, MANAGED_SERVICES]"

Actual (emulator):
  "2 validation errors detected: Value 'sim2real-probe-partnercentral-account-create-partner-bad-076ba00b' at 'clientToken' failed to satisfy constraint: Member must have length less than or equal to 64; Value at 'primarySolutionType' failed to satisfy constraint: Member must satisfy enum value set: [TRAINING_SERVICES, COMMUNICATION_SERVICES, CONSULTING_SERVICES, PROFESSIONAL_SERVICES, VALUE_ADDED_RESALE_AWS_SERVICES, HARDWARE_PRODUCTS, SOFTWARE_PRODUCTS, MANAGED_SERVICES]"

Delta reported by replay: $.Message value_mismatch — the substring "Value 'NOT_A_SOLUTION_TYPE' at 'primarySolutionType'" is served as "Value at 'primarySolutionType'". The clientToken clause of the same message retains its value clause, which isolates the defect to the enum branch of the formatter.

Fixed vs variable in that message: fixed text is "2 validation errors detected: ", "Value '", "' at '", "' failed to satisfy constraint: ", "Member must have length less than or equal to 64", "; ", "Member must satisfy enum value set: [...]"; variable parts are the clientToken string and the primarySolutionType string, both supplied by the request (the literal 'sim2real-probe-...-076ba00b' and 'NOT_A_SOLUTION_TYPE' are that recording's inputs, not requirements), plus the member paths, the length bound 64 from the model and the enum value set from the model.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a partnercentral-account client at its data listener.
2. Call CreatePartner with: a valid Catalog, LegalName, AllianceLeadContact and EmailVerificationCode; a ClientToken longer than 64 characters; and PrimarySolutionType set to a value outside the PrimarySolutionType enum (e.g. 'NOT_A_SOLUTION_TYPE').
3. Catch the ClientError and read response['Error']['Code'] and response['Error']['Message'].
4. Observe Code == 'ValidationException' and HTTP 400 (both correct), but the second clause of the message reads "Value at 'primarySolutionType' failed to satisfy constraint: ..." with no "Value '<the value you sent>' " prefix, while the clientToken clause in the same message does include its value.
5. Re-replay fixture partnercentral-account-partner-onboarding-attempt, step task#3, to confirm the fix; the earlier steps of that fixture must keep passing.
