Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.create-instance-validation-rejected rds.master-username-case-preservation-round-trip rds.master-username-length-boundary-postgres
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.create-instance-validation-rejected|_reject#7|AssertionError|raise AssertionError(
Issue-Key: rds|CreateDBInstance|InvalidParameterValue
Issue-Commit: a7bef14f25237d595eccda8ae7c58763b2cfbcfb

# RDS CreateDBInstance input validation: storage-type AllocatedStorage floor not enforced, and MasterUsername checked with one MySQL-shaped rule for every engine (2 net-new causes)

> _Note: this issue was identified against the code base at commit `a7bef14f25237d595eccda8ae7c58763b2cfbcfb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBInstance

## Description
Two net-new root causes in the RDS CreateDBInstance input-validation path. Both are grounded in failing behavioral checks of this suite; neither is covered by an open issue (the nearest, 20261111T084329Z-rds-four-control-plane-validation-and-representation-divergences.md, covers MaxAllocatedStorage's 10% floor, not the AllocatedStorage create-time floor, and no open issue mentions MasterUsername validation for RDS CreateDBInstance).

Cause 1 of 2: CreateDBInstance accepts an AllocatedStorage below the minimum for the REQUESTED StorageType, instead of refusing it.
AWS: the AllocatedStorage floor is scoped to the storage type, not to the engine alone. Smithy: CreateDBInstanceMessage.AllocatedStorage documentation, 'RDS for PostgreSQL ... General Purpose (SSD) storage (gp2, gp3): Must be an integer from 20 to 65536' (magnetic 'standard' starts at 5). A create with Engine=postgres, StorageType=gp3 and AllocatedStorage below 20 is refused with HTTP 400 and error code InvalidParameterValue (InvalidParameterCombination is the other code AWS uses for this class), and the message names the engine and the storage type whose floor was violated — i.e. its variable parts are the engine name and storage type taken from the request (here 'postgres' and 'gp3') and the floor for that pair; nothing is created.
Emulator: the request is ACCEPTED. The check's assertion is 'allocated storage below the gp3 minimum: CreateDBInstance was ACCEPTED; real AWS refuses this input (mutation={'AllocatedStorage': 5})'. No refusal, no error code, and an instance is minted. The sibling legs of the same check (reserved master username 'rdsadmin', password under the 8-character minimum) are refused correctly, so only the storage-type floor is missing.
Why: a client that relies on AWS to reject an undersized volume at create time gets a DB instance the real service would never have created, with an AllocatedStorage real RDS cannot represent for that storage type. Capacity-sizing and IaC pre-flight logic that treats the 400 as the signal silently proceeds, and the same template then fails against real AWS.
Fix: the CreateDBInstance handler's validation stage — add the storage-type-scoped AllocatedStorage range check (resolve the effective StorageType first; an unspecified StorageType defaults to gp2, whose floor differs) and emit the InvalidParameterValue refusal naming the engine and storage type.
Traces: rds.create-instance-validation-rejected.

Cause 2 of 2: MasterUsername is validated with a single 'must be 1 to 16 letters or numbers' rule applied to every engine, so admin user names real AWS accepts on postgres are refused at CreateDBInstance, and the refusal message is not AWS's.
AWS: the master-user-name length and character rules follow the chosen engine's own identifier rules. The model's generic prose (Smithy: CreateDBInstanceMessage.MasterUsername, 'Must be 1 to 16 letters, numbers, or underscores. First character must be a letter. Can't be a reserved word for the chosen database engine.') states the MySQL/MariaDB-shaped bound, but recorded behavior for Engine=postgres accepts names well past 16 characters — a 17-character mixed-case name and a 25-character name containing underscores are both accepted, stored and returned verbatim (no lowercasing, no truncation) — and refuses one past PostgreSQL's own ceiling (64 characters, one past rolname's 63) with HTTP 400, error code InvalidParameterValue and message 'Invalid master user name'. In that refusal 'Invalid master user name' is the fixed text; the never-created identifier is then reported by DescribeDBInstances as DBInstanceNotFound with message 'DBInstance <identifier> not found.', whose only variable part is the identifier from the request. Note also that underscores are permitted by the model's own prose, so refusing them is wrong under either reading.
Emulator: both accepted-by-AWS creates are refused with InvalidParameterValue and a message of its own wording: 'MasterUsername must be 1 to 16 letters or numbers; got 17 characters' for the 17-character mixed-case name, and 'MasterUsername must be 1 to 16 letters or numbers; got <name>' for the 25-character underscore-containing name. The rule is engine-independent, the character class excludes underscores, and the message text ('MasterUsername must be 1 to 16 letters or numbers; got ...') is not the text AWS emits for this class ('Invalid master user name'). Because the create is refused, the checks never reach their round-trip assertions on casing preservation, so whether the stored name round-trips exactly is still unverified.
Why: every client that creates a postgres (or Oracle/SQL Server) DB instance with an admin name longer than 16 characters, or containing an underscore, is refused outright — the instance is never created, so an application, CDK app or CloudFormation stack that works on real AWS cannot be stood up at all. Clients that match on the refusal text for the over-limit case also fail to recognise it.
Fix: the CreateDBInstance handler's MasterUsername validation — make the length ceiling and character class engine-scoped rather than a single 16-character alphanumeric rule, allow underscores, and emit AWS's InvalidParameterValue / 'Invalid master user name' message when the engine's own ceiling is exceeded; keep the accepted name stored and echoed with its original casing in both the CreateDBInstance response and DescribeDBInstances.
Traces: rds.master-username-length-boundary-postgres, rds.master-username-case-preservation-round-trip.

## Evidence
Cause 1 — check rds.create-instance-validation-rejected:
  Request: CreateDBInstance Engine=postgres, DBInstanceClass=db.t3.micro, StorageType=gp3, AllocatedStorage=5 (every other field legal).
  Expected (real AWS): HTTP 400, Error.Code InvalidParameterValue (or InvalidParameterCombination), message naming the engine ('postgres') and storage type ('gp3') whose floor was violated; no instance minted (DescribeDBInstances -> DBInstanceNotFound).
  Actual (emulator): 2xx — the create was ACCEPTED and the instance minted. Harness failure: "AssertionError: allocated storage below the gp3 minimum: CreateDBInstance was ACCEPTED; real RDS refuses this input (mutation={'AllocatedStorage': 5})".
  Smithy citation: CreateDBInstanceMessage.AllocatedStorage docs — 'RDS for PostgreSQL ... General Purpose (SSD) storage (gp2, gp3): Must be an integer from 20 to 65536'.

Cause 2 — check rds.master-username-case-preservation-round-trip:
  Request: CreateDBInstance Engine=postgres, MasterUsername='Sim2RealAdminUser' (17 characters, letters only), AllocatedStorage=20, db.t3.micro.
  Expected (real AWS): accepted; CreateDBInstance and DescribeDBInstances both echo MasterUsername exactly as supplied.
  Actual (emulator): ClientError (InvalidParameterValue) when calling the CreateDBInstance operation: 'MasterUsername must be 1 to 16 letters or numbers; got 17 characters'.

Cause 2 — check rds.master-username-length-boundary-postgres:
  Request A: CreateDBInstance Engine=postgres, MasterUsername='application_db_admin_user' (25 characters, underscores), AllocatedStorage=10, db.t3.micro.
  Expected (real AWS): accepted and echoed verbatim on create and on describe.
  Actual (emulator): ClientError (InvalidParameterValue) when calling the CreateDBInstance operation: "MasterUsername must be 1 to 16 letters or numbers; got 'application_db_admin_user'".
  Request B (never reached): same engine, MasterUsername 64 characters ('a'+'b'*63).
  Expected (real AWS): ('InvalidParameterValue', 'Invalid master user name'), and DescribeDBInstances on the refused identifier -> ('DBInstanceNotFound', 'DBInstance <identifier> not found.').
  Smithy citation: CreateDBInstanceMessage.MasterUsername docs — 'Must be 1 to 16 letters, numbers, or underscores. First character must be a letter. Can't be a reserved word for the chosen database engine.' (underscores permitted; the 16 bound is the MySQL/MariaDB-shaped prose, while recorded postgres behavior accepts 17 and 25 characters).

## Steps to Reproduce
Cause 1:
  1. CreateDBSubnetGroup over the run's VPC subnets.
  2. CreateDBInstance with DBInstanceIdentifier=<new id>, Engine=postgres, EngineVersion=<DescribeDBEngineVersions DefaultOnly result>, DBInstanceClass=db.t3.micro, StorageType=gp3, AllocatedStorage=5, MasterUsername=sim2real, MasterUserPassword=<>=8 chars>, BackupRetentionPeriod=0, DBSubnetGroupName=<group>.
  3. Observe the call is accepted (real AWS answers HTTP 400 InvalidParameterValue naming postgres and gp3), and DescribeDBInstances on the identifier returns a record instead of DBInstanceNotFound.
  4. Repeat with AllocatedStorage=20 and StorageType=gp3: this must be accepted (control case), confirming the floor, not the field, is the defect.

Cause 2:
  1. CreateDBSubnetGroup over the run's VPC subnets.
  2. CreateDBInstance with Engine=postgres, DBInstanceClass=db.t3.micro, AllocatedStorage=20, MasterUsername='Sim2RealAdminUser' (17 characters) -> emulator refuses with InvalidParameterValue 'MasterUsername must be 1 to 16 letters or numbers; got 17 characters'; real AWS accepts and echoes the name with its casing intact on both the create response and DescribeDBInstances.
  3. CreateDBInstance with Engine=postgres, MasterUsername='application_db_admin_user' (25 characters, underscores), AllocatedStorage=10 -> emulator refuses with the same rule; real AWS accepts and round-trips it unmangled.
  4. CreateDBInstance with Engine=postgres and a 64-character MasterUsername -> must be refused with ('InvalidParameterValue', 'Invalid master user name'), and DescribeDBInstances on that identifier must answer ('DBInstanceNotFound', 'DBInstance <identifier> not found.').
