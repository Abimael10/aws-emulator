Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-parameter-group-memory-formula-value-range
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-parameter-group-memory-formula-value-range
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter.AllowedValues
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter.Description
Issue-Commit: d047657e3f70a5c5997e5c7e5b277c3b7338dc33

# rds DescribeDBParameters serves emulator-authored innodb_buffer_pool_size metadata: paraphrased Description and a truncated AllowedValues upper bound

> _Note: this issue was identified against the code base at commit `d047657e3f70a5c5997e5c7e5b277c3b7338dc33`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBParameters

## Description
When DescribeDBParameters is read over a MySQL-family DB parameter group whose memory-formula parameter (the InnoDB buffer-pool size) is inspected, the emulator answers 200 with a well-formed DBParameterGroupDetails envelope, but the per-parameter metadata it carries is the emulator's own prose and its own numeric upper bound rather than the engine parameter catalog's published metadata that AWS returns. THERE IS ONE CAUSE IN THIS BUNDLE.

=== Cause 1 of 1: the InnoDB buffer-pool-size parameter's Description is paraphrased and its AllowedValues upper bound is a signed-64-bit maximum instead of the catalog's unsigned-64-bit maximum ===

AWS: Smithy: DescribeDBParameters -> DBParameterGroupDetails{Parameters: ParametersList, Marker}; com.amazonaws.rds#Parameter members Description ('Provides a description of the parameter.') and AllowedValues ('Specifies the valid range of values for the parameter.'). The model fixes the shape only; the two strings are values of the engine parameter catalog, so they are fixed per-parameter text and not per-run values. Recorded behavior confirms the catalog content AWS serves for this parameter: Description is exactly 'The size in bytes of the memory buffer innodb uses to cache data and indexes of its tables' and AllowedValues is exactly the hyphenated range '5242880-18446744073709551615' — i.e. the lower bound is the catalog's 5 MiB minimum and the upper bound is the full unsigned 64-bit maximum (2^64-1), which is what the MySQL buffer-pool size accepts. Both parts are fixed text; neither contains any value taken from the request, from emulator state, from a host/port, an id or a timestamp.

Emulator: the same parameter entry is served with the emulator's own summary, 'The size in bytes of the buffer pool InnoDB caches table and index data in.', in place of the catalog sentence, and with AllowedValues '5242880-9223372036854775807' — the upper bound truncated to the SIGNED 64-bit maximum (2^63-1), roughly half the range AWS publishes. Status is 200 -> 200 on both affected steps, so nothing surfaces to the client as an error; the response simply carries different content. The divergence reproduces on two steps of the same fixture (task#7 and task#9), i.e. it is a property of the served catalog row and not of one request.

Why: a client that trusts AllowedValues as the authoritative legal range — a parameter validator, a tuning/advisory tool, a drift report, or a console-style parameter browser that renders a slider or bound-checks an operator's input before calling ModifyDBParameterGroup — will reject buffer-pool sizes above 2^63-1 that AWS accepts, and a drift report that diffs served metadata against AWS's catalog flags the row as changed on every read. The paraphrased Description makes any generated documentation or audit snapshot differ from AWS's text for the same parameter name, so string-equality comparisons against the published catalog never converge.

Fix: the DescribeDBParameters handler's per-family parameter catalog — the Description string and the AllowedValues range expression for the InnoDB buffer-pool-size parameter — and the projection of those two members in its serializer. The upper bound must be the catalog's unsigned 64-bit maximum, not the signed one, so the range must be carried as a string/unsigned value rather than clamped through a signed 64-bit integer.

Traces: rds-db-parameter-group-memory-formula-value-range#task#7, rds-db-parameter-group-memory-formula-value-range#task#9.

Related but NOT covering: open issue 20261119T201015Z-rds-describedbparameters-pages-serve-emulator-authored-parameter-metadata.md covers DescribeDBParameters metadata divergences on the rds-db-parameters-pagination fixture's pages (task#4/#5/#8) for a different parameter set (binary-log/MyISAM, InnoDB compression, table-cache/adaptive-flushing) and a different AllowedValues defect (enumerated '0,1,2' instead of the range '0-2'). Also related: 20261115T081244Z-rds-describeenginedefaultparameters-serves-an-engine-default-catalog-whose.md (DescribeEngineDefaultParameters) and 20261114T091417Z-rds-6-of-the-9-reference-catalog-causes-remain-open.md. The replay reports this cluster as stub_path='' and owned_by=[], i.e. unowned by any of them, and closing them does not correct this parameter's Description or its unsigned upper bound.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true; '/__admin/health answered 200 and the data listener accepts'). The cluster is a content divergence on an expected-status response: expected_status 200, actual_status 200, verdict 'mismatch'. The replay reports stub_path='' and owned_by=[] for it, i.e. it is not covered by any existing issue, and affected_count=2.

Representative step — rds-db-parameter-group-memory-formula-value-range#task#7, DescribeDBParameters, 200 -> 200:
  $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter.AllowedValues: value_mismatch
    expected '5242880-18446744073709551615'  (lower bound 5242880 = 5 MiB; upper bound 18446744073709551615 = 2^64-1)
    got      '5242880-9223372036854775807'   (upper bound 9223372036854775807 = 2^63-1, the SIGNED 64-bit maximum)
  $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter.Description: value_mismatch
    expected 'The size in bytes of the memory buffer innodb uses to cache data and indexes of its tables'
    got      'The size in bytes of the buffer pool InnoDB caches table and index data in.'

Second member of the same cluster — rds-db-parameter-group-memory-formula-value-range#task#9: identical two field paths, identical expected-vs-actual values.

Divergence key: service=rds|DescribeDBParameters|status=200->200|keys=$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter.AllowedValues,$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter.Description

Model evidence (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json), via smithy_query shape=Parameter: com.amazonaws.rds#Parameter.Description -> String, doc 'Provides a description of the parameter.'; com.amazonaws.rds#Parameter.AllowedValues -> String, doc 'Specifies the valid range of values for the parameter.'. Both are plain strings whose content is the engine catalog's, so the divergence is in the emulator's catalog data, not in the wire shape. No host, port, generated id, request id or timestamp is part of either expected value.

## Steps to Reproduce
1. git_clone the project repo and build/boot the emulator from this checkout.
2. Replay trace set simparity-wire-aws-v1 against it (replay_wire_corpus).
3. Replay fixture rds-db-parameter-group-memory-formula-value-range, step task#7 — DescribeDBParameters over the MySQL-family DB parameter group used by the memory-formula / value-range scenario.
4. In the response, locate the InnoDB buffer-pool-size parameter entry under $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter and confirm:
   - Description is exactly 'The size in bytes of the memory buffer innodb uses to cache data and indexes of its tables' (the catalog sentence), not the emulator's 'The size in bytes of the buffer pool InnoDB caches table and index data in.';
   - AllowedValues is exactly '5242880-18446744073709551615' (upper bound = unsigned 64-bit max), not '5242880-9223372036854775807'.
5. Repeat for step task#9 of the same fixture, which reads the same parameter row and must report the same two values.
6. Both steps must replay clean before the issue is closed.
