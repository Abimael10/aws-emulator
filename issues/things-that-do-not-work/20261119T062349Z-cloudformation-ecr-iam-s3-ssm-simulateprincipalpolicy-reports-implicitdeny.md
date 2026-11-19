Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-custom-permissions-boundary-bounds-only-the-exec-role cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-custom-permissions-boundary-with-a-path-bounds-the-exec-role cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-example-permissions-boundary-bounds-the-exec-role-and-denies-its-removal
Issue-Key: cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-custom-permissions-boundary-bounds-only-the-exec-role|run#23|AssertionError|assert decisions == {
Issue-Key: cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-custom-permissions-boundary-with-a-path-bounds-the-exec-role|run#25|AssertionError|assert decisions == {
Issue-Key: cloudformation__ecr__iam__s3__ssm.cdk-bootstrap-example-permissions-boundary-bounds-the-exec-role-and-denies-its-removal|run#32|AssertionError|assert decisions == {
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudformation__ecr__iam__s3__ssm: SimulatePrincipalPolicy reports implicitDeny for an action a role's permissions boundary excludes, where AWS reports explicitDeny

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SimulatePrincipalPolicy

## Description
Cause 1 of 1: the aggregate EvalDecision SimulatePrincipalPolicy returns for an action blocked by the principal's permissions boundary is implicitDeny instead of explicitDeny.
AWS: Smithy — EvaluationResult.EvalDecision targets PolicyEvaluationDecisionType with values allowed | explicitDeny | implicitDeny, and EvaluationResult.PermissionsBoundaryDecisionDetail "contains information about the effect that a permissions boundary has on a policy simulation when the boundary is applied to an IAM entity". A permissions boundary is a ceiling: for a principal whose identity policy allows the action but whose attached boundary does not permit it (or explicitly denies it), the simulated decision is explicitDeny with PermissionsBoundaryDecisionDetail.AllowedByPermissionsBoundary = false; an action both the identity policy and the boundary allow is allowed with AllowedByPermissionsBoundary = true.
Emulator: for the cdk toolkit's cfn-exec role, whose PermissionsBoundary is attached and read back correctly, SimulatePrincipalPolicy returns ('implicitDeny', AllowedByPermissionsBoundary=false) for the out-of-boundary action — the boundary is taken into account in the detail flag but not in the decision label, so the decision is indistinguishable from an action nothing allows. The in-boundary control action is reported ('allowed', true) as AWS does, so only the deny label diverges.
Why: callers classify denials by EvalDecision — explicitDeny means a Deny (here, a boundary ceiling) that no added Allow can lift, implicitDeny means a missing Allow that granting a permission would fix. Reporting implicitDeny tells a policy-review or least-privilege tool to widen the identity policy, which in AWS would change nothing, and makes a boundary look absent in simulation output.
Fix: the decision aggregation in the IAM SimulatePrincipalPolicy handler's policy evaluator — when the permissions boundary is what blocks the action, the returned EvalDecision must be explicitDeny (while PermissionsBoundaryDecisionDetail.AllowedByPermissionsBoundary stays false).
Related but not covering: open issues 20261103T132715Z-iam-simulateprincipalpolicy-evaluation-results-omit-modeled-detail-members.md (missing detail members / aggregate decision) and 20261113T030715Z-cloudformation-ecr-iam-s3-ssm-cdk-bootstrap-toolkit-the-cloudformation-created-cfn-exec.md (the exec role carrying no boundary at all); the boundary is now attached and simulated, and the surviving divergence is the deny label these three checks assert.

## Evidence
All three checks fail on the same assertion shape, after _assert_bootstrap_roles confirmed the boundary is attached to the cfn-exec role:

1) cdk-bootstrap-custom-permissions-boundary-bounds-only-the-exec-role
   expected: {'s3:ListAllMyBuckets': ('allowed', True), 'sns:CreateTopic': ('explicitDeny', False)}
   actual:   {'s3:ListAllMyBuckets': ('allowed', True), 'sns:CreateTopic': ('implicitDeny', False)}

2) cdk-bootstrap-custom-permissions-boundary-with-a-path-bounds-the-exec-role
   expected: {'s3:ListAllMyBuckets': ('allowed', True), 'sns:CreateTopic': ('explicitDeny', False)}
   actual:   {'s3:ListAllMyBuckets': ('allowed', True), 'sns:CreateTopic': ('implicitDeny', False)}

3) cdk-bootstrap-example-permissions-boundary-bounds-the-exec-role-and-denies-its-removal
   expected: {'s3:ListAllMyBuckets': ('allowed', True), 'iam:DeleteRolePermissionsBoundary': ('explicitDeny', False)}
   actual:   {'s3:ListAllMyBuckets': ('allowed', True), 'iam:DeleteRolePermissionsBoundary': ('implicitDeny', False)}
(tuple = (EvalDecision, PermissionsBoundaryDecisionDetail.AllowedByPermissionsBoundary))

## Steps to Reproduce
1. `cdk bootstrap --qualifier <q> --custom-permissions-boundary <policy>` (or `--example-permissions-boundary`), so the cfn-exec role cdk-<q>-cfn-exec-role-<account>-<region> carries the policy as its PermissionsBoundary.
2. Call iam:SimulatePrincipalPolicy with PolicySourceArn = that role's ARN and ActionNames = one action the boundary allows (s3:ListAllMyBuckets) plus one it does not (sns:CreateTopic, or iam:DeleteRolePermissionsBoundary for the example boundary).
3. Read EvalDecision and PermissionsBoundaryDecisionDetail.AllowedByPermissionsBoundary per action: the emulator reports implicitDeny/false for the blocked action; AWS reports explicitDeny/false.
