// SPDX-License-Identifier: CC0-1.0

use super::*;

/// The SDK service NAME the diagnostic below names a called service by, per SIGNING name.
///
/// A resource provider calls the service that owns the type through an AWS SDK, and what it
/// reports when that call is refused is the SDK's exception message: the service's own
/// message followed by the SDK's own diagnostic of the call ([`sdk_diagnostic`]). The
/// service in that diagnostic is the SDK's name for it, which is NOT the signing name the
/// request was signed with — Amazon OpenSearch Service signs `es` and its SDK names it
/// `OpenSearch`, that model's own `sdkId`.
///
/// One entry per service whose reported wording this emulator pins; a service with no entry
/// has its message reported exactly as it worded it, with no diagnostic invented for it.
/// Each entry also carries the SHAPE that service's SDK words the diagnostic in
/// ([`SdkDiagnostic`]), which is the SDK's and cannot be derived from the service.
pub(super) const SDK_SERVICE_NAMES: &[(&str, &str, SdkDiagnostic)] = &[
    // Amazon API Gateway's SDK names it `ApiGateway` — that model's own
    // `awsService.cloudFormationName`, which is also the name its SDK client carries
    // (`smithy_query(query="service_info")` on that service's model: `sdkId: "API Gateway"`,
    // `cloudFormationName: ApiGateway`). The shape is the bare ENVELOPE, with no attempt
    // count after it, as the recorded `ResourceStatusReason` of an
    // `AWS::ApiGateway::Deployment` created for a REST API that has no methods carries it
    // (`"<the service's sentence> (Service: ApiGateway, Status Code: 400, Request ID: …)"`,
    // where Amazon Route 53's below carries `(SDK Attempt Count: …)` past the closing
    // parenthesis). Every type this emulator provisions over this signing name is that
    // service's or API Gateway v2's ([`provisioning::apigateway_rest_api`]).
    ("apigateway", "ApiGateway", SdkDiagnostic::Envelope),
    // AWS AppConfig's SDK names it by that model's own `sdkId`, which is also the
    // `cloudFormationName` the type names of its resources carry
    // (`smithy_query(query="service_info")` on that service's model: `sdkId: "AppConfig"`,
    // `cloudFormationName: AppConfig`). The shape is the bare ENVELOPE, with no attempt count
    // after it, as the recorded `ResourceStatusReason` of an `AWS::AppConfig::Application`
    // whose declared name is longer than that service's maximum carries it (`"<the service's
    // sentence> (Service: AppConfig, Status Code: 400, Request ID: …)"`, where Amazon Route 53's
    // below carries `(SDK Attempt Count: …)` past the closing parenthesis). The four resource
    // types this emulator provisions over this signing name are all that service's
    // ([`provisioning::appconfig_resources`]).
    ("appconfig", "AppConfig", SdkDiagnostic::Envelope),
    // Application Auto Scaling's SDK names it by its `sdkId` without the spaces, which is
    // also the `cloudFormationName` the type names of its resources carry
    // (`smithy_query(query="service_info")`).
    (
        "application-autoscaling",
        "ApplicationAutoScaling",
        SdkDiagnostic::Attempts,
    ),
    // Amazon Braket's SDK names it by that model's own `sdkId` (`Braket`), and words its
    // diagnostic with the refused OPERATION and that service's error code in it, as the
    // recorded `ResourceStatusReason` of a failed `AWS::Braket::SpendingLimit` carries it (in
    // the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `braket__cloudformation.deploy-braket-spendinglimit-simulator-device-rejected`, whose
    // reason reads `Resource handler returned message: "<the service's sentence> (Service:
    // Braket; Operation: CreateSpendingLimit; Status Code: 400; Error Code:
    // ValidationException; Request ID: …)" (RequestToken: …, HandlerErrorCode:
    // InvalidRequest)`).
    ("braket", "Braket", SdkDiagnostic::Operation),
    // Clean Rooms ML's SDK names it `CleanRoomsMl` — that model's own `sdkId` (`CleanRoomsML`,
    // `smithy_query(query="service_info")` on that service's model) written with a lower-case
    // final letter, which is the client name its SDK carries. The shape is the ATTEMPT-counting
    // one, as the recorded `ResourceStatusReason` of an `AWS::CleanRoomsML::TrainingDataset`
    // whose Glue table's Amazon S3 location holds nothing carries it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check
    // `cleanroomsml__cloudformation.deploy-cfn-cleanroomsml-training-dataset-empty-s3-location-rejected`,
    // whose reason reads `<the service's sentence> (Service: CleanRoomsMl, Status Code: 400,
    // Request ID: …) (SDK Attempt Count: …)` with `HandlerErrorCode: InvalidRequest`).
    ("cleanrooms-ml", "CleanRoomsMl", SdkDiagnostic::Attempts),
    // AWS CodePipeline's SDK names it by that model's own `sdkId` (`CodePipeline`,
    // `smithy_query(query="service_info")` on that service's model), which is also the
    // `cloudFormationName` the type names of its resources carry. The shape is the
    // ATTEMPT-counting one, as the recorded `ResourceStatusReason` of an
    // `AWS::CodePipeline::Pipeline` whose declared service role that service cannot assume
    // carries it (in the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `cloudformation__codepipeline.deploy-codepipeline-pipeline-unassumable-role-rolled-back`,
    // whose reason reads `Resource handler returned message: "<the service's sentence>
    // (Service: CodePipeline, Status Code: 400, Request ID: …) (SDK Attempt Count: …)"
    // (RequestToken: …, HandlerErrorCode: GeneralServiceException)`). Every type this
    // emulator provisions over this signing name is that service's
    // (`crate::state::provisioner`'s pipeline, custom-action-type and webhook arms).
    ("codepipeline", "CodePipeline", SdkDiagnostic::Attempts),
    // Amazon EC2's SDK names it by that model's own `sdkId` (`EC2`,
    // `smithy_query(query="service_info")` on that service's model) written as one word with a
    // capital initial, exactly as Amazon RDS's below is, and words the diagnostic with the
    // refused call's error CODE and the PROXY it was made through — as the recorded
    // `ResourceStatusReason` of an `AWS::EC2::LaunchTemplate` whose declared name Amazon EC2
    // already holds a template under carries it (in the behavioral corpus, check
    // `cloudformation__ec2.deploy-ec2-launch-template-duplicate-name-rejected`, whose reason
    // reads `Resource handler returned message: "<the service's sentence> (Service: Ec2;
    // Status Code: 400; Error Code: InvalidLaunchTemplateName.AlreadyExistsException; Request
    // ID: …; Proxy: null)" (RequestToken: …, HandlerErrorCode: AlreadyExists)`). Every type
    // this emulator provisions over this signing name is that service's.
    ("ec2", "Ec2", SdkDiagnostic::Proxied),
    // Elastic Load Balancing v2's SDK names it `ElasticLoadBalancingV2` — that model's own
    // `awsService.cloudFormationName`, which is also the name its SDK client carries
    // (`smithy_query(query="service_info")`: `sdkId: "Elastic Load Balancing v2"`,
    // `cloudFormationName: ElasticLoadBalancingV2`). The four resource types this emulator
    // provisions over this signing name are all that service's
    // ([`provisioning::elastic_load_balancing`]), and the shape is the bare ENVELOPE, with no
    // attempt count after it, as the recorded `ResourceStatusReason` of an
    // `AWS::ElasticLoadBalancingV2::ListenerRule` whose priority the listener already
    // evaluates a rule at carries it (`"<the service's sentence> (Service:
    // ElasticLoadBalancingV2, Status Code: 400, Request ID: …)"`, where Amazon Route 53's
    // below carries `(SDK Attempt Count: …)` past the closing parenthesis).
    (
        "elasticloadbalancing",
        "ElasticLoadBalancingV2",
        SdkDiagnostic::Envelope,
    ),
    ("es", "OpenSearch", SdkDiagnostic::Attempts),
    ("iam", "Iam", SdkDiagnostic::Attempts),
    ("lambda", "Lambda", SdkDiagnostic::Attempts),
    // Amazon CloudWatch Logs's SDK names it by that model's own `sdkId` (`CloudWatch Logs`,
    // `smithy_query(query="service_info")` on that service's model) written as one word,
    // exactly as Application Auto Scaling's above is. Its diagnostic is worded for a refusal
    // of the REQUEST and for no other class ([`SdkDiagnostic::EnvelopeOnRefusedRequest`]),
    // which is what the recorded `ResourceStatusReason` of each class of failure of its types
    // reports: an `AWS::Logs::MetricFilter` whose declared pattern that service cannot read,
    // and an `AWS::Logs::SubscriptionFilter` whose declared Lambda destination may not be
    // invoked, each carry `"… (Service: CloudWatchLogs, Status Code: 400, Request ID: …)"`
    // (in the behavioral corpus `simparity-behavioral-aws-v1`, checks
    // `cloudformation__lambda__logs.cfn-logs-subscription-filter-missing-permission-rejected-at-create`
    // among them), where a log group whose name is already taken and a filter whose declared
    // group is not there carry that service's own sentence alone. The four resource types
    // this emulator provisions over this signing name are all that service's
    // ([`provisioning::logs_resources`]).
    (
        "logs",
        "CloudWatchLogs",
        SdkDiagnostic::EnvelopeOnRefusedRequest,
    ),
    // Neptune Analytics' SDK names it `NeptuneGraph` — that model's own `sdkId`
    // (`Neptune Graph`) written as one word, exactly as Application Auto Scaling's above is —
    // and words the ATTEMPT-counting shape, as the recorded `ResourceStatusReason` of an
    // `AWS::NeptuneGraph::Graph` whose declared provisioned memory is below that service's
    // minimum carries it (in the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `cloudformation__ec2__neptune-graph.deploy-neptune-graph-provisioned-memory-out-of-range-rejected`,
    // whose reason reads `Resource handler returned message: "<the service's sentence>
    // (Service: NeptuneGraph, Status Code: 400, Request ID: …) (SDK Attempt Count: …)"
    // (RequestToken: …, HandlerErrorCode: InvalidRequest)`).
    ("neptune-graph", "NeptuneGraph", SdkDiagnostic::Attempts),
    // Amazon RDS's SDK names it by that model's own `sdkId` (`RDS`) written as one word with
    // a capital initial, and words the bare ENVELOPE — the service, the status and the call's
    // id, with nothing after it — as the recorded `ResourceStatusReason` of an
    // `AWS::RDS::DBSubnetGroup` whose subnets cover too few Availability Zones carries it (in
    // the behavioral corpus, check
    // `cloudformation__ec2__rds.cfn-rds-db-subnet-group-single-az-rejected-negative`, whose
    // reason reads `Resource handler returned message: "<the service's sentence> (Service:
    // Rds, Status Code: 400, Request ID: …)" (RequestToken: …, HandlerErrorCode:
    // InvalidRequest)`).
    ("rds", "Rds", SdkDiagnostic::Envelope),
    // Recycle Bin's SDK names it `Rbin` — that model's own `sdkId` (`rbin`) with the initial
    // capitalised, exactly as Amazon RDS's above is — and words the ATTEMPT-counting shape, as
    // the recorded `ResourceStatusReason` of an `AWS::Rbin::Rule` whose retention period is
    // longer than the resource type it retains supports carries it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check
    // `cloudformation__rbin.deploy-rbin-rule-retention-period-over-one-year-rejected`, whose
    // reason reads `Resource handler returned message: "<the service's sentence> (Service:
    // Rbin, Status Code: 400, Request ID: …) (SDK Attempt Count: …)" (RequestToken: …,
    // HandlerErrorCode: InvalidRequest)`).
    ("rbin", "Rbin", SdkDiagnostic::Attempts),
    // Amazon Route 53's SDK names it by its `sdkId` without the space, exactly as
    // Application Auto Scaling's does (`smithy_query(query="service_info")`: `sdkId:
    // "Route 53"`). The shape is the ATTEMPT-counting one, as the recorded
    // `ResourceStatusReason` of an `AWS::Route53::RecordSet` whose declared zone Route 53
    // does not hold carries it (`(Service: Route53, Status Code: 404, Request ID: …) (SDK
    // Attempt Count: …)`).
    ("route53", "Route53", SdkDiagnostic::Attempts),
    ("s3", "S3", SdkDiagnostic::Attempts),
    // AWS Cloud Map's SDK names it by that model's own `sdkId` (`ServiceDiscovery`,
    // `smithy_query(query="service_info")` on that service's model), which is also the
    // `cloudFormationName` the type names of its resources carry. The shape is the
    // ATTEMPT-counting one, as Amazon Route 53's above is — the two services' resource handlers
    // are the same generation — and the five resource types this emulator provisions over this
    // signing name are all that service's ([`provisioning::service_discovery`]).
    (
        "servicediscovery",
        "ServiceDiscovery",
        SdkDiagnostic::Attempts,
    ),
];

