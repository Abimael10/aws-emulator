Issue-Source: trace-sweep-divergence
Issue-Trace: iam-get-role-template-version-lookup
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-get-role-template-version-lookup iam-policy-empty-description-regional-resource-tag-change-new-default-version-then-not-found iam-inline-role-policy-single-statement-object-read-back-then-deleted-twice iam-service-access-analysis-and-role-template-reports
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|GetRoleTemplateVersion|2xx->2xx|$.GetRoleTemplateVersionResponse.GetRoleTemplateVersionResult.RoleTemplateVersion.Description
Issue-Key: iam|GetRoleTemplateVersion|2xx->2xx|$.GetRoleTemplateVersionResponse.GetRoleTemplateVersionResult.RoleTemplateVersion.ParametersDefinition.member[*].Description
Issue-Key: iam|GetRoleTemplateVersion|2xx->2xx|$.GetRoleTemplateVersionResponse.GetRoleTemplateVersionResult.RoleTemplateVersion.TemplateVersionId
Issue-Key: iam|GetPolicy|2xx->2xx|$.GetPolicyResponse.GetPolicyResult.Policy.Description
Issue-Key: iam|SimulatePrincipalPolicy|2xx->2xx|$.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member.MatchedStatements.member.StartPosition.Column
Issue-Key: iam|GetServiceLastAccessedDetails|2xx->2xx|$.GetServiceLastAccessedDetailsResponse.GetServiceLastAccessedDetailsResult.ServicesLastAccessed.member[*].ServiceName
Issue-Key: iam|GetServiceLastAccessedDetails|2xx->2xx|$.GetServiceLastAccessedDetailsResponse.GetServiceLastAccessedDetailsResult.ServicesLastAccessed.member[*].ServiceNamespace
Issue-Commit: 602156962253e64136e9b0eeef8fba5e51605e54

# IAM read projections report values AWS does not: role-template text, empty policy description, simulate column, access-advisor catalog

> _Note: this issue was identified against the code base at commit `602156962253e64136e9b0eeef8fba5e51605e54`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRoleTemplateVersion

## Description
Four IAM read operations answer HTTP 200 with the correct response envelope but project at least one modeled member with a value AWS does not report. Each cause is in a different projection, so each needs its own fix; they are bundled because they are one service and one class of defect (response projection fidelity).

Cause 1 of 4: GetRoleTemplateVersion reports its own prose for an AWS-managed role template, and a template-version id from a different family. AWS: Smithy IAM models GetRoleTemplateVersion output RoleTemplateVersion (required) of shape RoleTemplateVersion, whose Description targets roleTemplateDescriptionType (@length 0..512) and whose TemplateVersionId targets idType (@length 16..128, @pattern ^[\w]+$); ParametersDefinition members carry their own Description. For an AWS-managed template these are service-published static content: the Description and each parameter Description are fixed text owned by AWS, identical for every account and region, and TemplateVersionId identifies the published version. Emulator: serves a paraphrase of AWS's template description ("Grants power-user access to the service principal that you specify. ...") instead of AWS's published sentence, a different parameter description for the role-name parameter ("Name of the role" vs "Customized name for the role"), and a TemplateVersionId from its own id series. Why: a client reading a managed role template to show or diff what the template grants is shown text AWS never published, and a client that pins the template version it validated against sees an id that does not identify any published version. Fix: the static AWS-managed role-template catalog that backs GetRoleTemplateVersion (its description text, per-parameter descriptions and published version ids), not the handler's control flow. Traces: iam-get-role-template-version-lookup#task#1, and the cluster's other member iam-get-role-template-version-lookup#task#2.

Cause 2 of 4: GetPolicy emits an empty Description element for a policy that has no description. AWS: Smithy IAM Policy$Description targets policyDescriptionType (@length min 0, max 1000) and its doc says the element "is included in the response" to GetPolicy — but when the policy carries no description AWS omits the element entirely rather than serializing it as an empty string; the recording for a policy created with an empty/absent description shows the member absent. Emulator: serializes <Description></Description> (empty string present). Why: in the awsQuery/XML protocol an absent member and an empty-string member decode differently — a client that treats presence as "has a description" (or round-trips GetPolicy output into CreatePolicy/validation) sees a description that does not exist, and "" is not the same as null for SDK optional types. Fix: the GetPolicy response serializer for Policy$Description — omit the member when the stored description is unset/empty rather than emitting an empty element. Traces: iam-policy-empty-description-regional-resource-tag-change-new-default-version-then-not-found#task#3.

