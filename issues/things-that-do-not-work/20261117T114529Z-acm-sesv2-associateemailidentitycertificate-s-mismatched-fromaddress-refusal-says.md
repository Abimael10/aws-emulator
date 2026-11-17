Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__sesv2.associate-email-identity-certificate-email-address-mismatch-rejected
Issue-Key: sesv2|AssociateEmailIdentityCertificate|BadRequestException
Issue-Commit: 14940b74adde738ffb545252b71aede81788f071

# acm__sesv2: AssociateEmailIdentityCertificate's mismatched-FromAddress refusal says "must exactly match" instead of AWS's "must match"

> _Note: this issue was identified against the code base at commit `14940b74adde738ffb545252b71aede81788f071`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateEmailIdentityCertificate

## Description
Cause 1 of 1: the BadRequestException message for a FromAddress that does not match the EMAIL_ADDRESS identity is reworded.
AWS: recorded behavior only for the text — Smithy (sesv2) models AssociateEmailIdentityCertificate with inputs EmailIdentity (required), FromAddress (optional) and CertificateArn (required), and BadRequestException at HTTP 400. The recording shows the refusal message "FromAddress <{from-address}> must match the email identity <{identity}>." — the fixed text is "FromAddress <" ... "> must match the email identity <" ... ">.", with both angle-bracketed values taken from the request (the supplied FromAddress) and from the targeted identity. Status 400 and Error.Code BadRequestException are correct in the emulator; only the wording diverges.
Emulator: the same call answers 400 BadRequestException with "FromAddress <{from-address}> must exactly match the email identity <{identity}>." — the extra word "exactly" is emulator-authored.
Why: a client that matches on the refusal text (log assertions, user-facing error mapping, retry/branch decisions keyed on the message) behaves differently against the emulator than against AWS; the message is part of the error contract, so any difference in its fixed text is a defect even though the code and status are right.
Fix: the sesv2 AssociateEmailIdentityCertificate handler's FromAddress validation — emit AWS's exact wording, interpolating the request's FromAddress and the identity being associated.
Traces: acm__sesv2.associate-email-identity-certificate-email-address-mismatch-rejected.

## Evidence
Expected: ('BadRequestException', 'FromAddress <{from-address}> must match the email identity <{identity}>.')
Actual:   ('BadRequestException', 'FromAddress <{from-address}> must exactly match the email identity <{identity}>.')
From the replay (per-run mailbox names fill the template): expected 'FromAddress <sim2real-behav-notowner-...@example.com> must match the email identity <sim2real-behav-emailid-...@example.com>.', got the same string with "must exactly match". HTTP status 400 and Error.Code BadRequestException agree; the rest of the check (omitted FromAddress associates, identical FromAddress associates, no association is created by the rejected call, positive control re-associates) is unaffected.

## Steps to Reproduce
1. sesv2 CreateEmailIdentity with an email-address identity <id>@example.com.
2. acm RequestCertificate(DomainName=<something>.example.com, ValidationMethod=DNS) and keep the ARN.
3. sesv2 AssociateEmailIdentityCertificate(EmailIdentity=<id>@example.com, FromAddress=<other>@example.com, CertificateArn=<arn>).
4. Read Error.Message: the emulator says "FromAddress <...> must exactly match the email identity <...>." where AWS says "FromAddress <...> must match the email identity <...>."
