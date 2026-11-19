Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatchomni.domain-name-pattern-and-identity-center-rules-reject-updates
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni.domain-name-pattern-and-identity-center-rules-reject-updates|_error#7|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 1a6df7b32e4c8350e561c708bcd111a16150a036

# cloudwatchomni UpdateDomain accepts a switch to the IDC identity provider with no Identity Center instance (200 instead of 400 ValidationException) and mints an identityCenterApplicationArn out of nothing

> _Note: this issue was identified against the code base at commit `1a6df7b32e4c8350e561c708bcd111a16150a036`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateDomain

## Description
ONE net-new root cause in the cloudwatchomni emulator: UpdateDomain does not enforce the Identity Center precondition on `identityProviders`, so a request that switches a domain to IDC while no Identity Center instance is configured is applied and answered 200 instead of being refused.

Related but NOT covering this: open issue 20261111T022337Z-cloudwatchomni-conflict-and-validation-error-messages-diverge-from.md reports, as its cause 5, UpdateDomain's @pattern message for `name` (value clause + anchored regex). That half now behaves: the check's first refusal assertion passes and the check advances to the second refusal, which is the cause reported here. The open issue's cause 5 only notes in prose that this second refusal was "not yet reachable"; it carries no replay key for it, and the replay now fails on an entirely different assertion (a 200 success, not a message mismatch). Open issue 20261115T152312Z-cloudwatchomni-searchprincipals-identity-center-precondition-message-diverges-from.md concerns the SearchPrincipals precondition message, a different operation. The fixed issue 20261111T022337Z-cloudwatchomni-updatedomain-stub.md stood the operation up; this is not a refusal it left behind, the call succeeds.

Cause 1 of 1: UpdateDomain does not require an Identity Center instance when identityProviders includes IDC.
  AWS: Smithy UpdateDomain declares ValidationException (errorKind client, @httpError 400) and takes `identityProviders` (IdentityProviderList, @length 1..2, @uniqueItems, member enum IdentityProvider) plus `identityProviderConfiguration` (IdentityProviderConfiguration). The model states the rule on that shape's only member: IdentityProviderConfiguration.identityCenterConfiguration is documented "Identity Center configuration. Required when identityProviders includes IDC." The wording of the refusal is recorded behavior only: AWS answers HTTP 400 with code ValidationException and the message "identityCenterInstanceArn is required when authorizationProviders includes IDC" — the entire sentence is FIXED text, with NO variable part: no domainId, no provider list and no value from the request appears in it. The domain is left exactly as it was, so a following GetDomain still reports the previous `name` and identityProviders == ["IAM"].
  Emulator: UpdateDomain(domainId=<d-...>, identityProviders=["IDC"]) on an IAM-only domain with no Identity Center configuration succeeds with HTTP 200 (rpc-v2-cbor) and returns the patched domain with identityProviders == ["IDC"], identityProviderConfiguration == {} (empty — no identityCenterConfiguration at all), and a fabricated identityCenterApplicationArn of the form arn:aws:sso::<account>:application/ssoins-<id>/apl-<id> invented by the handler. No ValidationException is raised at any point. The handler validates only the enum membership of each provider value (services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/ops/update_domain.rs lines 147-162) and then unconditionally mints the application ARN from the provider list (lines 230-254); there is no check that an IDC-listing result carries an identityCenterConfiguration.
  Why: a client that relies on AWS refusing this request believes it has a domain federated through Identity Center when no instance backs it. The 200 persists an impossible state — a domain listing IDC with an empty identityProviderConfiguration and an Identity Center application ARN that names an instance that does not exist — and every later read (GetDomain/ListDomains) and every Identity-Center-dependent call (e.g. SearchPrincipals) then answers from that state instead of from the refusal, so the error surfaces far from the call that caused it, with a different code. Negative-path tests asserting the refusal pass against AWS and fail here.
  Fix: UpdateDomain's input validation in its handler — before any mutation, when the post-patch provider list includes IDC, require an identityCenterConfiguration (from the request's identityProviderConfiguration or the one already stored on the domain) and otherwise return ValidationException (400) with AWS's fixed message; the same precondition must gate the identityCenterApplicationArn minting so no ARN is fabricated for a domain with no instance. The refusal must be answered before the store is written, so the domain keeps its name and identityProviders.
  Traces: cloudwatchomni.domain-name-pattern-and-identity-center-rules-reject-updates (assertion at check line 82, UpdateDomain(identityProviders=["IDC"])). Handler: services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/ops/update_domain.rs lines 147-162 (only enum validation) and 230-254 (unconditional application-ARN minting).