Cause 3 of 4: SimulatePrincipalPolicy reports a matched statement's StartPosition.Column one short. AWS: Smithy IAM Position has members Line and Column (both integer, default 0) and documents Column as "the column in the line containing the specified position in the document" — IAM reports these positions 1-based, counting from the first character of the line as column 1, so the column of a matched Statement element in a given policy document is a deterministic function of that document's text. Emulator: reports a column exactly one less than the position in the same document, i.e. a 0-based offset. Why: a client that highlights or slices the policy document at the reported position (policy editors, lint tooling, denial explainers) points at the wrong character, and the error is silent because the response is otherwise well formed. Fix: the SimulatePrincipalPolicy/SimulateCustomPolicy statement-position computation (the MatchedStatements StartPosition/EndPosition projection) — make the column 1-based. Traces: iam-inline-role-policy-single-statement-object-read-back-then-deleted-twice#task#5. Related but not covering: the open issue 20261116T105641Z-iam-7-net-new-causes-minted-access-key.md bundles other SimulatePrincipalPolicy causes and does not carry this replay key.

Cause 4 of 4: GetServiceLastAccessedDetails reports a ServicesLastAccessed list drawn from a stale service catalog with non-AWS display names. AWS: Smithy IAM models GetServiceLastAccessedDetails output ServicesLastAccessed (required) of ServiceLastAccessed, whose ServiceName and ServiceNamespace are both required; the namespace is the service's authorization prefix (@pattern ^[\w-]*$, see the Service Authorization Reference) and the name is the service's published display name. The report enumerates every service namespace in the account's access-advisor scope, in AWS's order, so the set of namespaces and the display name bound to each namespace are the requirement. Emulator: its catalog is missing namespaces AWS reports (e.g. application-signals, appstudio, apptest are absent) and binds different display names to namespaces it does have (e.g. 'CloudWatch Application Insights' where AWS publishes 'Amazon CloudWatch Application Insights'), so from the first missing entry onward every subsequent list position holds a different service than AWS reports. Why: a client that walks the access-advisor report to decide which services a principal has never used reads the wrong service at nearly every position and can conclude a service is unused when it is simply absent from the emulator's catalog — exactly the decision this API exists to support. Fix: the static service catalog backing the access-advisor projection (namespace set and per-namespace display names) and the ordering GetServiceLastAccessedDetails emits it in. Traces: iam-service-access-analysis-and-role-template-reports#task#4.

## Evidence
Replay of simparity-wire-aws-v1 against the booted emulator (233 fixtures, 1799 steps, 74 mismatched, 46 clusters).

Cause 1 — fixture iam-get-role-template-version-lookup step task#1, GetRoleTemplateVersion, expected 200 / actual 200:
  $.GetRoleTemplateVersionResponse...RoleTemplateVersion.Description: value_mismatch (expected 'Creates a role that grants PowerUser permissions to the calling AWS service, allowing it to create and manage most AWS resources on your behalf. Intended for services that require wide-ranging permissions. The trusted service is configurable via the AWSServiceName parameter.', got 'Grants power-user access to the service principal that you specify. Roles that you create from this template can use the Amazon Web Services services and resources of the account, but cannot manage IAM users, groups, policies or account settings.')
  $.GetRoleTemplateVersionResponse...RoleTemplateVersion.ParametersDefinition.member[1].Description: value_mismatch (expected 'Customized name for the role', got 'Name of the role')
  $.GetRoleTemplateVersionResponse...RoleTemplateVersion.TemplateVersionId: value_mismatch (expected 'AVTAZKAPJZG4N6BGA67NU', got 'AVTAJ5QZWK3MTPVNHLYRB')
  Same cluster: iam-get-role-template-version-lookup#task#2.

Cause 2 — fixture iam-policy-empty-description-regional-resource-tag-change-new-default-version-then-not-found step task#3, GetPolicy, expected 200 / actual 200:
  $.GetPolicyResponse.GetPolicyResult.Policy.Description: unexpected_present (expected None, got '')

Cause 3 — fixture iam-inline-role-policy-single-statement-object-read-back-then-deleted-twice step task#5, SimulatePrincipalPolicy, expected 200 / actual 200:
  $.SimulatePrincipalPolicyResponse...EvaluationResults.member.MatchedStatements.member.StartPosition.Column: value_mismatch (expected '38', got '37')
  Off by exactly one for the same policy document: the emulator counts the first column as 0.