/// The SHAPE of the parenthesis an SDK appends to the message of the exception it raises for
/// a refused call.
///
/// Each names the service and the HTTP status; what differs is what else they carry and how
/// the parts are separated. Which of them a called service's SDK words is that SDK's, and it
/// is not derivable from the service, so it is RECORDED per service in
/// [`SDK_SERVICE_NAMES`] — each entry citing where that shape was read from — and a service
/// with no entry has no diagnostic invented for it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SdkDiagnostic {
    /// `(Service: <service>, Status Code: <status>, Request ID: <id>) (SDK Attempt Count:
    /// <n>)` — comma-separated, with the call's id (or ids) and how many attempts the SDK
    /// made at it.
    Attempts,
    /// `(Service: <service>; Operation: <operation>; Status Code: <status>; Error Code:
    /// <code>; Request ID: <id>)` — semicolon-separated, naming the operation that was
    /// refused and the error code it was refused with in place of the attempt count.
    Operation,
    /// `(Service: <service>, Status Code: <status>, Request ID: <id>)` — the same
    /// comma-separated envelope [`SdkDiagnostic::Attempts`] opens with, and NOTHING after it:
    /// the SDK of such a service words no attempt count at all.
    Envelope,
    /// [`SdkDiagnostic::Envelope`] for a refusal of the REQUEST, and NOTHING AT ALL for one of
    /// any other class ([`handler_error_code`]): the providers of such a service's types pass
    /// the SDK's own exception on for the invalid-request class — where the diagnostic is part
    /// of the message that exception carries — and report every other class as the owning
    /// service's own sentence alone.
    EnvelopeOnRefusedRequest,
    /// `(Service: <service>; Status Code: <status>; Error Code: <code>; Request ID: <id>;
    /// Proxy: null)` — semicolon-separated like [`SdkDiagnostic::Operation`], naming the error
    /// code the call was refused with but not the call, and closing with the PROXY the SDK made
    /// the request through: `null` for the direct call this emulator's in-process dispatch is.
    Proxied,
}

