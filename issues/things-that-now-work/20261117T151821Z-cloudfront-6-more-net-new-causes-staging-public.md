Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.create-continuous-deployment-policy-disabled-staging-rejected cloudfront.create-public-key-rejects-private-key-material cloudfront.field-level-encryption-profile-overlapping-field-patterns-rejected cloudfront.connection-function-deny-and-error-distinguish-test-outputs cloudfront.continuous-deployment-policy-disabled-suppresses-header-routing cloudfront.managed-cache-policy-amplify-family-settings cloudfront.response-headers-policy-server-timing-edge-header
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.create-continuous-deployment-policy-disabled-staging-rejected|run#9|AssertionError|raise AssertionError(
Issue-Key: cloudfront.create-public-key-rejects-private-key-material|_error#6|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: cloudfront.field-level-encryption-profile-overlapping-field-patterns-rejected|_error#7|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded with {result!r}")
Issue-Key: cloudfront.connection-function-deny-and-error-distinguish-test-outputs|run#16|AssertionError|assert allow_output != deny_output, (
Issue-Key: cloudfront.continuous-deployment-policy-disabled-suppresses-header-routing|run#34|AssertionError|assert matching_enabled.content == b"staging-route", (
Issue-Key: cloudfront.managed-cache-policy-amplify-family-settings|run#6|AssertionError|assert not missing, f"Amplify-family managed cache policies missing from the catalog: {sorted(missing)}"
Issue-Key: cloudfront.response-headers-policy-server-timing-edge-header|_poll_server_timing#32|Timeout|status, headers = _get(url, verify)
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# cloudfront: 6 more net-new causes — staging/public-key/FLE validation missing, TestConnectionFunction collapses every outcome, Amplify managed cache policies absent, SingleHeader CD routing and Server-Timing never observed

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestConnectionFunction

## Description
Six further net-new root causes under the cloudfront service heading, each independent; all six must be closed before submitting. The open issue 20261110T105638Z-cloudfront-10-net-new-causes-in-the-now.md owns the single-header continuous-deployment ROUTING check it names, but not cause 5 below, whose own check is unowned.

Cause 1 of 6: CreateContinuousDeploymentPolicy accepts a staging distribution that is itself disabled.
AWS: Smithy: CreateContinuousDeploymentPolicy binds InvalidArgument (httpError 400) and StagingDistributionInUse (409); the config carries StagingDistributionDnsNames. Recorded refusal when the named staging distribution is Disabled: 400, Code `InvalidArgument`, Message "The parameter Staging distributions contains disabled distributions." — entirely fixed text — and no policy is created; the identical call once that distribution is enabled succeeds.
Emulator: answers 201 and mints the policy over the disabled staging distribution.
Why: a continuous-deployment policy exists that can never route traffic, and the caller is never told its staging distribution is unusable.
Fix: CreateContinuousDeploymentPolicy's validation — resolve each StagingDistributionDnsNames entry and check its Enabled state.
Traces: cloudfront.create-continuous-deployment-policy-disabled-staging-rejected.

Cause 2 of 6: CreatePublicKey accepts PEM-encoded RSA PRIVATE key material as EncodedKey.
AWS: Smithy: CreatePublicKey binds InvalidArgument (400); PublicKeyConfig.EncodedKey is required. Recorded refusal when EncodedKey is a '-----BEGIN PRIVATE KEY-----' PEM: 400, Code `InvalidArgument`, Message "Your request contains empty/invalid/out of limits RSA Encoded Key" — entirely fixed text — and ListPublicKeys shows nothing created; the matching public key PEM is accepted and round-trips through GetPublicKeyConfig.
Emulator: answers 201 and stores the private key, echoing it back from GetPublicKeyConfig.
Why: private key material is accepted and then served back out of the control plane — a client that pasted the wrong half of its key pair is told the upload succeeded and signed-URL verification will never work.
Fix: CreatePublicKey's EncodedKey validation (must parse as an RSA public key / SPKI PEM).
Traces: cloudfront.create-public-key-rejects-private-key-material.

Cause 3 of 6: CreateFieldLevelEncryptionProfile accepts overlapping field patterns.
AWS: Smithy: CreateFieldLevelEncryptionProfile binds InvalidArgument (400) plus the TooManyFieldLevelEncryption* errors. Recorded refusal for an encryption entity whose FieldPatterns contain a prefix pattern covering another ('ABC*' with 'AB*'): 400, Code `InvalidArgument`, Message "The parameter FieldPattern cannot start with a prefix field pattern already present in the encryption entity." — entirely fixed text — and ListFieldLevelEncryptionProfiles shows no profile; two disjoint prefixes create normally.
Emulator: answers 201 with FieldPatterns Items ['ABC*','AB*'] stored.
Why: a profile with ambiguous field patterns is persisted, so which key encrypts an overlapping field is undefined; AWS guarantees that configuration cannot exist.
Fix: CreateFieldLevelEncryptionProfile's validation of FieldPatterns within each encryption entity.
Traces: cloudfront.field-level-encryption-profile-overlapping-field-patterns-rejected.

Cause 4 of 6: TestConnectionFunction collapses allow, deny and an uncaught exception into the same empty result.
AWS: Smithy: ConnectionFunctionTestResult carries ConnectionFunctionOutput and ConnectionFunctionErrorMessage (both sensitive strings) plus execution logs. Recorded behavior: a function calling connection.allow() and one calling connection.deny() produce non-empty and DISTINCT ConnectionFunctionOutput with no ConnectionFunctionErrorMessage, and a function that throws produces a non-empty ConnectionFunctionErrorMessage containing the thrown error text.
Emulator: returns ConnectionFunctionOutput '{}' for both allow and deny, so the two are indistinguishable, and the throwing function reports no error message.
Why: TestConnectionFunction is the only way to validate a connection function before publishing it; with every outcome identical, a deny-by-default function cannot be distinguished from an allow one, and a syntactically broken function tests clean.
Fix: the TestConnectionFunction handler's runtime result serialization — render the connection object the function mutated and propagate the uncaught error text.
Traces: cloudfront.connection-function-deny-and-error-distinguish-test-outputs.

