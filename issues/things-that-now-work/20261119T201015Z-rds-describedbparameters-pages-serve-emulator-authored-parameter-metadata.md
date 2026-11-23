Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-parameters-pagination
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-parameters-pagination
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[*].AllowedValues
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[*].Description
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[*].MinimumEngineVersion
Issue-Commit: 0d5fdb6d7a78988c971aec85849026eed9fa5958

# rds DescribeDBParameters pages serve emulator-authored parameter metadata: paraphrased Description, wrong AllowedValues range form, stray MinimumEngineVersion (3 causes)

> _Note: this issue was identified against the code base at commit `0d5fdb6d7a78988c971aec85849026eed9fa5958`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBParameters

## Description
When DescribeDBParameters is paged over a MySQL-family DB parameter group, every page answers 200 with a well-formed DBParameterGroupDetails envelope, but the per-parameter metadata inside it is the emulator's own prose and its own value-range formatting rather than the engine catalog's published metadata that AWS returns. Three distinct pages of the same paged enumeration diverge, each for its own set of parameters, and each must be closed separately. THERE ARE THREE CAUSES IN THIS BUNDLE.

The defect class is general: Parameter.Description must be the engine catalog's own text for that parameter (a fixed per-parameter string, not a value from any one run), Parameter.AllowedValues must be the catalog's range expression (a contiguous integer range is expressed as a hyphenated range, e.g. '0-2', not as an enumerated list '0,1,2'), and Parameter.MinimumEngineVersion must be present only for parameters for which the catalog pins one. Only the parameter names, their Description/AllowedValues/MinimumEngineVersion and the page boundaries are requirements here; nothing in these causes depends on a host, port, id or timestamp.

Related but NOT covering: open issue 20261114T091417Z-rds-reference-catalogs-and-parameter-metadata-are-emulator.md (cause 5) and 20261113T145556Z-rds-parameter-group-reads-are-served-from-the.md own DescribeDBParameters on the rds-db-parameter-group-lifecycle fixture (Source/IsModifiable/parameter-set divergence on page 1). The three causes below are on the rds-db-parameters-pagination fixture's later pages, are reported by the replay as unowned, and are a different parameter set and a different defect set (Description text, AllowedValues range form, MinimumEngineVersion presence), so fixing those issues does not close these.

=== Cause 1 of 3: paraphrased Descriptions, enumerated AllowedValues and an invented MinimumEngineVersion on the binary-log/MyISAM page ===
AWS: Smithy: DescribeDBParameters (input DBParameterGroupName @required, Source, Filters, MaxRecords, Marker; output DBParameterGroupDetails{Parameters, Marker}; paginated {inputToken: Marker, outputToken: Marker, items: Parameters, pageSize: MaxRecords}); shape Parameter members Description ('Provides a description of the parameter.'), AllowedValues ('Specifies the valid range of values for the parameter.'), MinimumEngineVersion ('The earliest engine version to which the parameter can apply.'). The model fixes the shape; the strings themselves are the engine catalog's values, so for this page AWS returns exactly the catalog text: for the binlog commit-sync parameter 'Controls how many microseconds the binary log commit waits before synchronizing the binary log file to disk.', for the group-commit flush-queue parameter 'How long in microseconds to keep reading transactions from the flush queue before proceeding with the group commit (and syncing the log to disk, if sync_binlog is greater than 0). If the value is 0 (the default), there is no timeout and the server keeps reading new transactions until the queue is empty.', and for the transaction completion type 'The transaction completion type (0 - default, 1 - chain, 2 - release)' with AllowedValues '0-2'. Recorded behavior only for MinimumEngineVersion at index 12: the recording's entry has a different value from the one the emulator serves, which establishes the emulator's value is not the catalog's.
Emulator: the same page is served with emulator-authored summaries in place of the catalog text ('The time in microseconds the server waits before synchronizing transactions to disk.', 'The time in microseconds the server keeps reading transactions before flushing them to the binary log.', 'The transaction completion type.'), with AllowedValues rendered as the enumerated list '0,1,2' where AWS gives the range '0-2' (on the two parameters at indices 4 and 5), and with a MinimumEngineVersion at index 12 that diverges from the catalog. 15 field paths diverge on this one step (indices 2,3,4,5,6,7,10,11,12,14,15,16); status is 200 -> 200, so nothing surfaces as an error.
Why: a client that compares served parameter metadata against AWS's catalog - drift reporting, parameter-hardening checks, a console-style parameter browser, or a validator that parses AllowedValues to decide whether a candidate value is legal - sees a difference on nearly every parameter it inspects. The AllowedValues form is the worse failure: a validator that parses '0,1,2' as an enumeration and '0-2' as a range will accept/reject different inputs than AWS does, and a MinimumEngineVersion AWS does not pin makes an upgrade-eligibility check conclude a parameter is unavailable on the running engine version.
Fix: the DescribeDBParameters handler's per-family parameter catalog (the Description strings, the AllowedValues range expressions and the presence/value of MinimumEngineVersion) and the projection of those fields in its serializer.
Traces: rds-db-parameters-pagination#task#4.

