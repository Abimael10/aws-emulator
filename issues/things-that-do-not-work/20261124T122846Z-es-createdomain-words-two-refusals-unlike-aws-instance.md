Issue-Source: trace-sweep-divergence
Issue-Trace: eserr-create-domain-ultrawarm-as-data-node-invalid-type
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eserr-create-domain-ultrawarm-as-data-node-invalid-type esval-ebs-gp3-iops-below-floor esval-ebs-gp3-throughput-below-floor esval-range-snapshot-start-hour-25
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|CreateDomain|4xx->4xx|$.message
Issue-Commit: 5834ad3c3ba75550acae8591eeb4264fb69879c4

# es CreateDomain words two refusals unlike AWS: instance type echoed in the wrong vocabulary, snapshot-hour message generic (2 causes)

> _Note: this issue was identified against the code base at commit `5834ad3c3ba75550acae8591eeb4264fb69879c4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
This bundle carries TWO net-new causes, both CreateDomain refusals on the legacy Elasticsearch Service (service token es) endpoint. In both the HTTP status is right and only the error message diverges; AWS's message text is a requirement. A fixer must close both before submitting.

Shared fault: the es CreateDomain refusal path does not word its validation messages as AWS does - it re-writes the value it echoes into the emulator's own vocabulary in one case, and substitutes a generic schema-constraint sentence for AWS's specific message in the other.

Cause 1 of 2 - unsupported instance type (fixture eserr-create-domain-ultrawarm-as-data-node-invalid-type, step task#2): the refusal echoes the submitted instance type with the OpenSearch '.search' suffix instead of the '.elasticsearch' suffix the request used.
AWS: Smithy com.amazonaws.elasticsearchservice#InvalidTypeException{message: ErrorMessage}, errorKind client, httpError 409, documented "An exception for trying to create or access sub-resource that is either invalid or not supported. Gives http status code of 409"; the es API's instance types are the ESPartitionInstanceType vocabulary, whose values end in '.elasticsearch'. Recorded behavior: the message is the fixed text 'Invalid instance type: ' followed by the instance type EXACTLY as the request supplied it - so a request naming an UltraWarm type as a data node is refused 409 with 'Invalid instance type: ultrawarm1.medium.elasticsearch'.
Emulator: the status (409) and error class are right, but the echoed value is normalized into the OpenSearch suffix: 'Invalid instance type: ultrawarm1.medium.search'.
Why: the message names an instance type the caller never submitted and that does not exist in the es API's vocabulary, so a client (or a human) that parses the offending value out of the message to correct its request substitutes a value the es endpoint will reject again; the same normalization is evidence the request's instance type is rewritten before validation, which is how an es-vocabulary request can be matched against OpenSearch-vocabulary types at all.
Fix: the CreateDomain validation path for the es endpoint - echo the request's instance type verbatim in the InvalidTypeException message rather than the internally normalized form. Fixed text: 'Invalid instance type: '; variable part: the instance type string taken from the request.
Traces: eserr-create-domain-ultrawarm-as-data-node-invalid-type#task#2 (same cluster also covers esval-ebs-gp3-iops-below-floor#task#1 and esval-ebs-gp3-throughput-below-floor#task#1).

Cause 2 of 2 - out-of-range automated snapshot hour (fixture esval-range-snapshot-start-hour-25, step task#1): the refusal is AWS's 400 ValidationException but with a generic schema-constraint sentence instead of AWS's snapshot-hour message.
AWS: Smithy com.amazonaws.elasticsearchservice#SnapshotOptions{AutomatedSnapshotStartHour: IntegerClass} carries NO @range trait (nor does com.amazonaws.opensearch#SnapshotOptions), so the 'Member must have value less than or equal to 23' form is not model-derived; the refusal is ValidationException{message}, errorKind client, httpError 400, documented "An exception for missing / invalid input fields. Gives http status code of 400". Recorded behavior: AWS refuses an hour outside 0..23 with the message 'AutomatedSnapshotStartHour specified, <value>, is not a valid hour of the day. Please specify a number between 0 and 23.' - fixed text 'AutomatedSnapshotStartHour specified, ' / ', is not a valid hour of the day. Please specify a number between 0 and 23.' around the submitted value.
Emulator: status 400 is right, but the message is the generic constraint report "1 validation error detected: Value '<value>' at 'snapshotOptions.automatedSnapshotStartHour' failed to satisfy constraint: Member must have value less than or equal to 23".
Why: a client that matches on AWS's message (or surfaces it to an operator) does not recognize the refusal, and the emulator asserts a modelled range constraint that does not exist in the model - the same generic path will mis-word every other hand-written range check routed through it.
Fix: the CreateDomain input validation for SnapshotOptions.AutomatedSnapshotStartHour - raise ValidationException with AWS's snapshot-hour sentence instead of the generic schema-constraint formatter.
Traces: esval-range-snapshot-start-hour-25#task#1.

No open issue owns either cluster (the replay reports owned_by empty for both).

## Evidence
Replayed corpus simparity-wire-aws-v1 (emulator_up=true). Both clusters: verdict mismatch with expected status == actual status, divergence confined to $.message.

Cause 1 - fixture eserr-create-domain-ultrawarm-as-data-node-invalid-type, step task#2, service es, operation CreateDomain, 409 -> 409:
  $.message: value_mismatch (expected 'Invalid instance type: ultrawarm1.medium.elasticsearch', got 'Invalid instance type: ultrawarm1.medium.search')
  Cluster also covers esval-ebs-gp3-iops-below-floor#task#1 and esval-ebs-gp3-throughput-below-floor#task#1.
  Requirement: fixed text 'Invalid instance type: ' + the instance type as the request supplied it (here the es-vocabulary 'ultrawarm1.medium.elasticsearch'); 'ultrawarm1.medium' is this fixture's chosen type, not part of the template.

Cause 2 - fixture esval-range-snapshot-start-hour-25, step task#1, service es, operation CreateDomain, 400 -> 400:
  $.message: value_mismatch (expected 'AutomatedSnapshotStartHour specified, 25, is not a valid hour of the day. Please specify a number between 0 and 23.', got "1 validation error detected: Value '25' at 'snapshotOptions.automatedSnapshotStartHour' failed to satisfy constraint: Member must have value less than or equal to 23")
  Requirement: fixed text 'AutomatedSnapshotStartHour specified, ' and ', is not a valid hour of the day. Please specify a number between 0 and 23.'; the only variable part is the submitted hour (25 in this recording).

Smithy cited: com.amazonaws.elasticsearchservice#InvalidTypeException (client, httpError 409); com.amazonaws.elasticsearchservice#ValidationException (client, httpError 400); com.amazonaws.elasticsearchservice#SnapshotOptions.AutomatedSnapshotStartHour -> IntegerClass with no @range trait; com.amazonaws.opensearch#SnapshotOptions.AutomatedSnapshotStartHour -> IntegerClass, likewise unconstrained.

## Steps to Reproduce
1. git_clone, then replay_wire_corpus against the emulator built from the checkout.
2. Cause 1: replay fixture eserr-create-domain-ultrawarm-as-data-node-invalid-type step task#2 - a CreateDomain on the es endpoint whose ElasticsearchClusterConfig names an UltraWarm type (ultrawarm1.medium.elasticsearch) as the data-node InstanceType. Expect 409 with message 'Invalid instance type: ultrawarm1.medium.elasticsearch'; the emulator echoes '...medium.search'.
3. Cause 2: replay fixture esval-range-snapshot-start-hour-25 step task#1 - a CreateDomain with SnapshotOptions.AutomatedSnapshotStartHour = 25. Expect 400 ValidationException with 'AutomatedSnapshotStartHour specified, 25, is not a valid hour of the day. Please specify a number between 0 and 23.'; the emulator returns the generic '1 validation error detected: ...' sentence.
4. Both steps must pass against the recorded responses.