/// The services whose answers carry an EXTENDED request id beside the request id, which
/// their SDK states in the diagnostic too.
///
/// Amazon S3 is the service that answers one: every S3 response carries `x-amz-id-2` beside
/// `x-amz-request-id`, and the exception an S3 SDK raises states both ("Request ID: ...,
/// Extended Request ID: ..."). A service with no entry answers one id, and its diagnostic
/// states that one.
pub(super) const SDK_EXTENDED_REQUEST_ID_SERVICES: &[&str] = &["s3"];

/// How many characters the request id of a service that answers an extended one carries, and
/// the case it is written in: Amazon S3's `x-amz-request-id` is a short opaque token in
/// upper case, where the request id of the services above is a uuid
/// ([`crate::state::new_request_id`]).
pub(super) const SDK_SHORT_REQUEST_ID_CHARACTERS: usize = 16;

/// How many characters the EXTENDED request id carries: a long opaque token, which is what
/// `x-amz-id-2` is.
pub(super) const SDK_EXTENDED_REQUEST_ID_CHARACTERS: usize = 64;

/// The request id — and the extended request id where the service answers one — that the
/// diagnostic states for a call to `signing_name`.
///
/// Both are this call's own, minted per call as every identifier this service hands out is:
/// the in-process answer carries none of its own, and they identify the CALL rather than
/// anything about the resource.
pub(super) fn sdk_request_ids(signing_name: &str) -> String {
    if !SDK_EXTENDED_REQUEST_ID_SERVICES.contains(&signing_name) {
        return format!("Request ID: {}", crate::state::new_request_id());
    }
    format!(
        "Request ID: {}, Extended Request ID: {}",
        crate::state::new_identifier_digits("sdk-request", SDK_SHORT_REQUEST_ID_CHARACTERS)
            .to_uppercase(),
        crate::state::new_identifier_digits(
            "sdk-extended-request",
            SDK_EXTENDED_REQUEST_ID_CHARACTERS
        ),
    )
}