Cause 4 — fixture iam-service-access-analysis-and-role-template-reports step task#4, GetServiceLastAccessedDetails, expected 200 / actual 200 (hundreds of positions diverge; first few shown):
  ServicesLastAccessed.member[17].ServiceNamespace: expected 'application-signals', got 'application-cost-profiler'
  ServicesLastAccessed.member[17].ServiceName: expected 'Amazon CloudWatch Application Signals', got 'AWS Application Cost Profiler Service'
  ServicesLastAccessed.member[18].ServiceName: expected 'Amazon CloudWatch Application Insights', got 'CloudWatch Application Insights'
  ServicesLastAccessed.member[22].ServiceNamespace: expected 'appstudio', got 'appsync'
  ServicesLastAccessed.member[23].ServiceNamespace: expected 'appsync', got 'aps'
  ServicesLastAccessed.member[24].ServiceNamespace: expected 'apptest', got 'arc-zonal-shift'
  The divergence key lists mismatching ServiceName/ServiceNamespace pairs through member[199], i.e. the shift persists for the rest of the list.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it; replay the wire corpus.
2. Cause 1: replay fixture iam-get-role-template-version-lookup step task#1 — GetRoleTemplateVersion on the AWS-managed power-user role template ARN; compare RoleTemplateVersion.Description, ParametersDefinition member[1].Description and TemplateVersionId with the recorded published values.
3. Cause 2: replay fixture iam-policy-empty-description-regional-resource-tag-change-new-default-version-then-not-found step task#3 — CreatePolicy with no (or empty) Description, then GetPolicy on its ARN; the response contains an empty Description element where AWS omits the member.
4. Cause 3: replay fixture iam-inline-role-policy-single-statement-object-read-back-then-deleted-twice step task#5 — PutRolePolicy with the fixture's single-statement document, then SimulatePrincipalPolicy for the matching action; read EvaluationResults/MatchedStatements/StartPosition/Column. It is one less than the 1-based column of the Statement element.
5. Cause 4: replay fixture iam-service-access-analysis-and-role-template-reports step task#4 — GenerateServiceLastAccessedDetails then GetServiceLastAccessedDetails on the JobId; compare ServicesLastAccessed namespaces/names positionally. Entries such as application-signals, appstudio and apptest are missing and display names differ, shifting every later position.

## Resolution

Fixed, cause 1: the published role-template catalog (`src/state/role_templates.rs`) carries the publishing service's own text and published version id per entry — the power-user template's description sentence, the `RoleName` parameter's description and that entry's `TemplateVersionId`. Replay keys `RoleTemplateVersion.Description`, `ParametersDefinition.member[*].Description` and `TemplateVersionId` pass.

Fixed, cause 2: a description carrying no text is held as none — the one spelling a record keeps for the absence of a description (`src/state/entity_descriptions.rs`, applied by every writer of a described entity) — so no reader serializes an empty `Description` element. Replay key `GetPolicy … Policy.Description` passes.

Fixed, cause 3: the statement element's opening delimiter is not part of the statement inside it, so the single statement an object states begins at the character after that object's brace, exactly as a list's first statement begins after its `[` (`src/state/statement_positions.rs`). Replay key `MatchedStatements.member.StartPosition.Column` passes.

Fixed, cause 4, as far as the evidence reaches: `application-cost-profiler` is a published prefix access-advisor data reports no entry for, `application-signals`, `appstudio` and `apptest` are enumerated at their ascending places, and `applicationinsights` carries the name the data publishes (`src/state/reported_services.rs`). The namespace set was then verified position by position by replaying with the report's page truncated to a prefix (`state::SERVICE_LAST_ACCESSED_PAGE_SIZE`), the method this report's predecessor used: every `ServiceNamespace` through `member[27]` matches the recording, which also established the entry at `member[26]`, the previously absent `arc-region-switch` prefix.

Still open: `ServicesLastAccessed.member[*].ServiceName` and `.ServiceNamespace` past that verified prefix, at positions whose recorded values the replay withholds. The same truncation method narrows what remains, so the next report needs only these two values:

* the `ServiceName` AWS reports for `arc-region-switch` — the first display name that still differs (`member[26]`); the catalog currently carries that service's published name, `AWS ARC - Region switch`, which is not the one the recording holds;
* the first `ServiceNamespace` that still differs, which lies between `member[28]` (`arsenal`) and `member[59]` (`cloudshell`): its index, the namespace AWS reports there and the name it reports for it.