Cause 5 of 6: a matching SingleHeader continuous-deployment header does not reach the staging distribution.
AWS: recorded behavior only (the data-plane routing is not modeled): with an Enabled ContinuousDeploymentPolicy of TrafficConfig.Type SingleHeader attached to the primary distribution, a viewer request carrying the configured header/value is served by the STAGING distribution's origin, a non-matching value is served by the primary, and the routing follows the policy's Enabled flag (disabled → primary, re-enabled → staging again).
Emulator: the request carrying the matching header is answered with the primary origin's bytes while the policy is Enabled, so the staging path is never taken.
Why: continuous deployment is unusable — a client cannot shift a slice of traffic to a staging distribution, which is the feature's entire purpose, and the disabled/enabled transitions are untestable.
Fix: the edge request-routing path that evaluates a distribution's ContinuousDeploymentPolicyId (SingleHeader traffic config) before selecting the origin.
Traces: cloudfront.continuous-deployment-policy-disabled-suppresses-header-routing.

Cause 6 of 6: the Amplify-family managed cache policies are missing from the managed catalog.
AWS: recorded behavior only for the catalog contents: ListCachePolicies(Type='managed') returns the Managed-Amplify family — Managed-Amplify-Default, Managed-Amplify-Default-V2, Managed-Amplify-DefaultNoCookies, Managed-Amplify-DefaultNoCookies-V2, Managed-Amplify-ImageOptimization, Managed-Amplify-ImageOptimization-V2, Managed-Amplify-StaticContent, Managed-Amplify-StaticContent-V2 — each with its documented id and ParametersInCacheKeyAndForwardedToOrigin/TTL settings.
Emulator: the managed catalog omits all eight names.
Why: any client (Amplify itself, or a template) that attaches one of these policies by id gets a not-found refusal for a policy AWS always provides.
Fix: the managed cache-policy catalog seeded into CloudFront's state.
Traces: cloudfront.managed-cache-policy-amplify-family-settings.

Also observed and grounded in cloudfront.response-headers-policy-server-timing-edge-header: that check exhausts the suite's 60 s budget without reaching a verdict because the edge never answers with the policy's Server-Timing header; it is listed in trace_ids so a fixer re-replays it, and it is expected to pass once the Server-Timing injection path serves.

## Evidence
Behavioral suite simparity-behavioral-aws-v1; all checks below are listed under `### cloudfront`, neither ALREADY FILED nor OWNED.

Cause 1 — create-continuous-deployment-policy-disabled-staging-rejected: expected (InvalidArgument, 'The parameter Staging distributions contains disabled distributions.'); got HTTP 201 with ContinuousDeploymentPolicy {Enabled: True, TrafficConfig: SingleWeight}.
Cause 2 — create-public-key-rejects-private-key-material: expected (InvalidArgument, 'Your request contains empty/invalid/out of limits RSA Encoded Key'); got HTTP 201 with PublicKeyConfig.EncodedKey = '-----BEGIN PRIVATE KEY-----...'.
Cause 3 — field-level-encryption-profile-overlapping-field-patterns-rejected: expected (InvalidArgument, 'The parameter FieldPattern cannot start with a prefix field pattern already present in the encryption entity.'); got HTTP 201 with FieldPatterns {'Quantity': 2, 'Items': ['ABC*','AB*']}.
Cause 4 — connection-function-deny-and-error-distinguish-test-outputs: "connection.allow() and connection.deny() must produce distinct ConnectionFunctionOutput, both were '{}'".
Cause 5 — continuous-deployment-policy-disabled-suppresses-header-routing: "a matching header while the policy was Enabled did not reach staging: b'primary-route'".
Cause 6 — managed-cache-policy-amplify-family-settings: "Amplify-family managed cache policies missing from the catalog: ['Managed-Amplify-Default', 'Managed-Amplify-Default-V2', 'Managed-Amplify-DefaultNoCookies', 'Managed-Amplify-DefaultNoCookies-V2', 'Managed-Amplify-ImageOptimization', 'Managed-Amplify-ImageOptimization-V2', 'Managed-Amplify-StaticContent', 'Managed-Amplify-StaticContent-V2']".
Additional — response-headers-policy-server-timing-edge-header: "Error: Check timed out after 60.0s" (no edge answer carrying Server-Timing).

## Steps to Reproduce
1. git clone the repo and let the behavioral suite build and boot the emulator.
2. Replay the simparity-behavioral-aws-v1 suite.
3. Re-run the checks in trace_ids.
4. Cause 1: CreateContinuousDeploymentPolicy naming a Disabled staging distribution's domain — expect the InvalidArgument refusal, then enable it and expect 201. Cause 2: CreatePublicKey with a private-key PEM — expect InvalidArgument and nothing in ListPublicKeys. Cause 3: CreateFieldLevelEncryptionProfile with FieldPatterns ['ABC*','AB*'] — expect InvalidArgument. Cause 4: CreateConnectionFunction for allow()/deny()/throw and TestConnectionFunction each at Stage=DEVELOPMENT — expect distinct non-empty outputs and a populated ConnectionFunctionErrorMessage. Cause 5: attach an Enabled SingleHeader policy to a primary distribution whose staging copy serves different bytes, then GET with the configured header. Cause 6: ListCachePolicies(Type='managed') and look for the eight Managed-Amplify-* names.