/// The diagnostic an AWS SDK appends to the message of the exception it raises for a refused
/// call: which service refused it, the HTTP status it answered with, the id (or ids) of the
/// call and how many attempts the SDK made at it.
///
/// `None` for a service [`SDK_SERVICE_NAMES`] does not name, which leaves the message as
/// that service worded it — for a diagnostic that NAMES the operation when the caller
/// named none, since an operation is not something to invent, and for a service whose
/// providers report one only on a refusal of the REQUEST
/// ([`SdkDiagnostic::EnvelopeOnRefusedRequest`]) when the refusal falls in another class.
///
/// `operation` is the called operation as its own model names it, which only a caller that
/// knows which operation it is making can state ([`call_rest_json_named`]).
pub(super) fn sdk_diagnostic(
    signing_name: &str,
    operation: Option<&str>,
    status: u16,
    code: &str,
) -> Option<String> {
    let (service, shape) = SDK_SERVICE_NAMES
        .iter()
        .find(|(name, _, _)| *name == signing_name)
        .map(|(_, service, shape)| (*service, *shape))?;
    // The envelope the two comma-separated shapes open with, written once so the two cannot
    // state the service, the status and the call's id differently.
    let envelope = || {
        format!(
            "(Service: {service}, Status Code: {status}, {})",
            sdk_request_ids(signing_name),
        )
    };
    match shape {
        SdkDiagnostic::Attempts => Some(format!(
            "{} (SDK Attempt Count: {})",
            envelope(),
            sdk_attempt_count(code),
        )),
        SdkDiagnostic::Operation => operation.map(|operation| {
            format!(
                "(Service: {service}; Operation: {operation}; Status Code: {status}; \
                 Error Code: {code}; {})",
                sdk_request_ids(signing_name),
            )
        }),
        SdkDiagnostic::Envelope => Some(envelope()),
        SdkDiagnostic::EnvelopeOnRefusedRequest => {
            (handler_error_code(code) == HANDLER_ERROR_INVALID_REQUEST).then(envelope)
        }
        SdkDiagnostic::Proxied => Some(format!(
            "(Service: {service}; Status Code: {status}; Error Code: {code}; {}; \
             Proxy: {SDK_DIRECT_CALL_PROXY})",
            sdk_request_ids(signing_name),
        )),
    }
}