## Evidence
Check cloudwatchomni.domain-name-pattern-and-identity-center-rules-reject-updates [mismatch]:

  code, message = _error(lambda: cwo.update_domain(domainId=domain_id, identityProviders=["IDC"]))
  AssertionError: expected the call to be rejected, but it succeeded: {'domain': {'domainId': 'd-7xejgluj1mtbwart1yrw00xtu', ..., 'identityProviders': ['IDC'], 'identityProviderConfiguration': {}, 'identityCenterApplicationArn': 'arn:aws:sso::000000000000:application/ssoins-2bf4bda05f014fdc/apl-d759cb5b51a842d6', 'status': 'ACTIVE'}, 'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/cbor', 'smithy-protocol': 'rpc-v2-cbor', ...}}}

Expected (the check's assertion): (code, message) == ("ValidationException", "identityCenterInstanceArn is required when authorizationProviders includes IDC"), i.e. HTTP 400, code ValidationException, and that sentence as wholly fixed text. The domain id, the generated application ARN, the account number, the host and the timestamps in the response above are per-run values and are not part of any requirement; the requirement is that there is no 200 response at all.

Got: HTTP 200 with the patch applied. The check's FIRST assertion (UpdateDomain with name="Sim2Real-Behav-Upper" refused 400 ValidationException "1 validation error detected: Value at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-z0-9]+(-[a-z0-9]+)*") now passes, which is how the replay reaches this second refusal.

Smithy cited: UpdateDomain input members domainId (DomainId, @pattern ^d-[0-9a-z]{1,25}$, required), name (String, @length 3..63, @pattern ^[a-z0-9]+(-[a-z0-9]+)*$), identityProviders (IdentityProviderList, @length 1..2, @uniqueItems, enum member IdentityProvider), identityProviderConfiguration (IdentityProviderConfiguration); errors include ValidationException (errorKind client, @httpError 400). IdentityProviderConfiguration.identityCenterConfiguration doc: "Identity Center configuration. Required when identityProviders includes IDC." The refusal's message text is not in the model — recorded behavior only.

Side effect confirming no validation runs: the returned domain carries identityProviders ['IDC'] together with an EMPTY identityProviderConfiguration and a non-null identityCenterApplicationArn — a combination the model's own documentation forbids.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Run the behavioral suite (simparity-behavioral-aws-v1), check cloudwatchomni.domain-name-pattern-and-identity-center-rules-reject-updates.
3. Manually: CreateDomain(name=<lowercase-hyphenated name>, identityProviders=["IAM"]) and note the returned domainId; the domain has no identityProviderConfiguration.
4. Call UpdateDomain(domainId=<that id>, identityProviders=["IDC"]) with NO identityProviderConfiguration.
5. Expected: HTTP 400, Error.Code "ValidationException", Error.Message exactly "identityCenterInstanceArn is required when authorizationProviders includes IDC" (no variable parts). Actual: HTTP 200 with the domain patched to identityProviders ['IDC'], identityProviderConfiguration {}, and a fabricated identityCenterApplicationArn.
6. Call GetDomain(domainId=<that id>): it must still report the original name and identityProviders == ["IAM"], because the refused update may not be applied. Actual: it reports ['IDC'].
