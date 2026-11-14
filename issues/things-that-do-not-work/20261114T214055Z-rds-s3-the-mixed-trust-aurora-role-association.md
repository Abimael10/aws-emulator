Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.cfn-deploy-rds-cluster-missing-iam-trust-negative
Issue-Key: rds__s3.cfn-deploy-rds-cluster-missing-iam-trust-negative|_assert_add_role_never_accepted#13|Timeout|assert code == "InvalidParameterValue", f"expected InvalidParameterValue for a mistrusted role, got {code}: {exc}"
Issue-Commit: 54e94d59ea058587fe0cd8b5d9bd32bdd3b540c1

# rds__s3: the mixed-trust Aurora role-association scenario never reaches a verdict — AddRoleToDBCluster acceptance / the association's terminal Status never converge inside the run budget

> _Note: this issue was identified against the code base at commit `54e94d59ea058587fe0cd8b5d9bd32bdd3b540c1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AddRoleToDBCluster

## Description
1 net-new root cause in RDS's DB-cluster IAM-role association, grounded in the one failing check under the `rds__s3` heading that is neither ALREADY FILED nor OWNED by an open issue.

Nearest open issues, none of which carries this check id or its replay key: 20261029T195009Z-rds-s3-5-net-new-root-causes-startexporttask.md (says AddRoleToDBCluster is a 501 stub — that is no longer what happens here: the call is answered, and the check's retry loop only tolerates `InvalidParameterValue`, so a 501 would have raised immediately instead of hanging); 20261114T102206Z-rds-s3-4-net-new-causes-aws-rds.md (AWS::RDS::DBCluster accepting an s3Import AssociatedRoles entry on aurora-mysql, and StartExportTask messages); 20261108T165124Z-rds-s3-the-emulated-postgresql-engine-has-no.md (the missing aws_s3 extension, an in-engine concern this check never reaches). A fix to any of those closes none of this.

Cause 1 of 1: an IAM role association on an Aurora DB cluster never settles, so neither leg of the mixed-trust claim can be observed.
  AWS: `AddRoleToDBCluster(DBClusterIdentifier, RoleArn, FeatureName="s3Import")` on an available Aurora PostgreSQL cluster accepts a role whose trust policy names `rds.amazonaws.com` (after at most the brief IAM/RDS propagation window, during which it may answer `InvalidParameterValue`), and the association then appears in `DescribeDBClusters` under `DBCluster.AssociatedRoles` and leaves `PENDING` for a TERMINAL `Status`. Smithy (rds): AddRoleToDBCluster input `DBClusterIdentifier` (required), `RoleArn` (required, IAMRoleArn @length 20..2048, @pattern ^arn:aws[a-z-]*:iam::[0-9]*:role/.*$), `FeatureName`; modelled errors DBClusterNotFoundFault (404), DBClusterRoleAlreadyExistsFault (400), DBClusterRoleQuotaExceededFault (400), InvalidDBClusterStateFault (400). The member doc of `DBClusterRole.Status` states the only three values and that two of them are terminal: "ACTIVE - the IAM role ARN is associated with the DB cluster and can be used to access other Amazon Web Services on your behalf", "PENDING - the IAM role ARN is being associated with the DB cluster", "INVALID - the IAM role ARN is associated with the DB cluster, but the DB cluster is unable to assume the IAM role". A role that stays `PENDING` for ever is therefore not a state AWS leaves an association in. The negative leg is also modelled behaviour: a role trusting `ec2.amazonaws.com` instead is refused with `InvalidParameterValue` and the message `IAM role ARN value is invalid or does not include the required permissions for: AWS_ROLE_INTEGRATION` — fixed text whose trailing token is the feature-integration name, with no request- or state-derived values in it.
  Emulator: the check never reaches any of its assertions; it is killed by the run budget (`Check timed out after 60.0s`) with no verdict. The only unbounded waits between the (accepted) CloudFormation deploy and the first assertion are exactly two, and one of them is where it is stuck: (a) `AddRoleToDBCluster` for the correctly-trusted role keeps answering `InvalidParameterValue`, so the check's retry loop sleeps 5 s per attempt against a 120 s deadline; or (b) the association is created but its `DBClusterRole.Status` in DescribeDBClusters never becomes `ACTIVE` or `INVALID`, so the terminal-status poll (10 s interval, 600 s deadline) never converges. Hypothesis: (b), because the loop in (a) raises its own AssertionError naming the last ClientError rather than hanging, and nothing in the report shows such a message. Either way the association does not settle the way the model's own Status values require.
  Why: a client that associates an IAM role with an Aurora cluster and waits for the association to be usable — the documented prerequisite for any s3Import/s3Export feature, and the standard pattern every SDK waiter and Terraform/CloudFormation provider follows — waits for ever, or is refused a role AWS accepts. Both of this check's claims become unobservable: that a correctly-trusted role settles ACTIVE, and that a role trusting the wrong service principal is refused with `InvalidParameterValue` and the `AWS_ROLE_INTEGRATION` message on every retry rather than being accepted and left broken.
  Fix: RDS's DB-cluster role-association state and the `AddRoleToDBCluster` handler — the association must be accepted for a role whose trust policy names `rds.amazonaws.com`, be projected into `DescribeDBClusters`'s `AssociatedRoles`, and advance out of `PENDING` to a terminal `ACTIVE` (or `INVALID` when the cluster cannot assume the role) rather than remaining non-terminal; the mistrusted role must take the modelled `InvalidParameterValue` refusal with the documented message.
  Traces: rds__s3.cfn-deploy-rds-cluster-missing-iam-trust-negative (verdict: mismatch, `Check timed out after 60.0s`). Check helpers `_add_role_until_accepted` (retries only on `InvalidParameterValue`, 5 s sleep, 120 s deadline) and `_poll_role_status` (predicate `s in {"ACTIVE", "INVALID"}`, 10 s interval, 600 s deadline).

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout. Report heading `rds__s3`; this check is the only failure there that is neither ALREADY FILED nor OWNED.

[mismatch] rds__s3.cfn-deploy-rds-cluster-missing-iam-trust-negative
  Error: Check timed out after 60.0s

The check deploys one CloudFormation stack (CAPABILITY_IAM/CAPABILITY_NAMED_IAM) that creates an Aurora PostgreSQL cluster and two IAM roles with the same S3 policy but different trust policies (one trusting rds.amazonaws.com, one trusting ec2.amazonaws.com), reads GoodRoleArn/BadRoleArn from the stack outputs, then:
  good_latency_s = _add_role_until_accepted(rds, cluster_id, good_role_arn, "s3Import")   # retries only InvalidParameterValue, 5 s apart, 120 s cap
  good_status    = _poll_role_status(ctx, cluster_id, good_role_arn, timeout_s=600.0)      # waits for Status in {ACTIVE, INVALID}, 10 s apart
  assert good_status == "ACTIVE"
and only afterwards holds the negative (`_assert_add_role_never_accepted`, which requires code InvalidParameterValue and the exact message 'IAM role ARN value is invalid or does not include the required permissions for: AWS_ROLE_INTEGRATION').

Expected (AWS): the deploy settles CREATE_COMPLETE, AddRoleToDBCluster accepts the rds.amazonaws.com-trusting role within the propagation window, DescribeDBClusters lists it under AssociatedRoles and its Status leaves PENDING for ACTIVE within seconds-to-minutes, and the ec2.amazonaws.com-trusting role is refused InvalidParameterValue with the AWS_ROLE_INTEGRATION message on every attempt.
Actual (emulator): no assertion in the check is ever reached — the run budget ends the check at 60 s inside one of the two waits above, so the association never settles to a terminal Status (or acceptance of the good role never happens). Cluster ids, role names, the generated master password and the measured latency are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay simparity-behavioral-aws-v1 and re-run the check id in trace_ids (report heading `rds__s3`).
3. Or directly, against the emulator:
   a. Create an IAM role whose trust policy allows sts:AssumeRole for Service principal rds.amazonaws.com, with an inline policy allowing s3:GetObject/s3:ListBucket on a bucket.
   b. Create an Aurora PostgreSQL DB cluster and wait for it to be available.
   c. rds AddRoleToDBCluster(DBClusterIdentifier=<cluster>, RoleArn=<that role>, FeatureName="s3Import").
      Expected: accepted (at most after a brief InvalidParameterValue propagation window).
   d. Poll rds DescribeDBClusters(DBClusterIdentifier=<cluster>) and read the matching entry in DBClusters[0].AssociatedRoles.
      Expected: the entry exists and its Status reaches the terminal ACTIVE (or INVALID) and does not stay PENDING.
      Actual: the wait does not converge inside the run budget.
   e. Repeat (a)-(c) with a second role whose trust policy names ec2.amazonaws.com instead.
      Expected: InvalidParameterValue with message 'IAM role ARN value is invalid or does not include the required permissions for: AWS_ROLE_INTEGRATION' on every attempt, and the role never appears as an active association.
