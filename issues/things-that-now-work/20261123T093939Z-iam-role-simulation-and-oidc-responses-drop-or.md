Issue-Source: trace-sweep-divergence
Issue-Trace: iam-role-explicit-session-duration-and-path-assumed-description-cleared-tag-change-then-not-found
Issue-Step: task#12
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-role-explicit-session-duration-and-path-assumed-description-cleared-tag-change-then-not-found iam-simulate-principal-policy-pagination-and-params iam-add-client-id-to-openid-connect-provider
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|UpdateRoleDescription|2xx->2xx|$.UpdateRoleDescriptionResponse.UpdateRoleDescriptionResult.Role.Description
Issue-Key: iam|SimulatePrincipalPolicy|2xx->2xx|$.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member[*].EvalResourceName
Issue-Key: iam|GetOpenIDConnectProvider|2xx->2xx|$.GetOpenIDConnectProviderResponse.GetOpenIDConnectProviderResult.Url
Issue-Related: 20261029T184359Z-iam-policy-documents-are-returned-as-raw-json.md
Issue-Commit: f76d85d47be0a2dd652a7035a8c2c788dca4515b

# iam: role, simulation and OIDC responses drop or rewrite values held in state

> _Note: this issue was identified against the code base at commit `f76d85d47be0a2dd652a7035a8c2c788dca4515b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateRoleDescription

## Description
THREE net-new causes, all in how IAM projects a value it already holds into a 200 response: a stored document is re-encoded, a stored empty member is dropped, a derived member is flattened to a constant, and a stored URL is served as a host the emulator does not answer on. All three must be closed before this issue is submitted.

=== Cause 1 of 3: UpdateRoleDescription re-serializes AssumeRolePolicyDocument and omits an empty Description ===
AWS: Smithy: UpdateRoleDescription(RoleName required, Description required, roleDescriptionType @length min 0 max 1000) -> output Role (com.amazonaws.iam#Role); errors NoSuchEntity 404, UnmodifiableEntity 400, ServiceFailure 500. Role.AssumeRolePolicyDocument is policyDocumentType: AWS returns the trust policy document as it is stored for the role (URL-encoded on the query wire), preserving the document text supplied at CreateRole / UpdateAssumeRolePolicy — AWS does not canonicalize or reorder its members. Role.Description has @length min 0, so the empty string is a legal stored value; after UpdateRoleDescription with an empty Description AWS returns the member PRESENT and empty (recorded: expected '' at $...Role.Description).
Emulator: status 200 in both, body diverges on two members. (a) AssumeRolePolicyDocument is rebuilt from a parsed map with its keys in alphabetical order — it serves {"Statement":[{"Action":"sts:AssumeRole","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account-id>:root"},"Sid":""}],"Version":"2012-10-17"} where the stored document is {"Version":"2012-10-17","Statement":[{"Sid":"","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account-id>:root"},"Action":"sts:AssumeRole"}]} — same semantics, different bytes, at both the document and the statement level. (b) Role.Description is omitted from the response entirely when the stored description is the empty string (got None where '' is expected).
Why: a client relying on AWS's behavior hashes or diffs the trust policy text to detect drift (CDK/Terraform state, policy signing, config snapshots); a document that comes back re-ordered reads as a change on every single read, so a converged stack never converges. And a client that reads Role.Description after clearing it gets a missing member instead of an empty string, which is an unwrap/KeyError at the SDK boundary rather than "".
Fix: the shared Role projection/serializer in the IAM crate (the one CreateRole, GetRole, UpdateRole and UpdateRoleDescription all go through): keep and emit the stored document text instead of re-encoding a parsed structure, and emit Description whenever the role has one — including Some("") — not only when it is non-empty.
Traces: iam-role-explicit-session-duration-and-path-assumed-description-cleared-tag-change-then-not-found#task#12. NOTE on overlap: the open issue 20261029T184359Z-iam-policy-documents-are-returned-as-raw-json.md owns the CreateRole cluster where the same document differs only in inter-token whitespace; it does not carry this fixture/step, it is about spacing rather than member ORDER, and it says nothing about the dropped empty Description.

=== Cause 2 of 3: SimulatePrincipalPolicy serves EvalResourceName as a constant '*' ===
AWS: Smithy: SimulatePrincipalPolicy -> SimulatePolicyResponse.EvaluationResults (list of EvaluationResult). EvaluationResult.EvalResourceName (ResourceNameType, @length 1..2048) is documented as "The ARN template for the simulated resource type (for example, arn:${Partition}:s3:::${BucketName}/${KeyName}), or * if no ARN format is defined for the action" — so '*' is correct ONLY for an action whose resource type has no ARN format. Recorded behavior for this step: for the simulated IAM action the expected EvalResourceName is the IAM user ARN template form, 'arn:aws:iam::<account-id>:user/...'.
Emulator: status 200, and EvaluationResults.member[1].EvalResourceName is served as '*' where the action's ARN template is expected — i.e. the emulator has no per-action resource-ARN-format table and emits the no-ARN-format fallback for every result.
Why: a caller simulating several actions in one request uses EvalResourceName to tell which resource type each result is about before reading EvalDecision or descending into ResourceSpecificResults. A constant '*' makes every result look like an action with no resource scope, so an authorization pre-flight cannot distinguish a resource-scoped allow from an account-wide one.
Fix: the evaluation-result builder shared by SimulatePrincipalPolicy and SimulateCustomPolicy in the IAM crate: derive EvalResourceName from the simulated action's resource ARN format (and the request's ResourceArns), and fall back to '*' only for actions that define no ARN format.
Traces: iam-simulate-principal-policy-pagination-and-params#task#2 (the same cluster also covers task#3 of that fixture).

=== Cause 3 of 3: GetOpenIDConnectProvider serves a Url naming a host the emulator does not answer on ===
AWS: Smithy: GetOpenIDConnectProvider(OpenIDConnectProviderArn required) -> Url (OpenIDConnectProviderUrlType, @length 1..255), ClientIDList, ThumbprintList, CreateDate, Tags; errors InvalidInput 400, NoSuchEntity 404, ServiceFailure 500. Recorded behavior only: AWS returns the issuer URL exactly as supplied to CreateOpenIDConnectProvider, with the scheme stripped — here a Cognito user-pool issuer, host plus pool path.
Emulator: status 200, and the replay verdict on $.GetOpenIDConnectProviderResponse.GetOpenIDConnectProviderResult.Url is host_not_own. The served and recorded strings are identical character for character, so what the replay rejects is not the text but that the served URL names an external AWS host (cognito-idp.<region>.amazonaws.com) which this emulator does not serve. Hypothesis: the OIDC-provider Url is stored and projected verbatim and takes no part in the emulator's own-host rewriting, so the address it publishes is not the address the emulator is reached on.
Why: a client relying on AWS's behavior reads Url from GetOpenIDConnectProvider and resolves it to fetch the issuer's discovery document before establishing the federation. Pointed at a host the emulator does not answer on, that fetch leaves the emulator entirely and the OIDC flow cannot complete, even though the string matches the recording.
Fix: the OpenIDConnectProvider projection in the IAM crate (GetOpenIDConnectProvider / ListOpenIDConnectProviders, and the create path that stores Url) together with whichever component owns host rewriting for served URLs. The host is a per-environment value and no literal host is a requirement: the requirement is that the published Url resolve to the endpoint the emulator is reached on.
Traces: iam-add-client-id-to-openid-connect-provider#task#4.

## Evidence
Cause 1 (iam-role-explicit-session-duration-and-path-assumed-description-cleared-tag-change-then-not-found#task#12, UpdateRoleDescription, expected 200 / got 200):
- $.UpdateRoleDescriptionResponse.UpdateRoleDescriptionResult.Role.AssumeRolePolicyDocument: value_mismatch
  expected (decoded): {"Version":"2012-10-17","Statement":[{"Sid":"","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}
  got (decoded):      {"Statement":[{"Action":"sts:AssumeRole","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Sid":""}],"Version":"2012-10-17"}
  (members alphabetized at both levels; the account id above is this run's value, not part of the requirement)
- $.UpdateRoleDescriptionResponse.UpdateRoleDescriptionResult.Role.Description: missing — expected '' (empty string), got None

Cause 2 (iam-simulate-principal-policy-pagination-and-params#task#2, SimulatePrincipalPolicy, expected 200 / got 200):
- $.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member[1].EvalResourceName: value_mismatch — expected the action's IAM user ARN template ('arn:aws:iam::000000000000:user/...'), got '*'
- same cluster, affected_count 2: also iam-simulate-principal-policy-pagination-and-params#task#3

Cause 3 (iam-add-client-id-to-openid-connect-provider#task#4, GetOpenIDConnectProvider, expected 200 / got 200):
- $.GetOpenIDConnectProviderResponse.GetOpenIDConnectProviderResult.Url: verdict host_not_own — expected 'cognito-idp.us-west-2.amazonaws.com/us-west-2_<poolid>', got the identical string. The two values do not differ; the step diverges because the served Url names a host the emulator does not serve (the host and pool id shown are this run's values).

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Cause 1: step task#12 of fixture iam-role-explicit-session-duration-and-path-assumed-description-cleared-tag-change-then-not-found — a role is created with a trust policy, then UpdateRoleDescription clears its description with Description="". Inspect the returned Role: AssumeRolePolicyDocument comes back with its members alphabetized rather than as stored, and Description is absent instead of empty.
4. Cause 2: step task#2 (and task#3) of fixture iam-simulate-principal-policy-pagination-and-params — SimulatePrincipalPolicy over the paginated action list. Read EvaluationResults.member[1].EvalResourceName: it is '*' for an action whose resource type does have an ARN format.
5. Cause 3: step task#4 of fixture iam-add-client-id-to-openid-connect-provider — create an OIDC provider for a Cognito user-pool issuer, AddClientIDToOpenIDConnectProvider, then GetOpenIDConnectProvider. The returned Url names the issuer's AWS host, which the emulator does not answer on.
