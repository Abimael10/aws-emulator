Issue-Source: trace-sweep-divergence
Issue-Trace: neptune-npt-traj-create-cluster-request-shape
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: neptune-npt-traj-create-cluster-request-shape
Issue-Shape-Id: com.amazonaws.neptune#AmazonRDSv19
Issue-Key: neptune|CreateDBCluster|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 7ea62d1f718eb0ddea36ce5991d6f153f8f96be6

# Neptune CreateDBCluster invalid-DBClusterIdentifier refusal message is emulator-authored not AWS wording

> _Note: this issue was identified against the code base at commit `7ea62d1f718eb0ddea36ce5991d6f153f8f96be6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBCluster

## Description
THIS ISSUE HAS 1 CAUSE.

Cause 1 of 1: CreateDBCluster with a syntactically invalid DBClusterIdentifier is refused with the right HTTP status and error code but with a message the emulator authored itself instead of Neptune's wording.
AWS: Smithy (com.amazonaws.neptune#AmazonRDSv19) CreateDBCluster input CreateDBClusterMessage member DBClusterIdentifier is a required plain String with NO modelled pattern or length constraint (the only patterned identifier on the input is GlobalClusterIdentifier, ^[A-Za-z][0-9A-Za-z-:._]*$), and no modelled error of CreateDBCluster is identifier-syntax specific (DBClusterAlreadyExistsFault 400, DBClusterNotFoundFault 404, DBClusterParameterGroupNotFound 404, DBClusterQuotaExceededFault 403, ... InvalidVPCNetworkStateFault 400) — so the refusal travels as the awsQuery error envelope and the text is recorded behavior only: HTTP 400 with ErrorResponse/Error/Message 'Invalid database cluster identifier:  <DBClusterIdentifier>'. The fixed text is 'Invalid database cluster identifier: ' and the only variable part is the rejected identifier taken verbatim from the request (the recording shows two spaces between the colon and the value). Error.Type/Error.Code and the 400 status already agree with the recording; RequestId is per-run and not a requirement.
Emulator: answers 400 with Error.Message 'The parameter DBClusterIdentifier is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.' — the generic RDS-style identifier prose, which shares no fixed text with AWS's sentence and never echoes the rejected value.
Why: Neptune reports this refusal with a short, value-echoing message; a client that logs, surfaces or matches the refusal text (console error surfacing, log scraping, a message-keyed test or an error-classification branch on the 'Invalid database cluster identifier' class) sees an unrelated sentence, and cannot read the offending identifier back out of the message because the emulator drops it.
Fix: the CreateDBCluster handler's DBClusterIdentifier validation — the 400 it raises must carry Neptune's 'Invalid database cluster identifier: ' template with the request's identifier appended, instead of the shared RDS generic identifier message.
Traces: neptune-npt-traj-create-cluster-request-shape#task#3.

Related open issues, none of which carries this replay key (replay reports the cluster with owned_by=[] and stub_path=""): 20261121T112636Z-neptune-cluster-endpoint-refusal-messages-are-emulator-authored.md reports the same emulator-authored-message family for the cluster-ENDPOINT operations (its cause 5 is the analogous 'Invalid cluster endpoint identifier: ' text on CreateDBClusterEndpoint), and 20261117T150412Z-neptune-3-net-new-causes-createdbcluster-ignores-the.md / 20261119T170405Z-neptune-2-net-new-port-causes-a-db.md report it for the CreateDBCluster/ModifyDBCluster port-range and cluster-not-empty refusals. The DBClusterIdentifier-syntax refusal on CreateDBCluster is net-new.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, bootstrap_signal=false, 19 fixtures replayed, 181 steps, 171 passed, 10 mismatched, 0 errored, 0 blocked, score_pct=94.5, divergence_count=9 (all service=neptune; 6 carry a non-empty owned_by and were skipped).

fixture neptune-npt-traj-create-cluster-request-shape, step task#3, operation CreateDBCluster, expected_status 400 / actual_status 400, verdict mismatch, owned_by=[], stub_path="", affected_count=1. divergence_key: service=neptune|CreateDBCluster|status=400->400|keys=$.ErrorResponse.Error.Message. Detail: "$.ErrorResponse.Error.Message: value_mismatch (expected 'Invalid database cluster identifier:  Bad_Name!', got 'The parameter DBClusterIdentifier is not a valid identifier. Identifiers must begin with a letter; must contain only ASCII letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive hyphens.')".

Only Error.Message diverges: the 400 status and the awsQuery error code/type match the recording. 'Bad_Name!' is this recording's request value filling the template 'Invalid database cluster identifier: ' + <identifier from the request>, not part of the requirement.

## Steps to Reproduce
1. Clone the repo, boot the emulator built from it, and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
2. POST the Neptune query-protocol Action=CreateDBCluster with Engine naming the graph engine and a DBClusterIdentifier containing underscores/uppercase/'!' (a syntactically invalid identifier) — fixture neptune-npt-traj-create-cluster-request-shape, step task#3.
3. Observe HTTP 400 whose ErrorResponse/Error/Message is the generic RDS identifier prose ('The parameter DBClusterIdentifier is not a valid identifier. ...') instead of 'Invalid database cluster identifier: ' followed by the rejected identifier taken from the request.
4. Re-replay neptune-npt-traj-create-cluster-request-shape (task#3) to confirm the fix.