/// The PROXY an SDK that words one states for a call it made DIRECTLY, with no proxy
/// configured — which is every call this emulator's in-process dispatch makes
/// ([`in_process_calls::dispatch`]).
pub(super) const SDK_DIRECT_CALL_PROXY: &str = "null";

/// How many attempts an AWS SDK makes at a call whose refusal it classifies as worth
/// retrying: its default maximum, reached because every attempt was refused the same way.
pub(super) const SDK_RETRIED_ATTEMPTS: u32 = 3;

/// How many it makes at a refusal it does not retry: the one call it was asked for.
pub(super) const SDK_SINGLE_ATTEMPT: u32 = 1;

/// The classes of refusal an SDK retries, as [`HANDLER_ERROR_CLASSES`] names them: a
/// throttled call, an exhausted service limit and a service that failed are conditions that
/// can clear on their own, where a refusal of the REQUEST cannot and is raised on the first
/// attempt.
pub(super) const SDK_RETRIED_CLASSES: [&str; 3] =
    ["Throttling", "ServiceLimitExceeded", "GeneralServiceException"];

/// The attempt count the diagnostic reports for a refusal carrying `code`, from the class
/// that code falls in ([`handler_error_code`]) — never from the message, which a request can
/// influence.
pub(super) fn sdk_attempt_count(code: &str) -> u32 {
    if SDK_RETRIED_CLASSES.contains(&handler_error_code(code)) {
        SDK_RETRIED_ATTEMPTS
    } else {
        SDK_SINGLE_ATTEMPT
    }
}

