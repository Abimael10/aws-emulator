Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatchomni.space-creation-seeds-service-managed-profile-and-grants cloudwatchomni.service-managed-profile-and-grant-cannot-be-changed cloudwatchomni.update-dashboard-rejects-invalid-body-and-keeps-stored-body cloudwatchomni.search-principals-requires-an-identity-center-domain
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni.space-creation-seeds-service-managed-profile-and-grants|run#8|AssertionError|assert [(p["name"], p["profileType"]) for p in profiles] == [
Issue-Key: cloudwatchomni.service-managed-profile-and-grant-cannot-be-changed|run#4|StopIteration|profile = next(p for p in cwo.list_access_profiles(spaceId=space_id)["items"]
Issue-Key: cloudwatchomni.update-dashboard-rejects-invalid-body-and-keeps-stored-body|run#11|AssertionError|assert message == (
Issue-Key: cloudwatchomni.search-principals-requires-an-identity-center-domain|run#5|AssertionError|assert message == (
Issue-Commit: ddd7f54964d818ea183d1c749ef5ca4926ba59fa

# cloudwatchomni: CreateSpace seeds no service-managed access profile or grants, dashboard body schema skips additionalProperties, SearchPrincipals IdC message re-worded

> _Note: this issue was identified against the code base at commit `ddd7f54964d818ea183d1c749ef5ca4926ba59fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSpace

## Description
Behavioral replay of the cloudwatchomni suite surfaced THREE distinct root causes in this service that no already-filed issue carries the replay key for. This bundle has 3 causes; each must be closed before submitting.

=== Cause 1 of 3: CreateSpace provisions none of the space's service-managed access profile or access grants ===
Failing checks: cloudwatchomni.space-creation-seeds-service-managed-profile-and-grants, cloudwatchomni.service-managed-profile-and-grant-cannot-be-changed

AWS: Smithy model com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend. CreateSpace (required input name @length 3..64 pattern ^[a-z0-9]+(-[a-z0-9]+)*$, domainId pattern ^d-[0-9a-z]{1,25}$, dataAccessRoleArn) returns the Space; the model declares AccessProfileType and AccessGrantType with value SERVICE_MANAGED documented as "Provisioned and owned by the service. Cannot be deleted by customers.", and DeleteAccessGrant's documentation states "A service-managed grant cannot be deleted." The model therefore states that service-managed profiles and grants exist and are service-provisioned, but does not state what CreateSpace seeds; for the seeded set this is recorded behavior only. Recorded behavior: immediately after CreateSpace succeeds, ListAccessProfiles(spaceId) returns exactly one AccessProfileSummary with name "DefaultAccessProfileForAsyncWorkflows" and profileType SERVICE_MANAGED, and ListAccessGrants(spaceId) returns exactly two AccessGrantSummary items whose names are "OmniDefaults-Space-Admin" (permission SPACE_ADMIN, grantType SERVICE_MANAGED) and "OmniDefaults-AsyncAccessProfile-AgentCanAssume" (permission CUSTOM, grantType SERVICE_MANAGED, principal.principalType AGENT, principal.principalId derived from the created space as "agent-<spaceId>"), both carrying the domainId the space was created in. Those two names and the profile name are fixed text; the spaceId inside the agent principalId and the domainId are values from the created state, not literals.

Emulator: CreateSpace returns an ACTIVE space with encryptionConfiguration {"encryptionStrategy": "AWS_OWNED"} (that part agrees), but the space is seeded empty. ListAccessProfiles(spaceId) returns items == [] (check space-creation-seeds-...: "AssertionError: unexpected seeded profiles []"), and because no SERVICE_MANAGED profile exists the second check cannot even select one and dies with a bare StopIteration on `next(p for p in cwo.list_access_profiles(spaceId=space_id)["items"] if p["profileType"] == "SERVICE_MANAGED")`. The service-managed immutability refusals that check goes on to assert were therefore never reached and are unverified: once the seeding exists, UpdateAccessProfile and DeleteAccessProfile on the SERVICE_MANAGED profile must be refused HTTP 409 ConflictException with the messages "A system-managed access profile cannot be updated" and "A system-managed access profile cannot be deleted" respectively, and DeleteAccessGrant on either SERVICE_MANAGED grant must be refused HTTP 409 ConflictException with "A system-managed grant cannot be deleted" (all three strings are wholly fixed text, with no request- or state-derived parts), leaving the profile's description and both grants intact. Fixing only the seeding will leave this check failing on those refusals.

Why: a client relying on AWS's behavior sees a brand-new space as having no access control at all. Callers that enumerate a space's profiles/grants to discover who already has SPACE_ADMIN, or that look up the agent principal "agent-<spaceId>" to attach async-workflow permissions, get an empty list and conclude the space is unconfigured — so they either create duplicate customer-managed profiles/grants that collide with the service-managed ones on real AWS, or they fail outright with a KeyError/StopIteration as this check did. Callers that probe immutability by attempting to delete the service-managed grant get a not-found instead of the 409 ConflictException AWS uses, so cleanup logic that treats 409 as "leave it alone" deletes nothing it should and reports success.

Fix: the CreateSpace handler and the space state it writes — space creation must materialise the one SERVICE_MANAGED access profile and the two SERVICE_MANAGED grants into the same store ListAccessProfiles/ListAccessGrants/GetAccessProfile read, with the agent grant's principalId derived from the new space id and both grants carrying the space's domainId; plus the UpdateAccessProfile, DeleteAccessProfile and DeleteAccessGrant handlers' guard branches that must classify a service-managed row as a 409 ConflictException rather than mutating or deleting it. DeleteSpace must remove the seeded rows with the space.

Traces: behavioral checks cloudwatchomni.space-creation-seeds-service-managed-profile-and-grants and cloudwatchomni.service-managed-profile-and-grant-cannot-be-changed.

=== Cause 2 of 3: UpdateOmniDashboard body validation never reports the additionalProperties violation ===
Failing check: cloudwatchomni.update-dashboard-rejects-invalid-body-and-keeps-stored-body

AWS: Smithy UpdateOmniDashboard (input spaceId, dashboardId required; body @length 1..1048576; name, description optional; doc "Only the provided fields are changed; omitted fields are left unchanged.") models ValidationException with httpError 400. The body's JSON schema is not in the model, so the schema rules are recorded behavior only. Recorded behavior: a body that is not JSON is refused 400 ValidationException with "dashboard body is not valid JSON at line 1 column 1", and a body whose panel object is missing its required keys and also carries an unknown key is refused 400 ValidationException with every schema violation reported, joined by "; ", in schema order: "panels[0].type: does not satisfy the 'required' constraint of the dashboard body schema; panels[0].layout: does not satisfy the 'required' constraint of the dashboard body schema; panels[0]: does not satisfy the 'additionalProperties' constraint of the dashboard body schema". Fixed text per violation is "<pointer>: does not satisfy the '<constraint>' constraint of the dashboard body schema"; the pointer (panels[0], panels[0].type) and the constraint name are derived from the submitted body and the schema, not literals.

Emulator: the non-JSON refusal matches, but the schema refusal reports only the two 'required' violations and drops the 'additionalProperties' one entirely — actual message "panels[0].type: does not satisfy the 'required' constraint of the dashboard body schema; panels[0].layout: does not satisfy the 'required' constraint of the dashboard body schema" for the body {"panels": [{"x": 1}]}, where the unknown panel key "x" must also be reported. Status (400) and error code (ValidationException) agree; the message is short one clause, i.e. the emulator's dashboard-body schema does not enforce additionalProperties: false on a panel object.

Why: a client relying on AWS's behavior uses the message to fix its dashboard JSON. Against the emulator a body with a misspelled or unsupported panel key passes that part of validation silently, so an author iterating against the emulator removes the reported 'required' problems, gets an accepted body, and is then refused by real AWS for the unknown key it was never told about. Tooling that parses the violation list to annotate the offending JSON pointers also loses the additionalProperties annotation.

Fix: the UpdateOmniDashboard handler's dashboard-body schema validator (and the CreateOmniDashboard path if it shares it) — enforce the panel object's additionalProperties rule and append its violation to the same "; "-joined list in schema order, rather than reporting only missing required members. Note: issue 20261111T022337Z-cloudwatchomni-updateomnidashboard-stub.md is still open but does NOT cover this — UpdateOmniDashboard now answers and validates, it is no longer a NotImplemented stub, and that issue carries no replay key for this check.

Traces: behavioral check cloudwatchomni.update-dashboard-rejects-invalid-body-and-keeps-stored-body.

=== Cause 3 of 3: SearchPrincipals Identity-Center precondition message is the emulator's own prose ===
Failing check: cloudwatchomni.search-principals-requires-an-identity-center-domain

AWS: Smithy SearchPrincipals (required input domainId pattern ^d-[0-9a-z]{1,25}$, searchQuery @length 1..128; errors include ValidationException httpError 400) documents the precondition that the domain must be configured with Identity Center. The message text is not in the model, so for the wording this is recorded behavior only: a domain whose identityProviders do not include IDC is refused HTTP 400 ValidationException with "Domain does not have an IdC application ARN configured. The domain must be provisioned with Identity Center before this operation can proceed." That string is entirely fixed text — it contains no request- or state-derived parts, in particular no domain id and no operation name.

Emulator: right status and error code, wrong message — "Domain 'd-<domainId>' is not configured with Identity Center. SearchPrincipals requires a domain whose identityProviders include IDC." The emulator interpolates the requested domainId and the operation name, neither of which appears in AWS's constant message, and both sentences differ in fixed text. (The check's follow-up GetDomain assertion — status ACTIVE with identityProviders ["IAM"] — was not reached.)

Why: AWS's message is a constant, so callers match it verbatim to distinguish "this domain still needs Identity Center provisioning" from any other 400 ValidationException on the same operation; the message is the only discriminator. Against the emulator that match never fires, so the caller reports a generic validation failure or retries a call that cannot succeed, and callers that log the text leak the domain id in an error where AWS emits no identifiers.

Fix: the SearchPrincipals handler's Identity-Center precondition branch — emit the fixed AWS message and stop interpolating the domain id and operation name; share one constant formatter with any other operation carrying the same precondition.

Related but NOT covering this check: open issue 20261115T152312Z-cloudwatchomni-searchprincipals-identity-center-precondition-message-diverges-from.md describes the identical wording divergence from the WIRE corpus (fixture cloudwatchomni-deep-link-and-identity-center-refusals step task#5). It carries no key for this behavioral check, so this check would not be re-replayed against it; closing one should close both, and the replay key for this check is recorded here.

Traces: behavioral check cloudwatchomni.search-principals-requires-an-identity-center-domain.

## Evidence
Suite simparity-behavioral-aws-v1, service heading ### cloudwatchomni (6/34 passed).

Cause 1 — [mismatch] cloudwatchomni.space-creation-seeds-service-managed-profile-and-grants:
  check_cloudwatchomni_space_creation_seeds_service_managed_profile_and_grants.py line 83:
  assert [(p["name"], p["profileType"]) for p in profiles] == [("DefaultAccessProfileForAsyncWorkflows", "SERVICE_MANAGED")]
  AssertionError: unexpected seeded profiles []
  expected: ListAccessProfiles(spaceId) -> 1 item, name "DefaultAccessProfileForAsyncWorkflows", profileType SERVICE_MANAGED;
            ListAccessGrants(spaceId) -> 2 items named "OmniDefaults-Space-Admin" (SPACE_ADMIN/SERVICE_MANAGED) and
            "OmniDefaults-AsyncAccessProfile-AgentCanAssume" (CUSTOM/SERVICE_MANAGED, principalType AGENT,
            principalId "agent-<spaceId>"), both with domainId == the space's domain.
  actual:   ListAccessProfiles(spaceId)["items"] == []

Cause 1 — [error] cloudwatchomni.service-managed-profile-and-grant-cannot-be-changed:
  check_cloudwatchomni_service_managed_profile_and_grant_cannot_be_changed.py line 75:
  profile = next(p for p in cwo.list_access_profiles(spaceId=space_id)["items"] if p["profileType"] == "SERVICE_MANAGED")
  StopIteration  (no SERVICE_MANAGED profile exists, so the run aborts before any assertion)
  unreached expectations: UpdateAccessProfile -> 409 ConflictException "A system-managed access profile cannot be updated";
            DeleteAccessProfile -> 409 ConflictException "A system-managed access profile cannot be deleted";
            DeleteAccessGrant (each of the 2 service-managed grants) -> 409 ConflictException
            "A system-managed grant cannot be deleted"; profile description and both grants unchanged afterwards.
  Model citation: AccessProfileType/AccessGrantType value SERVICE_MANAGED doc "Provisioned and owned by the service.
            Cannot be deleted by customers."; DeleteAccessGrant doc "A service-managed grant cannot be deleted.";
            ConflictException httpError 409 is modeled on UpdateAccessProfile, DeleteAccessProfile and DeleteAccessGrant.

Cause 2 — [mismatch] cloudwatchomni.update-dashboard-rejects-invalid-body-and-keeps-stored-body:
  check_..._update_dashboard_rejects_invalid_body_and_keeps_stored_body.py line 90, body {"panels": [{"x": 1}]}:
  code == ValidationException (agrees), message diverges.
  expected: "panels[0].type: does not satisfy the 'required' constraint of the dashboard body schema; panels[0].layout: does not satisfy the 'required' constraint of the dashboard body schema; panels[0]: does not satisfy the 'additionalProperties' constraint of the dashboard body schema"
  actual:   "panels[0].type: does not satisfy the 'required' constraint of the dashboard body schema; panels[0].layout: does not satisfy the 'required' constraint of the dashboard body schema"
  The missing third clause is the additionalProperties violation for the unknown panel key. The earlier non-JSON
  assertion ("dashboard body is not valid JSON at line 1 column 1") passed, so the 400/ValidationException wiring is fine.
  Per-violation fixed text: "<pointer>: does not satisfy the '<constraint>' constraint of the dashboard body schema";
  pointer and constraint name are derived from the submitted body, not literals.

Cause 3 — [mismatch] cloudwatchomni.search-principals-requires-an-identity-center-domain:
  check_..._search_principals_requires_an_identity_center_domain.py line 76:
  code == ValidationException and HTTP 400 both agree; message diverges.
  expected: "Domain does not have an IdC application ARN configured. The domain must be provisioned with Identity Center before this operation can proceed."  (wholly fixed text)
  actual:   "Domain 'd-4o3cughkwg41hza978y8jc8h2' is not configured with Identity Center. SearchPrincipals requires a domain whose identityProviders include IDC."
  In the actual string, "d-4o3cughkwg41hza978y8jc8h2" is the domainId from the request (a per-run value, not a requirement)
  and "SearchPrincipals" is the operation name; AWS's message contains neither.
  Model citation: SearchPrincipals input domainId (required, ^d-[0-9a-z]{1,25}$), searchQuery (required, @length 1..128);
  errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400).

## Steps to Reproduce
1. Build and boot the emulator, then replay the simparity-behavioral-aws-v1 suite (checks listed in trace_ids).

Cause 1 — CreateSpace seeding:
2. CreateDomain(name=<unique>, identityProviders=["IAM"]); create an IAM role CloudWatch can assume; CreateSpace(name=<unique lowercase-hyphen>, domainId=<that domain>, dataAccessRoleArn=<that role arn>).
3. Call ListAccessProfiles(spaceId=<new space>) and ListAccessGrants(spaceId=<new space>) (paginate both). Observe items == [] from both, where AWS returns the one SERVICE_MANAGED profile "DefaultAccessProfileForAsyncWorkflows" and the two SERVICE_MANAGED grants "OmniDefaults-Space-Admin" and "OmniDefaults-AsyncAccessProfile-AgentCanAssume" (the latter with principalType AGENT and principalId "agent-<spaceId>"), both grants carrying the space's domainId.
4. After the seeding exists, call UpdateAccessProfile(spaceId, profileId=<the SERVICE_MANAGED profile>, description="changed"), DeleteAccessProfile(spaceId, profileId=<same>) and DeleteAccessGrant(grantId=<each SERVICE_MANAGED grant>). Each must answer HTTP 409 ConflictException with, respectively, "A system-managed access profile cannot be updated", "A system-managed access profile cannot be deleted", "A system-managed grant cannot be deleted", and GetAccessProfile / ListAccessGrants must show the profile description and both grants unchanged.

Cause 2 — dashboard body schema:
5. In that space, CreateOmniDashboard(spaceId, name=<unique>, body='{"panels": []}', description="kept").
6. Call UpdateOmniDashboard(spaceId, dashboardId, body='{"panels": [{"x": 1}]}'). Observe 400 ValidationException whose message lists only the two 'required' violations; the 'additionalProperties' violation for the unknown panel key "x" (clause "panels[0]: does not satisfy the 'additionalProperties' constraint of the dashboard body schema") is missing.
7. Confirm the refused updates left body and description unchanged via GetOmniDashboard, and that a description-only UpdateOmniDashboard keeps the body.

Cause 3 — SearchPrincipals precondition message:
8. With the IAM-only domain from step 2, call SearchPrincipals(domainId=<that domain>, searchQuery="*").
9. Observe HTTP 400 ValidationException, as expected, but with the message "Domain '<domainId>' is not configured with Identity Center. SearchPrincipals requires a domain whose identityProviders include IDC." instead of the constant "Domain does not have an IdC application ARN configured. The domain must be provisioned with Identity Center before this operation can proceed." The divergence reproduces for every domain in that state; the emulator's text varies with the domainId supplied, AWS's does not.
