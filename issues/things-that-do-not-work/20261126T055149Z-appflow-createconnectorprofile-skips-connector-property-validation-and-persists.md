Issue-Source: trace-sweep-divergence
Issue-Trace: appflow-connector-profile-lifecycle
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appflow-connector-profile-lifecycle
Issue-Shape-Id: com.amazonaws.appflow#SandstoneConfigurationServiceLambda
Issue-Key: appflow|CreateConnectorProfile|4xx->2xx|$.message
Issue-Key: appflow|CreateConnectorProfile|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: appflow|CreateConnectorProfile|4xx->2xx|status
Issue-Key: appflow|DescribeConnectorProfiles|2xx->2xx|$.connectorProfileDetails[*]
Issue-Commit: bb631773176252a429a8a518c5447d51faf32ae8

# AppFlow CreateConnectorProfile skips connector-property validation and persists invalid profile

> _Note: this issue was identified against the code base at commit `bb631773176252a429a8a518c5447d51faf32ae8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConnectorProfile

## Description
CreateConnectorProfile performs no validation of the connector-specific profile properties it is handed, so a request AWS rejects outright is accepted with 200 and the rejected profile is then persisted in state and served by subsequent reads. The shared fault is the missing connector-property validation step in the create path; the second cause is the state that the missing rejection leaves behind. 2 causes.

Cause 1 of 2: CreateConnectorProfile returns 200 for a ServiceNow instanceUrl AWS rejects with ValidationException.
AWS: Smithy: CreateConnectorProfile (POST /create-connector-profile, success code 200) requires connectorProfileName, connectorType, connectionMode and connectorProfileConfig, and models ValidationException @httpError 400, doc "The request has invalid or missing parameters.", carrying a single `message` member; the error type is reported in the x-amzn-errortype header. The ServiceNow branch of the config is ServiceNowConnectorProfileProperties { instanceUrl: @required, @length 0..256, @pattern ^\S+$ }. The model's pattern alone does not reject the recorded value, so recorded behavior only for the rule: AWS additionally validates the instance URL and refuses one it cannot accept, answering 400 ValidationException with the FIXED text "Invalid inputs: " + <offending value> + " has an invalid character for " + <memberName> (recorded: "Invalid inputs: <instanceUrl> has an invalid character for instanceUrl"). The VARIABLE parts are the rejected value, taken verbatim from the request, and the member name of the property that failed; the probe's URL in the recording is a per-run value, not part of the template. Hypothesis: the validated condition is the URL's host form (AWS requires a resolvable ServiceNow instance host rather than an arbitrary string).
Emulator: no validation at all — it answers 200 with a CreateConnectorProfileResponse carrying a connectorProfileArn. No status 400, no $.message, and no x-amzn-errortype header at all.
Why: a client relying on AWS's behavior treats create as a validation gate: a bad instance URL surfaces immediately as a 400 ValidationException naming the offending member. Against the emulator the call succeeds, so misconfiguration is never reported, error-handling branches are never exercised, and the caller proceeds to build flows on a profile AWS would never have created.
Fix: the appflow CreateConnectorProfile input-validation layer — the connectorProfileConfig / connectorProfileProperties validation for each connector type (here the ServiceNow instanceUrl check), which must raise ValidationException(400) with the "Invalid inputs: ... has an invalid character for <member>" message before any state is written.
Traces: appflow-connector-profile-lifecycle#task#1.

Cause 2 of 2: the profile AWS refused to create is persisted and returned by DescribeConnectorProfiles.
AWS: Smithy: DescribeConnectorProfiles (POST /describe-connector-profiles) returns connectorProfileDetails, a list of the profiles that exist in the account, and models only InternalServerException(500) and ValidationException(400). Because the create in cause 1 is rejected with 400, no profile exists, and AWS returns the list with NO entry for that connectorProfileName (recorded: the list has no element at index 0 for this probe).
Emulator: returns 200 with connectorProfileDetails[0] fully populated for the profile that should not exist — connectorProfileName and connectorProfileArn for the rejected name, connectorType Servicenow, connectionMode Public, a credentialsArn, connectorProfileProperties.ServiceNow.instanceUrl echoing the invalid URL, and createdAt/lastUpdatedAt. (The ARNs, secret id and timestamps are per-run values; the defect is the presence of the entry, not its particular field values.)
Why: the account's connector-profile inventory diverges from AWS's. A client that lists profiles to decide whether to create one, to reconcile desired state, or to assert that a failed create left nothing behind sees a resource AWS does not have; create-if-absent logic skips the create, and cleanup paths try to delete something the real service never made. This is the reason cause 1 matters beyond the status code: the missing rejection is not transient, it leaks into state.
Fix: the appflow CreateConnectorProfile handler/state write — validation must run and fail BEFORE the profile is inserted into the connector-profile store, so DescribeConnectorProfiles has nothing to return. No change to the DescribeConnectorProfiles serializer itself is implied.
Traces: appflow-connector-profile-lifecycle#task#2.

## Evidence
Cause 1 — fixture appflow-connector-profile-lifecycle, step task#1, CreateConnectorProfile: expected status 400, actual 200. $.message missing: expected "Invalid inputs: <instanceUrl> has an invalid character for instanceUrl", actual None. Header x-amzn-errortype missing: expected an error type (shape not asserted), actual absent. Divergence key service=appflow|CreateConnectorProfile|status=400->200|keys=$.message,$headers.x-amzn-errortype,status.

Cause 2 — fixture appflow-connector-profile-lifecycle, step task#2, DescribeConnectorProfiles: expected status 200, actual 200, but $.connectorProfileDetails[0] is extra — expected None (empty list: the create was rejected so no profile exists), actual an entry for the rejected profile {connectorProfileName: <probe name>, connectorProfileArn: arn:aws:appflow:<region>:<account>:connectorprofile/<name>, connectorType: "Servicenow", connectionMode: "Public", credentialsArn: <secretsmanager arn>, connectorProfileProperties.ServiceNow.instanceUrl: <the invalid URL from the request>, createdAt/lastUpdatedAt: <timestamps>}. Divergence key service=appflow|DescribeConnectorProfiles|status=200->200|keys=$.connectorProfileDetails[0].

Model basis: Smithy CreateConnectorProfile errors include ValidationException(400, "The request has invalid or missing parameters."); ServiceNowConnectorProfileProperties.instanceUrl is @required with @length 0..256 and @pattern ^\S+$; DescribeConnectorProfiles.output.connectorProfileDetails is the account's profile list.

## Steps to Reproduce
1. Boot the emulator and replay the wire corpus (replay_wire_corpus).
2. Cause 1: POST /create-connector-profile with connectorType "Servicenow", connectionMode "Public", a valid connectorProfileName, and connectorProfileConfig.connectorProfileProperties.ServiceNow.instanceUrl set to a URL AWS refuses (the recording uses a non-resolvable https host). Observe HTTP 200 with a connectorProfileArn instead of AWS's 400 ValidationException (x-amzn-errortype set, message "Invalid inputs: <instanceUrl> has an invalid character for instanceUrl"). Fixture appflow-connector-profile-lifecycle, step task#1.
3. Cause 2: immediately POST /describe-connector-profiles for that account. Observe connectorProfileDetails contains an entry for the profile from step 2, where AWS returns no such entry because the create was rejected. Fixture appflow-connector-profile-lifecycle, step task#2.