/// One refused answer's message as the SDK of the called service raises it: the message that
/// service worded, then that SDK's diagnostic of the call where this crate words one
/// ([`sdk_diagnostic`]).
pub(crate) fn sdk_message(signing_name: &str, status: u16, code: &str, message: &str) -> String {
    sdk_message_of(signing_name, None, status, code, message)
}

/// [`sdk_message`] for a caller that knows WHICH operation of that service it called, which
/// is part of the diagnostic some SDKs word ([`SdkDiagnostic::Operation`]).
pub(crate) fn sdk_message_of(
    signing_name: &str,
    operation: Option<&str>,
    status: u16,
    code: &str,
    message: &str,
) -> String {
    match sdk_diagnostic(signing_name, operation, status, code) {
        Some(diagnostic) if message.is_empty() => diagnostic,
        Some(diagnostic) => format!("{message} {diagnostic}"),
        None => message.to_string(),
    }
}

/// Each `HandlerErrorCode` a provider's refusal is reported under, with the fragments an
/// owning service's own error CODE carries when the refusal falls in that class.
///
/// The classes are the model's own `HandlerErrorCode` values
/// ([`crate::ops::types::HANDLER_ERROR_CODE_VALUES`], which the tests below hold every
/// class here to), and that is what a client reads to decide whether a failed resource is
/// worth retrying — a throttled create is, a malformed declaration never is. The class is
/// decided by the error CODE the owning service answered, the value a client of that
/// service branches on, and never by message text a request can influence.
///
/// Walked in order, so a code carrying more than one fragment takes the first: the narrower
/// class comes before the wider one it reads like (`RequestLimitExceeded` is throttling, not
/// an exhausted quota).
pub(super) const HANDLER_ERROR_CLASSES: &[(&str, &[&str])] = &[
    (
        "AccessDenied",
        &["AccessDenied", "Forbidden", "Unauthorized", "AuthorizationError"],
    ),
    (
        "InvalidCredentials",
        &["InvalidClientTokenId", "SignatureDoesNotMatch", "ExpiredToken"],
    ),
    (
        "Throttling",
        &["Throttl", "TooManyRequests", "RequestLimitExceeded", "SlowDown"],
    ),
    (
        "ServiceLimitExceeded",
        &["LimitExceeded", "QuotaExceeded", "TooManyEntries"],
    ),
    (
        HANDLER_ERROR_ALREADY_EXISTS,
        &["AlreadyExists", "AlreadyOwnedBy", "Duplicate"],
    ),
    ("NotFound", &["NotFound", "NoSuch", "DoesNotExist"]),
    (
        "ResourceConflict",
        &["Conflict", "InUse", "InvalidState", "Concurrent"],
    ),
    (
        "InternalFailure",
        &["InternalFailure", "SerializationException"],
    ),
    (
        "GeneralServiceException",
        &[
            "InternalError",
            "InternalServerError",
            "ServiceUnavailable",
            "ServiceFailure",
            // A call the owning service would not carry out because the resource it manages
            // still HOLDS content (Amazon S3's `BucketNotEmpty` for a bucket that still has
            // objects in it): the declaration is well formed and the provider asked for
            // exactly what the stack said, so what it passes on is the called service's own
            // exception rather than a fault it found in the request — and, like every other
            // class here, a condition that clears on its own once that content is gone,
            // which is why the SDK retried the call ([`SDK_RETRIED_CLASSES`]).
            "NotEmpty",
        ],
    ),
];

/// The class a refusal falls in when nothing above matches: the owning service refused the
/// request the declaration asked for, which is what every validation refusal is.
///
/// Read by [`crate::state::provisioner::Provisioner::handler_message`] too: it is the class whose
/// own wording some providers carry inside the message.
pub(crate) const HANDLER_ERROR_INVALID_REQUEST: &str = "InvalidRequest";

/// The class a refusal that the resource's NAME is already in use falls in — the one class
/// CloudFormation's own record of the account and Region can say more about than the service
/// that answered it ([`already_managed_reason`]).
pub(super) const HANDLER_ERROR_ALREADY_EXISTS: &str = "AlreadyExists";

