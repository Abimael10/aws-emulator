Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: securityhub.enable-organization-admin-account-format-checked-before-authority
Issue-Shape-Id: com.amazonaws.securityhub#SecurityHubAPIService
Issue-Key: securityhub|EnableOrganizationAdminAccount|NotImplemented
Issue-Commit: 5f72b15ceea68fb83a63d5bc988a846400d3d8ef

# Implement securityhub EnableOrganizationAdminAccount: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `5f72b15ceea68fb83a63d5bc988a846400d3d8ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EnableOrganizationAdminAccount

## Description
`EnableOrganizationAdminAccount` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-securityhub-securityhubapiservice/src/ops/enable_organization_admin_account.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `EnableOrganizationAdminAccount` call, so each one unblocks only when `EnableOrganizationAdminAccount` works: `securityhub.enable-organization-admin-account-format-checked-before-authority`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-securityhub-securityhubapiservice/src/ops/enable_organization_admin_account.rs`. Replaying the corpus, the emulator refused the `EnableOrganizationAdminAccount` call: File "check_securityhub_enable_organization_admin_account_format_checked_before_authority.py", line 25, in run
    expect_error(lambda: sh.enable_organization_admin_account(AdminAccountId=_BAD_ACCOUNT_ID), *_INVALID_FORMAT)
  File "check_securityhub_enable_organization_admin_account_format_checked_before_authority.py", line 73, in expect_error
    assert actual_code == code, f"expected {code}, got {actual_code} ({actual_message})"
           ^^^^^^^^^^^^^^^^^^^
AssertionError: expected InvalidInputException, got NotImplemented (operation EnableOrganizationAdminAccount is scaffolded but not implemented in this emulator)

## Steps to Reproduce
Boot the securityhub emulator
Call EnableOrganizationAdminAccount and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
