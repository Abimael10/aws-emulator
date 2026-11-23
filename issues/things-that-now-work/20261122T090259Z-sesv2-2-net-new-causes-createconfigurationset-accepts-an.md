Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sesv2.sesv2-configuration-set-create-refusals-persist-nothing-then-name-reusable sesv2.tenant-name-validation-and-associated-resource-delete-blocked
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2.sesv2-configuration-set-create-refusals-persist-nothing-then-name-reusable|expect_error#9|AssertionError|raise AssertionError(f"expected {(code, message)!r}, but the call succeeded: {result!r}")
Issue-Key: sesv2.tenant-name-validation-and-associated-resource-delete-blocked|_expect_error#19|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Commit: 58e8e75a7b1ef1cb8dd0bc31b7ab38eaf68cbf98

# sesv2: 2 net-new causes — CreateConfigurationSet accepts an unverified CustomRedirectDomain, and DeleteConfigurationSet deletes a set that still has tenant associations

> _Note: this issue was identified against the code base at commit `58e8e75a7b1ef1cb8dd0bc31b7ab38eaf68cbf98`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfigurationSet, DeleteConfigurationSet

## Description
Two independent, net-new root causes in the SESv2 configuration-set control plane. Both are the same shape of defect — a precondition AWS enforces before mutating is not evaluated at all, so the emulator answers 200 with an empty body where AWS refuses with 400 BadRequestException. Neither is covered by an existing open issue: 20261118T223027Z-sesv2-8-net-new-causes-the-smtp-submission.md covers seven other control-plane validations (contact-list limits, identity/domain-label and template-tag rules, tenant suppression pairs) and the SMTP data plane, and 20261118T045426Z-sesv2-non-send-reads-skip-modeled-validation-and.md covers read-path validation; neither names the CustomRedirectDomain verification gate or the tenant-association delete gate.

Cause 1 of 2: CreateConfigurationSet does not verify TrackingOptions.CustomRedirectDomain against the account's verified identities.
AWS: Smithy: CreateConfigurationSet input member TrackingOptions -> TrackingOptions with required member CustomRedirectDomain; modeled errors include BadRequestException (httpError 400, "The input you provided is invalid."). A custom open/click tracking domain must be an identity already verified in the calling account, so a create naming an unverified domain is refused and NO configuration set is persisted. Recorded response: HTTP 400, Error.Code "BadRequestException", Error.Message "Domain <DOMAIN> is not verified under this account." The fixed text is "Domain <" ... "> is not verified under this account."; the only variable part is DOMAIN, taken verbatim from the request's TrackingOptions.CustomRedirectDomain (the recording's click.sim2real-behav-probe-<suffix>.example.com is one run's value, not part of the requirement). Because the create is refused, the name stays free: a subsequent GetConfigurationSet for it must answer 404 NotFoundException "Configuration set <NAME> does not exist." and the same name must then create cleanly without TrackingOptions.
Emulator: the call returns HTTP 200 with an empty JSON body ({}), content-type application/json — the verification gate is never evaluated. The check fails at the point where it expects the refusal: "expected ('BadRequestException', 'Domain <...> is not verified under this account.'), but the call succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}}". The neighbouring name-shape refusal in the same check (BadRequestException "Invalid configuration set name <bad name!>: only alphanumeric ASCII characters, '_', and '-' are allowed.") does pass, so the defect is specifically the missing CustomRedirectDomain verification, not configuration-set validation in general.
Why: a client that uses the refusal to detect "you must verify this domain first" sees success, then has a configuration set persisted with a tracking domain that can never serve open/click redirects. The name is also consumed, so the client's follow-up create of the same name on the legal path gets AlreadyExistsException against AWS's "name is still free" semantics, and its error-handling branch for the unverified-domain case is never exercised.
Fix: CreateConfigurationSet's handler validation — resolve TrackingOptions.CustomRedirectDomain against verified email identities in the account's state and return BadRequestException before any state mutation, so the refusal persists nothing.
Traces: sesv2.sesv2-configuration-set-create-refusals-persist-nothing-then-name-reusable.

