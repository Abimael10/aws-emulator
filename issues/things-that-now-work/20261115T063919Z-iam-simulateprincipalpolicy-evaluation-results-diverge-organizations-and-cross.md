Issue-Source: trace-sweep-divergence
Issue-Trace: iam-agentcore-traj-team-role-deny-is-read-only
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-agentcore-traj-team-role-deny-is-read-only iam-bap-simulate-with-bucket-policy iam-ebecs-events-role-run-task-pass-role-simulation
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|SimulatePrincipalPolicy|2xx->2xx|$.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member[*].MatchedStatements.member.StartPosition.Column
Issue-Key: iam|SimulatePrincipalPolicy|2xx->2xx|$.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member[*].ResourceSpecificResults.member.MatchedStatements.member.StartPosition.Column
Issue-Key: iam|SimulatePrincipalPolicy|2xx->2xx|$.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member.MatchedStatements.member.StartPosition.Column
Issue-Key: iam|SimulatePrincipalPolicy|2xx->2xx|$.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member.ResourceSpecificResults.member.MatchedStatements.member.StartPosition.Column
Issue-Key: iam|SimulatePrincipalPolicy|2xx->2xx|$.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member[*].ResourceSpecificResults.member[*].MatchedStatements.member.StartPosition.Column
Issue-Related: 20261029T184359Z-iam-modeled-empty-members-are-omitted-from-iam-success.md
Issue-Related: 20261103T132715Z-iam-simulateprincipalpolicy-evaluation-results-omit-modeled-detail-members.md
Issue-Commit: 051d1ca879f4768e0cde48b6e9d78ea8677353f9

# iam SimulatePrincipalPolicy evaluation results diverge: Organizations and cross-account decision details omitted, statement positions and per-resource decisions wrong (4 causes)

