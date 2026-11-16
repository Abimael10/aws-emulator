Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: transfer.create-server-rejects-invalid-protocol-and-idp-config
Issue-Shape-Id: com.amazonaws.transfer#TransferService
Issue-Key: transfer.create-server-rejects-invalid-protocol-and-idp-config|run#15|AssertionError|assert msg_idp == f'IdentityProviderDetails required for IdentityProviderType: {idp}', (
Issue-Commit: 295e6cd53063b1cdd300d8666c04e601acfae48f

# Transfer CreateServer's missing-IdentityProviderDetails refusal carries emulator-authored prose instead of AWS's "IdentityProviderDetails required for IdentityProviderType: <type>"

> _Note: this issue was identified against the code base at commit `295e6cd53063b1cdd300d8666c04e601acfae48f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateServer

## Description
ONE net-new root cause in the transfer emulator's CreateServer validation path, found by replaying simparity-behavioral-aws-v1. There is ONE.

Related open issues that do NOT cover this cause (named so a fixer can see the overlap): '20261029T193150Z-transfer-refusal-fidelity-six-operations-answer-emulator-authored.md' owns the SAME check's earlier leg (its Cause 4: the FTPS leg answering the certificate rule before the identity-provider/protocol-family gate, replay key run#8). That leg now passes, so the check advances and fails on a LATER leg — the identity-provider detail requirement — which none of its six causes describes. The fixed issue '20261022T011353Z-transfer-input-validation-diverges-vpc-createserver-accepted-without.md' covers the EndpointType=VPC/EndpointDetails requirement (which this run shows passing) and the enum-message value injection, not this message; '20261109T050426Z-transfer-reopens-createserver-still-refuses-every-api-gateway-identity.md' covers an API_GATEWAY provider WITH details being wrongly refused, a different branch from the details-absent refusal here.

Cause 1 of 1 — the missing-IdentityProviderDetails refusal does not reproduce AWS's message, so the three detail-requiring identity providers are not reported the way the service reports them.
AWS: Smithy (services/service-com-amazonaws-transfer-transferservice/smithy-model/com-amazonaws-transfer-transferservice.json): CreateServer.IdentityProviderType -> enum IdentityProviderType (API_GATEWAY | AWS_DIRECTORY_SERVICE | AWS_LAMBDA | SERVICE_MANAGED) and CreateServer.IdentityProviderDetails -> IdentityProviderDetails are both optional in the model, so the requirement is service-side; the member is documented "Required when IdentityProviderType is set to AWS_DIRECTORY_SERVICE, Amazon Web Services_LAMBDA or API_GATEWAY". CreateServer models InvalidRequestException (errorKind client, httpError 400, Message required). Recorded behavior: CreateServer with Protocols=['SFTP'], EndpointType=PUBLIC and one of the three detail-requiring IdentityProviderTypes and NO IdentityProviderDetails is refused with HTTP 400, code InvalidRequestException and Message 'IdentityProviderDetails required for IdentityProviderType: <IdentityProviderType>'. Fixed text: 'IdentityProviderDetails required for IdentityProviderType: '. Variable part: the IdentityProviderType value taken from the request — so API_GATEWAY, AWS_LAMBDA and AWS_DIRECTORY_SERVICE each yield a DISTINCT message naming that provider. (The 'API_GATEWAY' in the evidence below is the request's value filling that slot, not part of the template.)
Emulator: the right status (400) and the right code (InvalidRequestException), but Message 'IdentityProviderDetails must be specified when IdentityProviderType is API_GATEWAY' — a rebuilt sentence AWS never emits. The replay stopped at the first provider, so only the API_GATEWAY rendering is observed; the refusal is produced by one shared cross-member check (create_server.rs line 364 calls crate::state::ServerConfig::cross_member_violation, which returns the message), so the same non-AWS template is expected for AWS_LAMBDA and AWS_DIRECTORY_SERVICE.
Why: all of CreateServer's semantic refusals share code InvalidRequestException, so only the message distinguishes 'you forgot the identity-provider details' from 'the protocol family is unsupported' or 'the VPC endpoint settings are missing'. A client, IaC provider or test that exact-matches the service's refusal text to decide which member to repair does not recognise the string, and operator tooling keyed on AWS's wording sees prose no AWS runbook or document matches.
Fix: CreateServer's input validation — the identity-provider cross-member branch (crate::state::ServerConfig::cross_member_violation, reached from the handler's cross_member_violation call) must emit AWS's fixed text with the request's IdentityProviderType interpolated, keeping the three providers' messages distinct.
Traces: check transfer.create-server-rejects-invalid-protocol-and-idp-config. Its remaining legs — the AWS_LAMBDA and AWS_DIRECTORY_SERVICE refusals, the three-distinct-diagnostics assertion, the no-leaked-server assertion and the ListServers/DescribeSecurityPolicy liveness controls — were not reached in this run and are not asserted to pass. Source citations: services/service-com-amazonaws-transfer-transferservice/src/ops/create_server.rs:350-369.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 111/131 passed; transfer 92/109. Expected = real AWS behavior as the check records it, Actual = emulator.

transfer.create-server-rejects-invalid-protocol-and-idp-config [mismatch]:
  AssertionError: the missing-details refusal for API_GATEWAY must name API_GATEWAY, got 'IdentityProviderDetails must be specified when IdentityProviderType is API_GATEWAY'
  expected Message: 'IdentityProviderDetails required for IdentityProviderType: API_GATEWAY'
  actual   Message: 'IdentityProviderDetails must be specified when IdentityProviderType is API_GATEWAY'
  HTTP status (400) and error code (InvalidRequestException) both matched; only the Message's fixed text diverges. The 'API_GATEWAY' token is the request's IdentityProviderType filling the template's one variable slot.

Earlier legs of the same check passed in this run, so they are NOT part of this cause: the FTP and FTPS family refusal under SERVICE_MANAGED ('FTP and FTPS protocols unsupported for IdentityProviderType SERVICE_MANAGED', both legs identical) and the EndpointType=VPC refusal ('EndpointDetails required for VPC').

Model grounding: CreateServer declares InvalidRequestException (client, httpError 400, Message required); IdentityProviderType is an enum with members API_GATEWAY, AWS_DIRECTORY_SERVICE, AWS_LAMBDA, SERVICE_MANAGED; IdentityProviderDetails is optional, so the requirement is enforced service-side.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout.
2. Replay the simparity-behavioral-aws-v1 behavioral suite.
3. Re-replay transfer.create-server-rejects-invalid-protocol-and-idp-config. Its failing leg is: for each of IdentityProviderType in ('API_GATEWAY', 'AWS_LAMBDA', 'AWS_DIRECTORY_SERVICE'), call CreateServer with Protocols=['SFTP'], EndpointType='PUBLIC', that IdentityProviderType and NO IdentityProviderDetails.
4. For each call compare the HTTP status (400), the error code (InvalidRequestException) and the full Message against 'IdentityProviderDetails required for IdentityProviderType: <the IdentityProviderType sent>', and confirm the three providers produce three DISTINCT messages.
5. Confirm the check's later assertions also hold once the message is fixed: the three CreateServer defect messages (FTP family, VPC endpoint details, identity-provider details) are mutually distinct, no server is left behind by any refused call, and ListServers / DescribeSecurityPolicy(TransferSecurityPolicy-2024-01) still answer.