/// The lowest HTTP status that is the owning SERVICE failing rather than the request being
/// wrong, for an answer that carried no error code at all ([`UNNAMED_CODE_PREFIX`]).
pub(super) const SERVER_ERROR_STATUS: u16 = 500;

/// The class an answer that named no error at all falls in when its status says the owning
/// service failed rather than that the request was wrong.
///
/// Read by [`crate::state::provisioner::Provisioner::handler_error_class`] too: it is the
/// class a provider that maps none of the owning service's own error codes reports a refusal
/// under, whatever the code itself reads like.
pub(crate) const HANDLER_ERROR_SERVICE_FAILED: &str = "GeneralServiceException";

/// The `HandlerErrorCode` an owning service's refusal is reported under — see
/// [`HANDLER_ERROR_CLASSES`].
///
/// Read by the per-type arms too, for a provider whose own wording of a refusal is the wording
/// of ONE class's exception: the class decides whether that wording applies, so the arm asks
/// the same question of the same code this does
/// ([`crate::state::provisioner::call_firewall_association`]).
pub(in crate::state) fn handler_error_code(code: &str) -> &'static str {
    if let Some(status) = code.strip_prefix(UNNAMED_CODE_PREFIX) {
        // An answer that named no error at all: only the status it came back with says
        // whether the service failed or the request was refused.
        return match status.parse::<u16>() {
            Ok(status) if status >= SERVER_ERROR_STATUS => HANDLER_ERROR_SERVICE_FAILED,
            _ => HANDLER_ERROR_INVALID_REQUEST,
        };
    }
    HANDLER_ERROR_CLASSES
        .iter()
        .find(|(_, fragments)| fragments.iter().any(|fragment| code.contains(fragment)))
        .map_or(HANDLER_ERROR_INVALID_REQUEST, |(class, _)| *class)
}

/// The `ResourceStatusReason` the event for a resource its provider refused to act on
/// carries: "Success/failure message associated with the resource".
///
/// CloudFormation does not report its own words for such a failure — it reports the
/// provider's, quoted, beside the token of the resource request it answered and the
/// `HandlerErrorCode` class that refusal falls in ([`handler_error_code`]), so a client
/// reads WHY the resource failed and whether the failure is of a kind worth retrying.
///
/// The token is this resource request's own, minted per request as every identifier this
/// service hands out is.
///
/// `owner` is the provisioner of the type whose resource failed, because the message inside
/// the quotes is the type's HANDLER's
/// ([`crate::state::provisioner::Provisioner::handler_message`]): for a type whose provider words a
/// refusal through the class it put it in, that wording is part of what CloudFormation
/// quotes. A type this emulator has no provisioner for reports the refusal as it came.
///
/// `identifier` is the resource the refusal is about, as its owning service knows it: a
/// provider that names the resource in its own report of a refusal — a bucket whose name is
/// already taken ([`s3_bucket::handler_message`]) — states it from here, so the event says
/// WHICH resource the refusal was about.
///
/// A type CloudFormation provisions ITSELF has no provider answer to quote, and its failure is
/// therefore reported in CloudFormation's own words with no envelope around them
/// ([`own_failure_reason`]).
pub(in crate::state) fn handler_failure_reason(owner: Option<Owner>, refusal: &Refusal, identifier: &str) -> String {
    // A type CloudFormation provisions itself: no provider answered for the resource, so there
    // is nothing to quote and the refusal stands as it reads.
    if owner.is_some_and(|owner| !owner.reported_by_a_resource_provider()) {
        return own_failure_reason(refusal);
    }
    let message = stated_message(refusal);
    let class = match owner {
        Some(owner) => owner.handler_error_class(&refusal.code, handler_error_code(&refusal.code)),
        None => handler_error_code(&refusal.code),
    };
    let message = match owner {
        // The bucket provider's own wording, which lives beside this algorithm for the
        // reason [`s3_bucket`] records.
        Some(Owner::Builtin(Provisioner::S3Bucket)) => {
            s3_bucket::handler_message(class, identifier, message)
        }
        Some(provisioner) => provisioner.handler_message(class, message),
        None => message.to_string(),
    };
    format!(
        "Resource handler returned message: \"{message}\" (RequestToken: {}, \
         HandlerErrorCode: {class})",
        crate::state::new_request_id(),
    )
}