=== Cause 2 of 3: a parameter is served another parameter's Description, plus the same AllowedValues/Description defects on the InnoDB compression page ===
AWS: Smithy: as cause 1 - Parameter.Description 'Provides a description of the parameter.', Parameter.AllowedValues 'Specifies the valid range of values for the parameter.'; the strings are catalog values. On this page AWS returns, for the InnoDB compression padding parameter, 'Sets the cutoff point at which MySQL begins adding padding within compressed pages to avoid expensive compression failures.'; for the row-lock timeout parameter 'Timeout in seconds an innodb transaction may wait for a row lock before giving up'; and AllowedValues '0-2' for the three-valued parameter at index 3.
Emulator: the entry at index 0 carries the description of a DIFFERENT parameter - 'Specifies the level of zlib compression to use for InnoDB compressed tables and indexes.' (the zlib compression-level parameter's text) is served for the compression-padding parameter, so the metadata is not merely paraphrased but mismatched to the parameter name. Index 3's AllowedValues is again '0,1,2' instead of '0-2', and index 4's Description is the emulator's 'The time in seconds an InnoDB transaction waits for a row lock before giving up.' instead of the catalog text. Status 200 -> 200.
Why: a description bound to the wrong parameter name actively misinforms: a client (or an operator reading a generated report) tuning compression padding is shown the semantics of the compression level, so a change can be made for the wrong reason. The AllowedValues divergence has the same validator consequence as cause 1.
Fix: the DescribeDBParameters handler's parameter catalog for this family - correct the name-to-Description binding for the InnoDB compression parameters and the AllowedValues range expression.
Traces: rds-db-parameters-pagination#task#5.

=== Cause 3 of 3: same defects on the table-cache / adaptive-flushing / autoextend page ===
AWS: Smithy: as cause 1. On this page AWS returns the catalog text 'The number of seconds after which mysqld server will fetch data from storage engine and replace the data in cache.' for the cached-table-statistics expiry parameter, 'Low water mark representing percentage of redo log capacity at which adaptive flushing is enabled.' for the adaptive-flushing low-water-mark parameter, 'The increment size (in MB) for extending the size of an auto-extending tablespace file when it becomes full' for the autoextend increment, 'Sets the cutoff point at which MySQL begins adding padding within compressed pages to avoid expensive compression failures.' for the compression-padding parameter, 'Timeout in seconds an innodb transaction may wait for a row lock before giving up' for the row-lock timeout, and AllowedValues '0-2' for the three-valued parameter at index 10.
Emulator: every one of those is replaced by emulator prose ('The period of time before cached table statistics expire.', 'Defines the low water mark representing percentage of redo log capacity at which adaptive flushing is enabled.', 'The increment size (in MB) for extending the size of an auto-extending InnoDB system tablespace file when it becomes full.', 'The time in seconds an InnoDB transaction waits for a row lock before giving up.'), the compression-padding parameter again gets the zlib-compression-level text, and AllowedValues at index 10 is '0,1,2' instead of '0-2'. Six field paths diverge (indices 1,2,3,7,10,11). Status 200 -> 200.
Why: this is the same client-visible failure as causes 1 and 2 but on a third page of the same enumeration, so a client that walks the whole group - which is what the paginated contract exists for - diverges on every page, not just one. Fixing one page's strings leaves the others wrong.
Fix: the DescribeDBParameters handler's parameter catalog for the InnoDB/table-cache parameters on this page (Description strings, name-to-Description binding, AllowedValues range expression).
Traces: rds-db-parameters-pagination#task#8.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true; /__admin/health answered 200). All three causes are content divergences on an expected-status response: expected 200, actual 200, verdict mismatch, so no client-visible error is raised. Each cause is its own divergence cluster (affected_count 1 each) and the replay reports stub_path='' and owned_by=[] for all three, i.e. none is covered by an existing issue.

Cause 1 - rds-db-parameters-pagination#task#4, DescribeDBParameters, 200 -> 200 (15 diverging paths under $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters):
  Parameter[2].Description: expected 'Controls how many microseconds the binary log commit waits before synchronizing the binary log file to disk.'; got 'The time in microseconds the server waits before synchronizing transactions to disk.'
  Parameter[3].Description: expected 'How long in microseconds to keep reading transactions from the flush queue before proceeding with the group commit (and syncing the log to disk, if sync_binlog is greater than 0). If the value is 0 (the default), there is no timeout and the server keeps reading new transactions until the queue is empty.'; got 'The time in microseconds the server keeps reading transactions before flushing them to the binary log.'
  Parameter[4].AllowedValues: expected '0-2'; got '0,1,2'
  Parameter[4].Description: expected 'The transaction completion type (0 - default, 1 - chain, 2 - release)'; got 'The transaction completion type.'
  Parameter[5].AllowedValues: expected '0-2'; got '0,1,2'
  Parameter[5].Description: expected 'Allows INSERT and SELECT statements to run concurrently for MyISAM tables that have no free blocks in the middle of the data file.'; got 'If AUTO (the default), MySQL permits INSERT and SELECT statements to run concurrently for MyISAM tables that have no free blocks in the middle of the data file.'
  Parameter[6].Description, Parameter[7].Description, Parameter[10..12].Description, Parameter[14..16].Description and Parameter[12].MinimumEngineVersion diverge in the same way (divergence_key keys list all 15 paths).

Cause 2 - rds-db-parameters-pagination#task#5, DescribeDBParameters, 200 -> 200:
  Parameter[0].Description: expected 'Sets the cutoff point at which MySQL begins adding padding within compressed pages to avoid expensive compression failures.'; got 'Specifies the level of zlib compression to use for InnoDB compressed tables and indexes.' (another parameter's description)
  Parameter[3].AllowedValues: expected '0-2'; got '0,1,2'
  Parameter[4].Description: expected 'Timeout in seconds an innodb transaction may wait for a row lock before giving up'; got 'The time in seconds an InnoDB transaction waits for a row lock before giving up.'

Cause 3 - rds-db-parameters-pagination#task#8, DescribeDBParameters, 200 -> 200:
  Parameter[1].Description: expected 'The number of seconds after which mysqld server will fetch data from storage engine and replace the data in cache.'; got 'The period of time before cached table statistics expire.'
  Parameter[2].Description: expected 'Low water mark representing percentage of redo log capacity at which adaptive flushing is enabled.'; got 'Defines the low water mark representing percentage of redo log capacity at which adaptive flushing is enabled.'
  Parameter[3].Description: expected 'The increment size (in MB) for extending the size of an auto-extending tablespace file when it becomes full'; got 'The increment size (in MB) for extending the size of an auto-extending InnoDB system tablespace file when it becomes full.'
  Parameter[7].Description: expected 'Sets the cutoff point at which MySQL begins adding padding within compressed pages to avoid expensive compression failures.'; got 'Specifies the level of zlib compression to use for InnoDB compressed tables and indexes.'
  Parameter[10].AllowedValues: expected '0-2'; got '0,1,2'
  Parameter[11].Description: expected 'Timeout in seconds an innodb transaction may wait for a row lock before giving up'; got 'The time in seconds an InnoDB transaction waits for a row lock before giving up.'

Model evidence (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json): DescribeDBParameters -> DBParameterGroupDetails{Parameters: ParametersList, Marker}, paginated {inputToken: Marker, outputToken: Marker, items: Parameters, pageSize: MaxRecords}; com.amazonaws.rds#Parameter members Description, AllowedValues ('Specifies the valid range of values for the parameter.') and MinimumEngineVersion ('The earliest engine version to which the parameter can apply.').

## Steps to Reproduce
1. git clone the project repo and build/boot the emulator from this checkout.
2. Replay trace set simparity-wire-aws-v1 against it (replay_wire_corpus).
3. Cause 1: replay fixture rds-db-parameters-pagination step task#4 - DescribeDBParameters paging the MySQL-family DB parameter group. Inspect $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[2..16] and confirm each Description is the engine catalog's text, that the two three-valued parameters at indices 4 and 5 report AllowedValues as the hyphenated range '0-2', and that index 12's MinimumEngineVersion matches the catalog.
4. Cause 2: replay rds-db-parameters-pagination step task#5 - confirm the InnoDB compression-padding parameter at index 0 carries ITS OWN description (the compression-padding cutoff text), not the zlib compression-level text; confirm index 3's AllowedValues is '0-2' and index 4's Description is the catalog's row-lock-timeout text.
5. Cause 3: replay rds-db-parameters-pagination step task#8 - confirm the Descriptions at indices 1, 2, 3, 7 and 11 are the catalog's text (including that index 7, the compression-padding parameter, is not given the zlib compression-level text) and that index 10's AllowedValues is '0-2'.
6. All three steps must replay clean before the issue is closed; a fix to one page's strings does not close the other two.
