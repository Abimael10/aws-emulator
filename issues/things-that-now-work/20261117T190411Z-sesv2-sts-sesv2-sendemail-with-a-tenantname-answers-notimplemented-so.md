Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sesv2__sts.send-email-tenant-resolution-precedes-sender-verification
Issue-Key: sesv2__sts.send-email-tenant-resolution-precedes-sender-verification|run#15|AssertionError|assert ghost_result == (
Issue-Commit: 78812e82c5d05e63ee0f30b5ef7177ddca283e5f

# sesv2: SendEmail with a TenantName answers NotImplemented, so tenant resolution never precedes sender verification

> _Note: this issue was identified against the code base at commit `78812e82c5d05e63ee0f30b5ef7177ddca283e5f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendEmail

## Description
Cause 1 of 1: the TenantName branch of SendEmail is an unimplemented scaffold, so a send that names a tenant is refused with an unmodeled NotImplemented error instead of being resolved against the account's tenants.
AWS: Smithy: SendEmail (POST /v2/email/outbound-emails) accepts an optional TenantName member (target TenantName, @length min 1) and models NotFoundException (HTTP 404) alongside MessageRejected (HTTP 400). Recorded behavior: tenant resolution happens BEFORE the sender identity is verified — a SendEmail whose TenantName names no tenant in the account is refused HTTP 404 Error.Code `NotFoundException` with the message `Tenant <TenantName> for AwsAccountId <account id> not found.` (fixed text `Tenant `, ` for AwsAccountId `, ` not found.`; the variable parts are the TenantName taken from the request and the caller's 12-digit account id). With a TenantName that does resolve — or with no TenantName at all — the same send from an unverified sender is instead refused HTTP 400 Error.Code `MessageRejected` with `Email address is not verified. The following identities failed the check in region <REGION>: <sender address>` (fixed text apart from the upper-cased region and the sender address from the request), and both of those legs must produce the identical code and message.
Emulator: the call answers Error.Code `NotImplemented` with the message `operation SendEmail with TenantName is scaffolded but not implemented in this emulator`, for both the unresolvable and the resolvable tenant, so neither the 404 NotFoundException nor the ordering against sender verification is observable.
Why: a multi-tenant SES client cannot send at all through a tenant, and it receives an error code that is not in SendEmail's modeled error set — so its NotFoundException (bad tenant) and MessageRejected (unverified sender) handling never runs, and it cannot tell a missing tenant from a rejected message. Note: the existing open issues for SendEmail cover other branches (20261027T161051Z / 20261114T220956Z for Content.Template, 20261109T232952Z for the configuration-set send); none of them covers the TenantName branch.
Fix: implement the TenantName branch of the SendEmail handler — resolve TenantName against the account's tenant store and refuse an unknown one with NotFoundException and AWS's message template, placing that resolution ahead of the from-identity verification check that produces MessageRejected.
Traces: sesv2__sts.send-email-tenant-resolution-precedes-sender-verification.

## Evidence
sesv2__sts.send-email-tenant-resolution-precedes-sender-verification:
  SendEmail(FromEmailAddress=<unverified sender>, Destination={ToAddresses:[success@simulator.amazonses.com]}, Content={Simple:{...}}, TenantName=<name of no tenant>)
  expected: ('NotFoundException', 'Tenant <ghost tenant> for AwsAccountId <account id> not found.')  [HTTP 404]
  got:      ('NotImplemented', 'operation SendEmail with TenantName is scaffolded but not implemented in this emulator')

Positive controls in the same check already pass: CreateTenant/GetTenant round-trip for the live tenant, and GetTenant on the ghost name is correctly refused NotFoundException 'The requested tenant <name> does not exist.' — so the tenant store itself resolves; only SendEmail's TenantName branch is missing.

The two downstream legs were therefore never reached:
  SendEmail with the EXISTING TenantName -> expected ('MessageRejected', 'Email address is not verified. The following identities failed the check in region <REGION>: <sender>')
  SendEmail with NO TenantName          -> expected the identical ('MessageRejected', <same message>)

## Steps to Reproduce
1. sesv2:CreateTenant(TenantName=<live>) and confirm sesv2:GetTenant(TenantName=<live>) returns it.
2. Pick a tenant name that does not exist and confirm sesv2:GetTenant on it is refused NotFoundException.
3. Call sesv2:SendEmail(FromEmailAddress=<an UNVERIFIED address>, Destination={"ToAddresses":["success@simulator.amazonses.com"]}, Content={"Simple":{"Subject":{"Data":"..."},"Body":{"Text":{"Data":"..."}}}}, TenantName=<the nonexistent name>) — expect HTTP 404 NotFoundException "Tenant <name> for AwsAccountId <account> not found."; the emulator answers NotImplemented.
4. Repeat step 3 with TenantName=<live> and then with no TenantName at all — both must be refused HTTP 400 MessageRejected with the identical "Email address is not verified. ..." message.