> _Note: this issue was identified against the code base at commit `051d1ca879f4768e0cde48b6e9d78ea8677353f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SimulatePrincipalPolicy

## Description
SimulatePrincipalPolicy answers HTTP 200 with an EvaluationResult projection that is structurally and semantically different from AWS's. Four independent causes, each grounded in its own replayed cluster. All four are net-new: the only open SimulatePrincipalPolicy issue, 20261115T003554Z-cloudformation-ec2-ecr-iam-lambda-logs-s-iam-simulateprincipalpolicy-answers-501-notimplemented-for-an-aws.md, is about the operation answering 501 for an AWS-managed policy missing from the catalog, not about the content of a 200 result.

Cause 1 of 4: OrganizationsDecisionDetail and EvalDecisionDetails are never emitted.
AWS: Smithy iam#EvaluationResult declares OrganizationsDecisionDetail (-> iam#OrganizationsDecisionDetail{AllowedByOrganizations: boolean}) and EvalDecisionDetails (-> EvalDecisionDetailsType, a map). The model documents OrganizationsDecisionDetail as present whenever the simulated principal's account is part of an organization, and documents EvalDecisionDetails as "present but the response is empty" when the simulation evaluates policies within the same account and includes a resource ARN. The recordings show exactly that: every EvaluationResult carries OrganizationsDecisionDetail with AllowedByOrganizations true and an EvalDecisionDetails element with empty content.
Emulator: both members are absent from every EvaluationResult in the result list ($...EvaluationResults.member[*].OrganizationsDecisionDetail: missing (expected {'AllowedByOrganizations': 'true'}, got None); $...EvaluationResults.member[*].EvalDecisionDetails: missing (expected '', got None)).
Why: a client that reads result.organizations_decision_detail().allowed_by_organizations() to decide whether an SCP is the thing blocking a call gets None instead of a decision, and a client that distinguishes same-account from cross-account simulations by the presence of EvalDecisionDetails can never see the same-account case. Both members are modelled as optional, so no SDK error surfaces - the answer is simply silently incomplete.
Fix: the SimulatePrincipalPolicy result builder / awsQuery result serializer in the iam service crate must populate both members for every EvaluationResult it returns.
Traces: iam-agentcore-traj-team-role-deny-is-read-only#task#9 (both members missing on member[0] and member[1]).

Cause 2 of 4: MatchedStatements StartPosition.Column is computed wrongly.
AWS: Smithy iam#Statement.StartPosition -> iam#Position{Line, Column}, documented as "the row and column of the beginning of the Statement in an IAM policy", i.e. a position in the policy document as stored. The recordings show the column of the matched statement in the role's own policy document.
Emulator: the column is consistently off by a small amount and the offset is not constant across documents - expected 212 got 214, expected 287 got 288, expected 323 got 325 - and the same wrong value is repeated inside ResourceSpecificResults.member[*].MatchedStatements.member.StartPosition.Column. Hypothesis: the position is measured against a re-serialised or escaped copy of the policy document (e.g. after percent-/JSON-escaping) rather than against the document the policy was stored with, so each added escape shifts the column.
Why: tooling that uses StartPosition/EndPosition to highlight the statement that produced a decision points at the wrong character offset, and any client that asserts the position for a known policy document disagrees with AWS.
Fix: the statement-position computation in the policy-simulation engine must index the stored policy document text, and the same value must be reused for the top-level and per-resource MatchedStatements.
Traces: iam-bap-simulate-with-bucket-policy#task#3; iam-ebecs-events-role-run-task-pass-role-simulation#task#10; iam-agentcore-traj-team-role-deny-is-read-only#task#9.

Cause 3 of 4: MissingContextValues is reported at the top level for a simulation that supplied ResourceArns.
AWS: Smithy iam#EvaluationResult.MissingContextValues documents that the top-level list is used when the simulated resource is "*" (explicitly, or because ResourceArns was blank), and that "if you include a list of resources, then any missing context values are instead included under the ResourceSpecificResults section". The recording for a simulation that passes an explicit resource ARN carries no top-level missing-context entry.
Emulator: it emits a top-level MissingContextValues containing aws:SecureTransport ($...EvaluationResults.member.MissingContextValues: type_mismatch (expected '', got {'member': 'aws:SecureTransport'})).
Why: a caller that treats a non-empty top-level MissingContextValues as "this simulation is inconclusive, supply more context" is told to retry a simulation that AWS already answered conclusively, and the per-resource placement the SDK documents is not where the value appears.
Fix: the simulation engine must route missing context keys to ResourceSpecificResults when the request carried ResourceArns, and leave the top-level list empty.
Traces: iam-bap-simulate-with-bucket-policy#task#3.

Cause 4 of 4: EvalResourceName is the concrete request ARN, and the aggregate/per-resource decisions are mis-assigned.
AWS: Smithy iam#EvaluationResult.EvalResourceName documents the value as "the ARN template for the simulated resource type (for example, arn:${Partition}:s3:::${BucketName}/${KeyName}), or * if no ARN format is defined for the action. This is not a specific customer-provided resource ARN" - the recordings show the templates arn:aws:ecs:${Region}:${Account}:${ResourceType}/${ResourcePath} and arn:aws:iam::${Account}:${ResourceType}/${ResourceName}. The same shape's documentation states the top-level EvalDecision is the aggregate (most restrictive) decision across all requested resources, while per-resource decisions belong in ResourceSpecificResults.
Emulator: EvalResourceName is the caller's concrete ARN from the request (got 'arn:aws:ecs:<region>:<account>:task-definition/<name>:<revision>' where AWS returns the template), and the same concrete ARN is repeated for the iam:PassRole result whose AWS template is the iam one. In the same step the decisions are swapped: the top-level EvalDecision for the second action is implicitDeny where AWS reports allowed, while ResourceSpecificResults.member[1].EvalResourceDecision is allowed where AWS reports implicitDeny.
Why: this is an authorization answer, not cosmetics - a caller asking "may this principal PassRole/RunTask here" is told the opposite of what AWS says, in both directions, so a pre-flight permission check both passes calls AWS would deny and blocks calls AWS would allow. Keying results by EvalResourceName also breaks, because every result carries the same concrete ARN instead of the per-action template.
Fix: the SimulatePrincipalPolicy handler must derive EvalResourceName from the action's resource-ARN template (or "*" when the action defines none), and the aggregation step must compute the top-level EvalDecision as the most restrictive of the per-resource decisions while keeping each ResourceSpecificResults entry aligned with its own resource ARN.
Traces: iam-ebecs-events-role-run-task-pass-role-simulation#task#4.

## Evidence
Replayed corpus simparity-wire-aws-v1, 4 divergence clusters, all verdict=mismatch, expected_status 200 / actual_status 200.

Cause 1 - iam-agentcore-traj-team-role-deny-is-read-only#task#9:
  $.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member[0].OrganizationsDecisionDetail: missing (expected {'AllowedByOrganizations': 'true'}, got None)
  $...EvaluationResults.member[0].EvalDecisionDetails: missing (expected '', got None)
  $...EvaluationResults.member[1].OrganizationsDecisionDetail: missing (expected {'AllowedByOrganizations': 'true'}, got None)
  $...EvaluationResults.member[1].EvalDecisionDetails: missing (expected '', got None)

Cause 2 - statement column:
  iam-agentcore-traj-team-role-deny-is-read-only#task#9: $...member[0].MatchedStatements.member.StartPosition.Column: expected '212', got '214' (same on ResourceSpecificResults.member.MatchedStatements.member.StartPosition.Column)
  iam-bap-simulate-with-bucket-policy#task#3: expected '287', got '288'
  iam-ebecs-events-role-run-task-pass-role-simulation#task#10: expected '323', got '325'

Cause 3 - iam-bap-simulate-with-bucket-policy#task#3:
  $...EvaluationResults.member.MissingContextValues: type_mismatch (expected '', got {'member': 'aws:SecureTransport'})

Cause 4 - iam-ebecs-events-role-run-task-pass-role-simulation#task#4:
  $...EvaluationResults.member[0].EvalResourceName: value_mismatch (expected 'arn:aws:ecs:${Region}:${Account}:${ResourceType}/${ResourcePath}', got the concrete task-definition ARN from the request)
  $...EvaluationResults.member[1].EvalResourceName: value_mismatch (expected 'arn:aws:iam::${Account}:${ResourceType}/${ResourceName}', got the same concrete ECS task-definition ARN)
  $...EvaluationResults.member[1].EvalDecision: value_mismatch (expected 'allowed', got 'implicitDeny')
  $...EvaluationResults.member[0].ResourceSpecificResults.member[1].EvalResourceDecision: value_mismatch (expected 'implicitDeny', got 'allowed')

Model references: smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json - operation SimulatePrincipalPolicy (output SimulatePolicyResponse.EvaluationResults), shapes EvaluationResult (member docs for EvalResourceName, EvalDecisionDetails, MissingContextValues, OrganizationsDecisionDetail and the aggregate-EvalDecision note), OrganizationsDecisionDetail, Statement, Position.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Cause 1: replay fixture iam-agentcore-traj-team-role-deny-is-read-only, step task#9 - a SimulatePrincipalPolicy for a role with a read-only/deny policy; inspect EvaluationResults.member[*] for OrganizationsDecisionDetail and EvalDecisionDetails.
3. Cause 2: replay iam-bap-simulate-with-bucket-policy step task#3 and iam-ebecs-events-role-run-task-pass-role-simulation step task#10 and compare MatchedStatements.member.StartPosition.Column (top-level and inside ResourceSpecificResults) against the recorded column.
4. Cause 3: in iam-bap-simulate-with-bucket-policy step task#3 the request supplies an explicit ResourceArns entry; check that the top-level MissingContextValues is empty and that any missing key appears under ResourceSpecificResults.
5. Cause 4: replay iam-ebecs-events-role-run-task-pass-role-simulation step task#4 - a SimulatePrincipalPolicy over ecs:RunTask plus iam:PassRole with explicit resource ARNs; compare EvalResourceName against the action's ARN template and compare the top-level EvalDecision and each ResourceSpecificResults entry's EvalResourceDecision against the recording.