/// What a refusal STATES: the message it worded, or — for a service that worded none at all
/// but still named the error — the code it named, which is then all there is to report.
fn stated_message(refusal: &Refusal) -> &str {
    if refusal.message.is_empty() {
        refusal.code.as_str()
    } else {
        refusal.message.as_str()
    }
}

/// The `ResourceStatusReason` the event for a failed resource of a type CLOUDFORMATION
/// PROVISIONS ITSELF carries: the refusal exactly as it stands.
///
/// The envelope [`handler_failure_reason`] builds is CloudFormation QUOTING a resource
/// provider — the registry extension of the service that owns the type, whose answer
/// CloudFormation reports beside the token of the resource request it answered and the class
/// that answer falls in. A type CloudFormation deploys itself has no such provider: a nested
/// stack is created by CloudFormation's own `CreateStack` ([`nested_stack`]), so what fails
/// that resource is CloudFormation's own account of why the stack was not created and is
/// reported as it reads — with no quoting, no request token and no handler class, none of
/// which any provider supplied.
///
/// Only [`handler_failure_reason`] decides this, from the type whose resource failed
/// ([`Owner::reported_by_a_resource_provider`]), so the question is asked once wherever a
/// refusal is recorded.
fn own_failure_reason(refusal: &Refusal) -> String {
    stated_message(refusal).to_string()
}

/// CLOUDFORMATION's own reason for a create the owning service refused because the name is
/// already in use AND a live stack of this Region is what manages the resource holding it, or
/// `None` where the refusal is not that.
///
/// The service that owns a type arbitrates its own namespace, and a name already in use is
/// what it answers the `AlreadyExists` class for ([`handler_error_code`]) — but CloudFormation
/// knows something that service cannot: WHICH STACK manages the resource that holds the name.
/// Where the holder is one of this Region's stacks, the resource's event therefore carries
/// CloudFormation's own sentence naming that stack
/// ([`crate::state::physical_name::already_exists_in_stack`]) instead of the owning service's report
/// about its own namespace, which is the actionable half of the failure. A name no stack of
/// the Region manages is nothing CloudFormation can add to — a name another account holds, or
/// one a client took outside any stack — so that service's own message stands.
///
/// Asked at the refusal rather than ahead of the create for the reason
/// [`crate::state::physical_name`] records: a create the owning service ACCEPTS under a name one of
/// this Region's stacks holds is a deployment that got the resource it declared, and nothing
/// of CloudFormation's turns it into a failure.
pub(super) fn already_managed_reason(
    resource: &StoredResource,
    stack: &StackProvenance,
    refusal: &Refusal,
) -> Option<String> {
    if handler_error_code(&refusal.code) != HANDLER_ERROR_ALREADY_EXISTS {
        return None;
    }
    crate::state::physical_name::taken_by_another_stack(
        resource,
        &stack.managed_names,
        &stack.stack_id,
    )
        .map(|held| {
            crate::state::physical_name::already_exists_in_stack(
                &held.physical_resource_id,
                &held.stack_id,
            )
        })
}

/// The refusal the stack's SERVICE ROLE answers a call of `operation` on `signing_name`
/// with, or `None` where nothing refuses it — so a call the role's policies deny is never
/// made ([`crate::state::service_role`]).
///
/// The refusal is CLOUDFORMATION's, not the owning service's: the call was not made, so
/// there is no answer of that service's to report. Its code is the one an unauthorized call
/// is classified under ([`handler_error_code`]'s `AccessDenied` class, which is what tells a
/// client whether the failed resource is worth retrying) rather than the particular spelling
/// the called service would have used, and its message is the sentence AWS words such a
/// refusal with, naming the principal, the action and the resource. `operation` is `None` for
/// a wire this cannot read an operation name off, which is judged by nothing.
pub(super) fn denied_by_the_service_role(signing_name: &str, operation: Option<&str>) -> Option<Refusal> {
    let message = crate::state::service_role::refusal(signing_name, operation?)?;
    Some(Refusal {
        code: crate::state::service_role::ACCESS_DENIED.to_string(),
        message,
    })
}
