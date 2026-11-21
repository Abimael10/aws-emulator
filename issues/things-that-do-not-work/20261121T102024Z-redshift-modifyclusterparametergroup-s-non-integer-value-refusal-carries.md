Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift.parameter-group-known-parameter-value-validation
Issue-Shape-Id: com.amazonaws.redshift#RedshiftServiceVersion20121201
Issue-Key: redshift.parameter-group-known-parameter-value-validation|run#21|AssertionError|assert message == "invalid integer value:soon", message
Issue-Commit: 40ba441ab2a060754ad5b4fdc7b4ab9b12b6aabc

# Redshift ModifyClusterParameterGroup's non-integer value refusal carries an emulator-authored message ('parameter value is not a valid integer:<v>') instead of AWS's 'invalid integer value:<v>'

> _Note: this issue was identified against the code base at commit `40ba441ab2a060754ad5b4fdc7b4ab9b12b6aabc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyClusterParameterGroup

## Description
One net-new root cause in the redshift (awsQuery) control plane, on the parameter-value validation path of ModifyClusterParameterGroup. It is a refusal-MESSAGE divergence: the HTTP status and the error code already match AWS, so code that branches only on Error.Code sees nothing, and the divergence surfaces only in the text clients display, snapshot-test or pattern-match.

Cause 1 of 1: a known integer-typed parameter given a non-numeric value is refused with the right code but the emulator's own sentence instead of AWS's per-class template.
Failing check: redshift.parameter-group-known-parameter-value-validation

AWS: ModifyClusterParameterGroup(ParameterGroupName=<a redshift-1.0 group>, Parameters=[{ParameterName:'statement_timeout', ParameterValue:<a value that is not an integer>]) is refused with HTTP 400 and the service-common awsQuery code InvalidParameterValue, message `invalid integer value:<rejected value>` — the fixed text of this error class is the lowercase `invalid integer value:` with no space after the colon and no trailing period; the single variable part is the rejected value, copied verbatim from the request's ParameterValue. The message does not name the parameter, does not name the member's position in the query serialization and does not describe the expected domain. Smithy (com.amazonaws.redshift#RedshiftServiceVersion20121201, ModifyClusterParameterGroup) models only ClusterParameterGroupNotFoundFault (awsQueryError ClusterParameterGroupNotFound, httpError 404) and InvalidClusterParameterGroupStateFault (awsQueryError InvalidClusterParameterGroupState, httpError 400), so the value-domain check is the service-common InvalidParameterValue and this pairing is recorded behavior only; the recording shows code InvalidParameterValue with exactly the text above. Note that the sibling classes on the same path already match: the boolean class renders `parameter value is not a valid boolean:<v>` and the wlm_json_configuration class renders `Invalid JSON for wlm_json_configuration`, both of which now pass in this check — so AWS does NOT use one shared template here, it uses a distinct fixed text per value class, and the integer class is the one still wrong.

Emulator: the same code InvalidParameterValue (correct) and the same HTTP 400, but the message `parameter value is not a valid integer:<rejected value>` — i.e. the boolean class's fixed text with the word 'integer' substituted for 'boolean'. Different fixed text from AWS's integer-class message, which is `invalid integer value:` + the value.

Why: the refusal message is part of the contract. A client that distinguishes a type-parse failure from an allowed-value or out-of-range failure by matching AWS's wording (CLI transcript tests, error-mapping layers, log scrapers, IaC drift handlers) does not recognize this sentence, and a test suite pinned to AWS's text fails against the emulator while the code comparison passes. It also mis-teaches that Redshift uses one generic 'parameter value is not a valid <type>' family, when the integer class has its own wording.

Fix: the redshift ModifyClusterParameterGroup parameter-value validation and the serializer that renders its InvalidParameterValue — the integer-typed value check must render AWS's own fixed text for that class (`invalid integer value:` followed by the request's ParameterValue) rather than reusing the boolean class's template with the type name swapped. The open issue 20261112T223632Z-redshift-2-net-new-refusal-message-causes-modifyclusterparametergroup.md describes the wlm_json_configuration class of the same validation path (now fixed and passing) and explicitly records that this check's integer leg was never reached and therefore unverified; it does not carry this check's current replay key, so the integer class is filed here and that issue is named only for context. Because this check stops at this assertion, its remaining legs are still unverified rather than passing: that every rejected modify leaves the stored value and Source='engine-default' untouched, that a mixed valid+invalid modify is rejected wholesale with the out-of-range message `The statement_timeout of <v> is outside of the acceptable range of 0,100-2147483647` and applies neither parameter, that a legal multi-parameter modify reports a ParameterGroupStatus and flips Source to 'user', and that ResetClusterParameterGroup(ResetAllParameters=True) restores the defaults.

Traces: redshift.parameter-group-known-parameter-value-validation.

## Evidence
Replay of simparity-behavioral-aws-v1; check redshift.parameter-group-known-parameter-value-validation, verdict=mismatch, listed under the service heading `### redshift`. The suite did not mark this failure ALREADY FILED or OWNED.

Setup (all passed): CreateClusterParameterGroup(ParameterGroupFamily='redshift-1.0'); DescribeClusterParameters exposes require_ssl (DataType 'boolean', Source 'engine-default'), wlm_json_configuration and statement_timeout (DataType 'integer'); ModifyClusterParameterGroup(Parameters=[{require_ssl:'maybe'}]) is refused InvalidParameterValue with the expected 'parameter value is not a valid boolean:maybe'; ModifyClusterParameterGroup(Parameters=[{wlm_json_configuration:'{not-valid-json'}]) is refused InvalidParameterValue with the expected 'Invalid JSON for wlm_json_configuration'.

Failing call: ModifyClusterParameterGroup(ParameterGroupName=<group>, Parameters=[{ParameterName:'statement_timeout', ParameterValue:'soon'}]).
HTTP status: 400 — matches AWS.
Error.Code: 'InvalidParameterValue' — matches AWS (the check's code assertion passed).
Error.Message expected: 'invalid integer value:soon'
Error.Message actual:   'parameter value is not a valid integer:soon'
Replay assertion: AssertionError at check_redshift_parameter_group_known_parameter_value_validation.py line 155 — `assert message == "invalid integer value:soon"`, reported as `AssertionError: parameter value is not a valid integer:soon`.

Requirement decomposition: fixed text = 'invalid integer value:' (lowercase, no space after the colon, no trailing period). Variable part = the rejected value, taken verbatim from the request's Parameters[n].ParameterValue. 'soon' and the generated parameter-group name are this run's values filling the template, not part of it; the group name does not appear in the message at all.

## Steps to Reproduce
1. redshift CreateClusterParameterGroup(ParameterGroupName=<new>, ParameterGroupFamily='redshift-1.0', Description=<any>).
2. DescribeClusterParameters(ParameterGroupName=<same>) and confirm statement_timeout is present with DataType 'integer' and Source 'engine-default'.
3. ModifyClusterParameterGroup(ParameterGroupName=<same>, Parameters=[{ParameterName:'statement_timeout', ParameterValue:'soon'}]).
4. Read the refusal: expect HTTP 400, Error.Code 'InvalidParameterValue', Error.Message exactly 'invalid integer value:soon' (substitute any non-integer value and the message must echo that value after the colon).
5. Re-read DescribeClusterParameters(ParameterGroupName=<same>): statement_timeout must still hold its engine default with Source='engine-default'.
Replay: redshift.parameter-group-known-parameter-value-validation.
