Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.serverless-v2-capacity-modify-current-capacity-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.serverless-v2-capacity-modify-current-capacity-rejected|run#11|AssertionError|assert error["Message"] == (
Issue-Commit: df9f105751b61bb3107878b2d592312c9fa917ee

# rds ModifyCurrentDBClusterCapacity refuses a non-Serverless-v1 cluster with the model's doc sentence instead of AWS's InvalidParameterValue message

> _Note: this issue was identified against the code base at commit `df9f105751b61bb3107878b2d592312c9fa917ee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyCurrentDBClusterCapacity

## Description
1 net-new root cause in the RDS control plane's refusal wording.

Cause 1 of 1: ModifyCurrentDBClusterCapacity against a cluster that is not Aurora Serverless v1 is refused with the wrong message text.
AWS: ModifyCurrentDBClusterCapacity "only applies to Aurora Serverless v1 DB clusters" (Smithy: com.amazonaws.rds#ModifyCurrentDBClusterCapacity documentation; its modeled errors are DBClusterNotFoundFault/InvalidDBClusterCapacityFault/InvalidDBClusterStateFault only, so this refusal is the Query-API common error and is recorded behavior only). The recorded response for a cluster created with a ServerlessV2ScalingConfiguration (engine aurora-postgresql, engine mode provisioned/Serverless v2) is HTTP 400 with Error.Code "InvalidParameterValue" and Error.Message, fixed in full, "Only Aurora Serverless clusters can have their capacity modified." — the message has no variable parts: it names neither the identifier, the capacity, nor the engine, so the whole sentence is the requirement.
Emulator: the code is right (InvalidParameterValue, 400) but the message is the model's documentation sentence, "This operation only applies to Aurora Serverless v1 DB clusters." The check asserts the message and fails with: AssertionError: unexpected message 'This operation only applies to Aurora Serverless v1 DB clusters.'
Why: a client that branches on, logs or asserts the refusal text of a v1-only capacity call sees prose AWS never emits; parity tests and support tooling that match AWS's sentence do not match, and the emulator's sentence misdescribes the rule (AWS refuses this for any non-Serverless cluster, not only for a "v1" distinction).
Fix: the refusal message constant used by the non-Serverless-v1 branch of the ModifyCurrentDBClusterCapacity handler's validation (the `SERVERLESS_V1_ONLY_MESSAGE` text it hands to `invalid_parameter_value`) in the RDS service's state/message module. The error code and status already match; only the text diverges. Nothing else in the scenario diverged: the rejected call left ServerlessV2ScalingConfiguration unchanged and the ModifyDBCluster positive control applied the new range.
Traces: rds.serverless-v2-capacity-modify-current-capacity-rejected.

## Evidence
Check rds.serverless-v2-capacity-modify-current-capacity-rejected:
  ModifyCurrentDBClusterCapacity(DBClusterIdentifier=<aurora-postgresql Serverless v2 cluster>, Capacity=256)
  Expected (AWS): HTTP 400, Error.Code=InvalidParameterValue, Error.Message="Only Aurora Serverless clusters can have their capacity modified."
  Actual (emulator): HTTP 400, Error.Code=InvalidParameterValue (matches), Error.Message="This operation only applies to Aurora Serverless v1 DB clusters."
  Failure: File "check_rds_serverless_v2_capacity_modify_current_capacity_rejected.py", line 51, in run — AssertionError: unexpected message 'This operation only applies to Aurora Serverless v1 DB clusters.'
Source of the diverging text: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/modify_current_db_cluster_capacity.rs:231-235 returns `invalid_parameter_value(crate::state::SERVERLESS_V1_ONLY_MESSAGE)`.

## Steps to Reproduce
1. CreateDBCluster(DBClusterIdentifier=<id>, Engine=aurora-postgresql, MasterUsername=..., MasterUserPassword=..., ServerlessV2ScalingConfiguration={MinCapacity:0.5, MaxCapacity:2.0}, DBSubnetGroupName=<subnet group>) and wait for Status=available.
2. Call ModifyCurrentDBClusterCapacity(DBClusterIdentifier=<id>, Capacity=256).
3. Observe the error: the code is InvalidParameterValue but the message is "This operation only applies to Aurora Serverless v1 DB clusters." instead of AWS's "Only Aurora Serverless clusters can have their capacity modified."