Cause 2 of 2: DeleteConfigurationSet ignores tenant resource associations and deletes the set anyway.
AWS: Smithy: DeleteConfigurationSet input ConfigurationSetName (httpLabel, required), modeled errors BadRequestException (400), NotFoundException (404); CreateTenantResourceAssociation binds a configuration-set ARN to a tenant. A configuration set that is still associated with a tenant cannot be deleted. Recorded response: HTTP 400, Error.Code "BadRequestException", Error.Message "Cannot delete <ARN> because it has tenant associations. Remove all tenant associations and try again." The fixed text is "Cannot delete <" ... "> because it has tenant associations. Remove all tenant associations and try again."; the one variable part is ARN — the configuration set's ARN (arn:aws:ses:<region>:<account>:configuration-set/<name>) derived from the request and the emulator's state, not a literal from any single run. After the refusal the set must survive a read (GetConfigurationSet returns it), and once DeleteTenantResourceAssociation removes the last association the same delete must succeed and the set must then read back 404 NotFoundException "Configuration set <NAME> does not exist."
Emulator: the delete returns HTTP 200 with an empty body ({}), content-type application/x-amz-json-1.1 — the association check is never performed. The check fails with "expected the call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, ...}}". The tenant-name validation and positive tenant/configuration-set creates earlier in the same check pass, so the gap is the referential-integrity gate on delete.
Why: the tenant association is left dangling — a client that relies on the refusal to learn which tenants still reference a set destroys a set that tenants still point at, and ListTenantResources can keep reporting an ARN that no longer resolves. Teardown and migration logic that catches this BadRequestException to disassociate first never runs, so the account is silently left in a state AWS makes unreachable.
Fix: DeleteConfigurationSet's handler validation/state — before removing the set, look up tenant resource associations for its ARN and refuse with BadRequestException when any remain, leaving the set intact.
Traces: sesv2.tenant-name-validation-and-associated-resource-delete-blocked.

## Evidence
Cause 1 — check sesv2.sesv2-configuration-set-create-refusals-persist-nothing-then-name-reusable:
Expected (AWS): HTTP 400, Error.Code=BadRequestException, Error.Message="Domain <click.<probe-name>.example.com> is not verified under this account."; no configuration set persisted; GetConfigurationSet on the name then 404 NotFoundException "Configuration set <NAME> does not exist."
Actual (emulator): HTTP 200, empty JSON body. Harness output: AssertionError: expected ('BadRequestException', 'Domain <click.sim2real-behav-probe-a5d2f0246fcb.example.com> is not verified under this account.'), but the call succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '2', ...}, 'RetryAttempts': 0}}
Control that passes in the same check: CreateConfigurationSet(ConfigurationSetName="bad name!") is correctly refused with BadRequestException "Invalid configuration set name <bad name!>: only alphanumeric ASCII characters, '_', and '-' are allowed."

Cause 2 — check sesv2.tenant-name-validation-and-associated-resource-delete-blocked:
Setup that succeeds: CreateTenant(<tenant>), CreateConfigurationSet(<cfgset>), CreateTenantResourceAssociation(TenantName=<tenant>, ResourceArn=arn:aws:ses:<region>:<account>:configuration-set/<cfgset>).
Expected (AWS): DeleteConfigurationSet(ConfigurationSetName=<cfgset>) -> HTTP 400, Error.Code=BadRequestException, Error.Message="Cannot delete <arn:aws:ses:<region>:<account>:configuration-set/<cfgset>> because it has tenant associations. Remove all tenant associations and try again."; the set survives GetConfigurationSet.
Actual (emulator): HTTP 200, empty body. Harness output: AssertionError: expected the call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.1', 'content-length': '2', ...}, 'RetryAttempts': 0}}
Model citations: CreateConfigurationSet errors {AlreadyExistsException 400, BadRequestException 400, ConcurrentModificationException 500, LimitExceededException 400, NotFoundException 404, TooManyRequestsException 429}; DeleteConfigurationSet errors {BadRequestException 400, ConcurrentModificationException 500, NotFoundException 404, TooManyRequestsException 429}; TrackingOptions.CustomRedirectDomain required.

## Steps to Reproduce
Cause 1:
1. Point a boto3 sesv2 client at the emulator.
2. Call create_configuration_set(ConfigurationSetName="<valid-name>", TrackingOptions={"CustomRedirectDomain": "click.<some-unverified-domain>", "HttpsPolicy": "REQUIRE"}) where that domain is NOT a verified identity in the account.
3. Observe HTTP 200 with an empty body. AWS returns 400 BadRequestException "Domain <click.<some-unverified-domain>> is not verified under this account."
4. Call get_configuration_set(ConfigurationSetName="<valid-name>") — AWS answers 404 NotFoundException "Configuration set <<valid-name>> does not exist." because the refused create persisted nothing; then create the same name without TrackingOptions and confirm it succeeds with no TrackingOptions on read.

Cause 2:
1. create_tenant(TenantName="<tenant>") and create_configuration_set(ConfigurationSetName="<cfgset>").
2. create_tenant_resource_association(TenantName="<tenant>", ResourceArn="arn:aws:ses:<region>:<account>:configuration-set/<cfgset>").
3. Call delete_configuration_set(ConfigurationSetName="<cfgset>"). Observe HTTP 200. AWS returns 400 BadRequestException "Cannot delete <arn:aws:ses:<region>:<account>:configuration-set/<cfgset>> because it has tenant associations. Remove all tenant associations and try again." and the set survives get_configuration_set.
4. delete_tenant_resource_association for that ARN, confirm list_tenant_resources(<tenant>) is empty, then delete_configuration_set succeeds and get_configuration_set answers 404 NotFoundException "Configuration set <<cfgset>> does not exist."

Replay: run_behavioral_suite and re-run the two checks in trace_ids.
