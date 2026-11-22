// SPDX-License-Identifier: CC0-1.0

//! WHERE an integration's request names the resource its call acts on, and what the
//! access request over that resource is.
//!
//! One table ([`TARGET_RESOURCES`]), one entry per integration, because which member of
//! the request names the target — and how that member spells it ([`TargetNaming`]) — is
//! that integration's own fact. The reading of it is here too: the resources a request
//! names ([`TargetResource::every_named_in`]) and the ARN each of them is
//! ([`TargetNaming::arn_of`]), which is what the role gate decides a `Task` state's call
//! against ([`super::super::task_authorization`]).
//!
//! Split out of `super`'s file so that file stays inside the project's length limit.

use super::*;

/// Where an integration's request names the resource its call acts ON, which IAM
/// action that call is subject to, and how a refusal of it is reported to the `Task`
/// state.
///
/// A `Task` state's integration call is made AS the state machine's execution role,
/// so it is subject to that role's own policies — an IAM access request over an
/// `(action, resource)` pair. Both halves are the integration's own: the action is the
/// one the invoked operation publishes, which is not always the operation's own name,
/// and which member of the request names the resource, and in what form, is a fact of
/// that integration — which is why there is one entry per integration rather than a
/// rule.
#[derive(Debug)]
pub struct TargetResource {
    /// The `<namespace>:<action>` the OPTIMIZED family names this integration by.
    pub integration: &'static str,
    /// The IAM action this call is subject to, for an operation whose PUBLISHED
    /// permission is not its own name (`lambda:InvokeFunction` for `Invoke`); `None`
    /// where the action is the invoked service's namespace and the operation's name,
    /// which is the usual case ([`access_request`]).
    pub action: Option<&'static str>,
    /// The member of the integration's request that names the resource the call acts
    /// on, named in the integration's own PascalCase; EMPTY for an entry whose naming
    /// reads no member at all, because the action names no resource type
    /// ([`TargetNaming::EveryResource`]).
    ///
    /// A member that sits INSIDE the request rather than at its top level is named by
    /// the path to it, its segments separated by [`MEMBER_PATH_SEPARATOR`]
    /// (`Entries.EventBusName`): each segment is a member of the object reached so far,
    /// and a LIST on the way is walked element by element — because an operation whose
    /// request carries one unit of work per element names a resource in EACH of them, so
    /// such a call is subject to one access request per resource its request names
    /// ([`access_requests`]).
    pub member: &'static str,
    /// The resource that member names when the request OMITS it, for an operation
    /// that documents a default for it; `None` where it documents none, and a request
    /// omitting the member then names no resource at all — which is a request the
    /// invoked operation answers for itself rather than an authorization decision
    /// ([`access_request`]).
    ///
    /// For a member reached inside a LIST the default stands in per ELEMENT, because the
    /// operation documents it for the member rather than for the request.
    pub default: Option<&'static str>,
    /// How that member spells the resource it names ([`TargetNaming`]).
    pub named: TargetNaming,
    /// The error the invoked operation MODELS for a call its caller is not authorized
    /// to make ([`Denied`]); `None` where that operation models none, whose refusal
    /// reaches the state under the invoked service's BASE SDK exception
    /// ([`Api::base_error`]) — the class its client deserializes an unmodelled code
    /// into.
    pub denied: Option<Denied>,
}

/// The error one operation answers a call its caller is not authorized to make with,
/// as that operation's own model declares it.
///
/// A refusal the role gate makes on the invoked service's behalf
/// ([`super::task_authorization`]) is the refusal THAT SERVICE would have answered, so
/// where the operation models an access-denied error of its own the task reports that
/// error's class and the status and code its answer carries — which is what a
/// definition's `Retry` and `Catch` match on. Every field is read from the operation's
/// own `smithy_query(query="operation")`, so this states no wire detail of its own.
#[derive(Debug, Clone, Copy)]
pub struct Denied {
    /// The SDK exception class the error shape is deserialized into, which is the
    /// error half of the name a `Task` state reports ([`Api::task_error`]).
    pub exception: &'static str,
    /// The code the answer names the class by — for an `awsQuery` service the error's
    /// own query code (`awsQueryError.code`), which is what that service's client
    /// matches on.
    pub code: &'static str,
    /// The status that refusal answers with (`httpError`).
    pub status: u16,
}

/// How the member of an integration's request that names the call's target SPELLS it.
///
/// An IAM access request is over the resource's ARN, whatever the operation's own
/// member takes: an operation that addresses its resource by ARN carries the resource
/// itself, while one that addresses it by NAME carries a name that is the ARN's last
/// field, and the ARN it names is this account's own ARN for that resource in the
/// Region the call is made in. Some services address a resource by neither — a queue by
/// its url, a function by any of the forms Lambda documents for one — and there the ARN
/// is read the way that service addresses the resource. Which of these a member is, is
/// a fact of the invoked operation's model, so it is stated per entry rather than
/// guessed from the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetNaming {
    /// The member carries the resource's ARN, so it IS the resource the access
    /// request is over.
    Arn,
    /// The member carries the resource's NAME inside one account and Region, so the
    /// ARN it names is minted from it. A value already written as an ARN is the
    /// resource itself, because such a member is one the invoked operation takes
    /// either way.
    Named {
        /// The ARN namespace of the service the resource belongs to.
        service: &'static str,
        /// What precedes the name in the ARN's resource field, the resource type the
        /// invoked service writes it under (`table/`).
        prefix: &'static str,
    },
    /// The same for a resource of a GLOBAL service, whose ARN carries no Region label at
    /// all (`arn:aws:cloudfront::<account>:distribution/<id>`): the resource one of its
    /// operations names is the same resource whichever Region the call is made in.
    Global {
        /// The ARN namespace of the service the resource belongs to.
        service: &'static str,
        /// What precedes the name in the ARN's resource field.
        prefix: &'static str,
    },
    /// The member carries a queue's `QueueUrl`, whose last path segment is the
    /// `QueueName` the queue's ARN carries
    /// (`arn:<partition>:sqs:<region>:<account>:<QueueName>`) and whose endpoint host
    /// names the Region that ARN is in.
    QueueUrl,
    /// The member carries a `FunctionName` in one of the forms Lambda documents for
    /// it: `my-function[:qualifier]`, the function's ARN, or the partial
    /// `<account>:function:my-function[:qualifier]`
    /// (`smithy_query(query="shape",shape="NamespacedFunctionName",model_path="services/
    /// service-com-amazonaws-lambda-awsgirapiservice/smithy-model/
    /// com-amazonaws-lambda-awsgirapiservice.json")`). Each names the same function,
    /// whose ARN is what `lambda:InvokeFunction` is taken on.
    FunctionName,
    /// NO member carries it: the permission the invoked operation publishes names no
    /// resource TYPE at all, so the action is taken on every resource and the access
    /// request is over `*` ([`EVERY_RESOURCE`]).
    ///
    /// That is what an account-level operation publishes — a listing of what the account
    /// holds is about no one resource — and it is a fact of the invoked action rather
    /// than of the request, which is why such an entry reads nothing out of the request
    /// and carries no member ([`TargetResource::member`]).
    EveryResource,
}

/// The resource an access request is over for an action whose published permission names
/// no resource type: every resource, which is also the only resource such a grant can be
/// written for.
pub const EVERY_RESOURCE: &str = "*";

impl TargetNaming {
    /// The ARN of the resource `named` names, as read in the Region `region` the call
    /// is made in.
    ///
    /// A value already written as an ARN is the resource itself whichever of these a
    /// member is: such a member is one the invoked operation takes either way, and the
    /// resource the access request is over is the same one. `None` for a value that
    /// names no resource of the kind at all — there is nothing to decide an access
    /// request over, which the caller leaves to the invoked operation's own refusal.
    pub(super) fn arn_of(self, named: &str, region: &str) -> Option<String> {
        if named.is_empty() {
            return None;
        }
        if named.starts_with(ARN_PREFIX) {
            return Some(named.to_string());
        }
        match self {
            Self::Arn => Some(named.to_string()),
            Self::Named { service, prefix } => Some(::engine::emulator_arn(
                service,
                region,
                &format!("{prefix}{named}"),
            )),
            // A global service's resource carries no Region label, so the one the call
            // is made in is not part of its ARN.
            Self::Global { service, prefix } => Some(::engine::emulator_arn(
                service,
                "",
                &format!("{prefix}{named}"),
            )),
            // A queue url is `<endpoint>/<account>/<QueueName>`, so the name is its
            // last path segment — and the queue's own Region is the one its endpoint
            // host names (`sqs.<region>.amazonaws.com`), which is not always the
            // Region the call is made in: a task may send to a queue in another one.
            // An endpoint host that names none leaves the call's own Region, which is
            // where a queue addressed through this emulator's own endpoint lives.
            Self::QueueUrl => {
                let name = named.trim_end_matches('/').rsplit('/').next()?;
                (!name.is_empty()).then(|| {
                    let endpoint = endpoint_region(named);
                    ::engine::emulator_arn(
                        SQS_NAMESPACE,
                        endpoint.as_deref().unwrap_or(region),
                        name,
                    )
                })
            }
            // Every partial form writes the function's name last, after the labels
            // the longer ones carry before it; the qualifier a form may close with
            // stays on the ARN, because a grant may be written for one version or
            // alias of a function.
            Self::FunctionName => {
                let tail = named
                    .rsplit_once(FUNCTION_RESOURCE_TYPE_PREFIX)
                    .map_or(named, |(_, tail)| tail);
                (!tail.is_empty()).then(|| {
                    ::engine::emulator_arn(
                        LAMBDA_NAMESPACE,
                        region,
                        &format!("{FUNCTION_RESOURCE_TYPE_PREFIX}{tail}"),
                    )
                })
            }
            // No resource is named, because the action names no resource type: the
            // access request is over every resource, whichever Region the call is made
            // in.
            Self::EveryResource => Some(EVERY_RESOURCE.to_string()),
        }
    }

    /// The Region the call acting on `named` is ADDRESSED to, for an addressing form
    /// that carries the service's own ENDPOINT; `None` for one that does not.
    ///
    /// A queue url is the endpoint the call is made to — "the URL of the Amazon SQS
    /// queue to which a message is sent" is both the queue's name and its address, so
    /// the request goes to the host that url names and is signed for the Region that
    /// host carries, which is how a client reaches a queue of another Region without
    /// changing anything else. Every OTHER form carries only a resource's name or its
    /// ARN, which names no endpoint: such a call is made against the Region the caller
    /// itself is in, and a resource of another Region is then the invoked operation's
    /// own business to refuse.
    pub(super) fn addressed_region(self, named: &str) -> Option<String> {
        match self {
            Self::QueueUrl => endpoint_region(named),
            Self::Arn
            | Self::Named { .. }
            | Self::Global { .. }
            | Self::FunctionName
            | Self::EveryResource => None,
        }
    }
}

/// The Region ONE integration call is addressed to: the Region the resource its
/// request names carries its own endpoint in ([`TargetNaming::addressed_region`]), or
/// `None` for a call whose request addresses its target by a form that names no
/// endpoint — which is made in the Region the caller runs in.
///
/// Read off the invoked operation's own input ([`Call::operation_input`]), exactly as
/// the access request over the same target is ([`access_request`]), so both families of
/// an integration address the same endpoint.
pub fn addressed_region(call: &Call, request: &Value) -> Option<String> {
    let targets = call.targets?;
    let named = targets.named_in(&call.operation_input(request))?;
    targets.named.addressed_region(&named)
}

/// The Region the endpoint host of `url` names, for a url addressed to one of a
/// service's own regional endpoints (`<prefix>.<region>.<suffix>`); `None` for a url
/// whose host names no Region, which is what this emulator's own endpoint does.
///
/// The authority and the bare host come from the ONE shared reading of a url's host
/// (`service_common::data_plane_addressing`), and the label is held to the grammar every
/// model's ARN pattern states for a Region field (`service_common::arn::region_label`),
/// because it ends up in the ARN an access request is decided over. The Region is the
/// SECOND of the host's dot-separated labels — the one an AWS regional endpoint host
/// puts after the service's endpoint prefix — and a host with no label after it carries
/// no Region at all.
pub(super) fn endpoint_region(url: &str) -> Option<String> {
    let authority = ::service_common::data_plane_addressing::addressed_host(url, None)?;
    let host = ::service_common::data_plane_addressing::host_only(&authority).to_string();
    let mut labels = host.split('.');
    let _prefix = labels.next()?;
    let region = labels.next()?;
    let further = labels.next().is_some();
    let shaped = ::service_common::arn::region_label(region).is_some() && region.contains('-');
    (further && shaped).then(|| region.to_string())
}

/// What every ARN opens with, which is what makes a value one rather than one of the
/// shorter forms a service also addresses a resource by.
pub(super) const ARN_PREFIX: &str = "arn:";

/// The resource-type label a function's ARN and the partial forms of a `FunctionName`
/// write before the function's own name.
pub(super) const FUNCTION_RESOURCE_TYPE_PREFIX: &str = "function:";

/// The integrations whose request names the resource their call acts on.
///
/// One entry per integration, because which member names the target — and how that
/// member spells it — is that integration's own fact. An entry also needs the invoked
/// service's base SDK exception, the one a refusal its operation does not model is
/// reported under, so an entry is added from that service's own model and SDK rather
/// than guessed at. An integration with no entry is one no access request is formed
/// for, and the call is made as before.
///
/// An entry is keyed by the `<namespace>:<action>` the OPTIMIZED family writes, and
/// serves BOTH families: the AWS SDK family names the same service and the same
/// action, so it makes the same call on the same resource and is subject to the same
/// access request. Only the SPELLING of the request's members differs between them,
/// which [`TargetResource::resource_of`] reads either way.
///
/// Nothing here is per ACTION beyond the entry: the IAM action an entry's call is
/// subject to is the invoked service's namespace and the operation's own name
/// ([`access_request`]) unless the entry states otherwise, which it does only where
/// the invoked operation publishes its permission under a different name, so two
/// actions of one service usually differ only in which member names their target.
pub const TARGET_RESOURCES: &[TargetResource] = &[
    // `arn:aws:states:::states:startExecution` starts an execution of the state
    // machine its `StateMachineArn` parameter names, which is the resource
    // `states:StartExecution` is taken on
    // (`smithy_query(query="operation",operation="StartExecution")` on this crate's
    // own model: `stateMachineArn` is the ARN "of the state machine to execute").
    // A refusal this operation does not model is reported under this service's own
    // base SDK exception, the one the AWS SDKs deserialize an unmodelled Step
    // Functions error into ([`Api::base_error`]).
    TargetResource {
        integration: "states:startExecution",
        action: None,
        member: "StateMachineArn",
        default: None,
        named: TargetNaming::Arn,
        denied: None,
    },
    // DynamoDB's item-level integrations each act on ONE table, the one their
    // `TableName` parameter names: "The name of the table to contain the item" /
    // "containing the requested item" / "from which to delete the item"
    // (`smithy_query(query="operation",operation="PutItem,GetItem,UpdateItem,
    // DeleteItem",model_path="services/
    // service-com-amazonaws-dynamodb-dynamodb-20120810/smithy-model/
    // com-amazonaws-dynamodb-dynamodb-20120810.json")`). The member is the table's
    // NAME rather than its ARN, and a table's ARN writes that name under the `table`
    // resource type — "The Amazon Resource Name (ARN) that uniquely identifies the
    // table" (`TableDescription$TableArn` of the same model, which that service mints
    // as `table/<name>`) — so the resource the access request is over is minted from
    // it. A refusal none of these operations models is reported under DynamoDB's base
    // SDK exception.
    TargetResource {
        integration: "dynamodb:putItem",
        action: None,
        member: "TableName",
        default: None,
        named: DYNAMODB_TABLE,
        denied: None,
    },
    TargetResource {
        integration: "dynamodb:getItem",
        action: None,
        member: "TableName",
        default: None,
        named: DYNAMODB_TABLE,
        denied: None,
    },
    TargetResource {
        integration: "dynamodb:updateItem",
        action: None,
        member: "TableName",
        default: None,
        named: DYNAMODB_TABLE,
        denied: None,
    },
    TargetResource {
        integration: "dynamodb:deleteItem",
        action: None,
        member: "TableName",
        default: None,
        named: DYNAMODB_TABLE,
        denied: None,
    },
    // `arn:aws:states:::sqs:sendMessage` sends to the queue its `QueueUrl` parameter
    // addresses, which is the queue `sqs:SendMessage` is taken on
    // (`smithy_query(query="operation",operation="SendMessage",model_path="services/
    // service-com-amazonaws-sqs-amazonsqs/smithy-model/
    // com-amazonaws-sqs-amazonsqs.json")`: `QueueUrl` is "the URL of the Amazon SQS
    // queue to which a message is sent"). Amazon SQS models no access-denied error on
    // `SendMessage`, so a refusal of it is reported under that service's base SDK
    // exception.
    TargetResource {
        integration: "sqs:sendMessage",
        action: None,
        member: "QueueUrl",
        default: None,
        named: TargetNaming::QueueUrl,
        denied: None,
    },
    // `arn:aws:states:::sns:publish` publishes to the topic its `TopicArn` parameter
    // names — "The topic you want to publish to" — which is the resource
    // `sns:Publish` is taken on
    // (`smithy_query(query="operation",operation="Publish",model_path="services/
    // service-com-amazonaws-sns-amazonsimplenotificationservice/smithy-model/
    // com-amazonaws-sns-amazonsimplenotificationservice.json")`). Unlike the
    // integrations above, `Publish` MODELS the refusal of an unauthorized call:
    // `AuthorizationErrorException` "indicates that the user has been denied access to
    // the requested resource", answered with HTTP 403 under the query code
    // `AuthorizationError` — so that is the error a denied publish reaches the state
    // as, rather than the service's base SDK exception.
    TargetResource {
        integration: SNS_PUBLISH_INTEGRATION,
        action: None,
        member: "TopicArn",
        // `Publish` documents no default destination: "If you don't specify a value for
        // the TopicArn parameter, you must specify a value for the PhoneNumber or
        // TargetArn parameters", so a request naming none names no topic at all.
        default: None,
        named: TargetNaming::Arn,
        denied: Some(Denied {
            exception: "AuthorizationErrorException",
            code: "AuthorizationError",
            status: 403,
        }),
    },
    // `arn:aws:states:::events:putEvents` publishes each entry of its request onto the
    // event bus THAT ENTRY names: "The name or ARN of the event bus to receive the event.
    // [...] If you omit this, the default event bus is used"
    // (`smithy_query(query="shape",shape="PutEventsRequestEntry",model_path="services/
    // service-com-amazonaws-eventbridge-awsevents/smithy-model/
    // com-amazonaws-eventbridge-awsevents.json")`), which is the resource
    // `events:PutEvents` is taken on. The member sits inside the `Entries` list, so the
    // call is subject to one access request per bus its entries name
    // ([`TargetResource::member`]). It carries the bus's NAME or its ARN, and a bus's ARN
    // writes that name under the `event-bus` resource type —
    // `DescribeEventBusResponse$Arn` of the same model, which that service mints as
    // `event-bus/<name>` — so the resource an access request is over is minted from it
    // ([`EVENT_BUS`]). `PutEvents` models no access-denied error of its own, declaring
    // only `InternalException`
    // (`smithy_query(query="operation",operation="PutEvents"` on the same model), so a
    // refusal of it is reported under Amazon EventBridge's base SDK exception — the class
    // its client deserializes an unmodelled code into.
    TargetResource {
        integration: "events:putEvents",
        action: None,
        member: EVENT_BUS_NAME_MEMBER,
        // "If you omit this, the default event bus is used": the bus every account
        // carries.
        default: Some(DEFAULT_EVENT_BUS),
        named: EVENT_BUS,
        denied: None,
    },
    // `arn:aws:states:::lambda:invoke` invokes the function its `FunctionName`
    // parameter names. The action is `lambda:InvokeFunction` — the permission Lambda
    // publishes for the `Invoke` API, which is not the operation's own name — and the
    // resource is that function's ARN. `Invoke` models no access-denied error either
    // (`smithy_query(query="operation",operation="Invoke",model_path="services/
    // service-com-amazonaws-lambda-awsgirapiservice/smithy-model/
    // com-amazonaws-lambda-awsgirapiservice.json")`), so a refusal of it is reported
    // under Lambda's base SDK exception.
    TargetResource {
        integration: "lambda:invoke",
        action: Some("lambda:InvokeFunction"),
        member: "FunctionName",
        default: None,
        named: TargetNaming::FunctionName,
        denied: None,
    },
    // `arn:aws:states:::athena:startQueryExecution` runs its query IN a workgroup —
    // `WorkGroup` is "the name of the workgroup in which the query is being started"
    // (`smithy_query(query="operation",operation="StartQueryExecution",
    // model_path="services/service-com-amazonaws-athena-amazonathena/smithy-model/
    // com-amazonaws-athena-amazonathena.json")`) — and a query's actions are taken on
    // that workgroup, which Athena writes under the `workgroup` resource type. A
    // request naming none runs in the account's own default workgroup: "if a workgroup
    // is not specified, [...] the primary workgroup"
    // (`ListQueryExecutionsInput$WorkGroup` of the same model), which is the name the
    // access request is then over. Athena models no access-denied error on these
    // operations, so a refusal of one is reported under that service's base SDK
    // exception.
    TargetResource {
        integration: "athena:startQueryExecution",
        action: None,
        member: "WorkGroup",
        default: Some(ATHENA_DEFAULT_WORK_GROUP),
        named: ATHENA_WORK_GROUP,
        denied: None,
    },
    // `arn:aws:states:::glue:startJobRun` runs the job its `JobName` parameter names —
    // "the name of the job definition to use", `@required`
    // (`smithy_query(query="operation",operation="StartJobRun",model_path="services/
    // service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`)
    // — and a run's actions are taken on that job, which Glue writes under the `job`
    // resource type. Glue models no access-denied error on these operations either.
    TargetResource {
        integration: "glue:startJobRun",
        action: None,
        member: "JobName",
        default: None,
        named: GLUE_JOB,
        denied: None,
    },
    // `arn:aws:states:::aws-sdk:backup:startBackupJob` backs a resource up INTO the
    // vault its `BackupVaultName` parameter names — "the name of a logical container
    // where backups are stored"
    // (`smithy_query(query="operation",operation="StartBackupJob",model_path="services/
    // service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/
    // com-amazonaws-backup-cryocontrollerusermanager.json")`) — which is the resource
    // `backup:StartBackupJob` is taken on ("Actions defined by AWS Backup", IAM Service
    // Authorization Reference, whose resource type for it is the backup vault). The
    // member is the vault's NAME rather than its ARN, and a vault's ARN writes that name
    // under the `backup-vault` resource type — "for example,
    // arn:aws:backup:us-east-1:123456789012:backup-vault:aBackupVault"
    // (`CreateBackupVaultOutput$BackupVaultArn` of the same model) — so the resource the
    // access request is over is minted from it. `StartBackupJob` models no
    // access-denied error (that model declares none at all), so a refusal of it is
    // reported under AWS Backup's base SDK exception. The other AWS Backup integrations
    // this emulator calls carry no entry: which resource each of THEM is taken on is not
    // a fact this entry's sources state — the actions AWS publishes for a copy, a
    // restore and the reads name their own resource types — and an access request formed
    // over a guessed one would refuse calls AWS serves, so each is left to the change
    // that grounds it.
    TargetResource {
        integration: "backup:startBackupJob",
        action: None,
        member: "BackupVaultName",
        default: None,
        named: BACKUP_VAULT,
        denied: None,
    },
    // `arn:aws:states:::aws-sdk:cloudfront:createInvalidation` — and the two reads of an
    // invalidation beside it — act on the DISTRIBUTION their `DistributionId` names:
    // "The distribution's id" (`smithy_query(query="operation",operation=
    // "CreateInvalidation,GetInvalidation,ListInvalidations",model_path="services/
    // service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/
    // com-amazonaws-cloudfront-cloudfront2020-05-31.json")`), which is the resource
    // `cloudfront:CreateInvalidation` and its sibling actions are taken on. The member is
    // the distribution's ID rather than its ARN, and that ARN writes the id under the
    // `distribution` resource type with NO Region label, CloudFront being global
    // ([`cloudfront::CLOUDFRONT_DISTRIBUTION`]). Each of these operations MODELS the
    // refusal of an unauthorized call — `AccessDenied`, "Access denied", answered with
    // HTTP 403 — so that is the error a denied call reaches the state as rather than the
    // service's base SDK exception.
    TargetResource {
        integration: "cloudfront:createInvalidation",
        action: None,
        member: cloudfront::DISTRIBUTION_ID_MEMBER,
        default: None,
        named: cloudfront::CLOUDFRONT_DISTRIBUTION,
        denied: Some(CLOUDFRONT_ACCESS_DENIED),
    },
    TargetResource {
        integration: "cloudfront:getInvalidation",
        action: None,
        member: cloudfront::DISTRIBUTION_ID_MEMBER,
        default: None,
        named: cloudfront::CLOUDFRONT_DISTRIBUTION,
        denied: Some(CLOUDFRONT_ACCESS_DENIED),
    },
    TargetResource {
        integration: "cloudfront:listInvalidations",
        action: None,
        member: cloudfront::DISTRIBUTION_ID_MEMBER,
        default: None,
        named: cloudfront::CLOUDFRONT_DISTRIBUTION,
        denied: Some(CLOUDFRONT_ACCESS_DENIED),
    },
    // The ACCOUNT-LEVEL listings this emulator calls: each reports what the account
    // holds rather than acting on one resource, and the permission each publishes names
    // no resource type at all — "Actions defined by AWS Account Management", "…by Amazon
    // EventBridge" and "…by AWS Lambda" (IAM Service Authorization Reference) each list
    // these three with no resource type — so each is taken on every resource
    // ([`TargetNaming::EveryResource`]) and no member of the request names one. A role
    // that may make such a call therefore carries the action over `*`, which is the only
    // resource such a grant can be written for.
    //
    // `ListRegions` MODELS the refusal of an unauthorized call — `AccessDeniedException`,
    // HTTP 403 — so that is the error a denied listing reaches the state as
    // ([`account::ACCOUNT_ACCESS_DENIED`]); the other two model no access error at all,
    // so a refusal of either is reported under its service's base SDK exception
    // ([`Api::base_error`]).
    TargetResource {
        integration: "account:listRegions",
        action: None,
        member: "",
        default: None,
        named: TargetNaming::EveryResource,
        denied: Some(account::ACCOUNT_ACCESS_DENIED),
    },
    TargetResource {
        integration: "events:listApiDestinations",
        action: None,
        member: "",
        default: None,
        named: TargetNaming::EveryResource,
        denied: None,
    },
    TargetResource {
        integration: "lambda:listCodeSigningConfigs",
        action: None,
        member: "",
        default: None,
        named: TargetNaming::EveryResource,
        denied: None,
    },
    // `arn:aws:states:::aws-sdk:rds:stopDBInstance` — and the start and the reboot of a
    // DB instance beside it — act on the INSTANCE their `DBInstanceIdentifier` names,
    // which is the resource `rds:StopDBInstance` and its sibling actions are taken on
    // ([`rds::RDS_DB_INSTANCE`]). Each publishes its permission under its own operation
    // name, so the entry states no action of its own. None of these operations models an
    // access-denied error (`smithy_query(query="operation",operation="StopDBInstance,
    // StartDBInstance,RebootDBInstance",model_path="services/
    // service-com-amazonaws-rds-amazonrdsv19/smithy-model/
    // com-amazonaws-rds-amazonrdsv19.json")` declares only that service's own faults), so
    // a refusal of one carries the code its front door answers such a call with
    // ([`Api::access_denied_code`]) and reaches a `Task` state under Amazon RDS's base SDK
    // exception. The other operations of that service carry no entry: which resource each
    // of THEM is taken on is not a fact these sources state, and an access request formed
    // over a guessed one would refuse calls AWS serves.
    TargetResource {
        integration: rds::STOP_DB_INSTANCE_INTEGRATION,
        action: None,
        member: rds::DB_INSTANCE_IDENTIFIER_MEMBER,
        default: None,
        named: rds::RDS_DB_INSTANCE,
        denied: None,
    },
    TargetResource {
        integration: rds::START_DB_INSTANCE_INTEGRATION,
        action: None,
        member: rds::DB_INSTANCE_IDENTIFIER_MEMBER,
        default: None,
        named: rds::RDS_DB_INSTANCE,
        denied: None,
    },
    TargetResource {
        integration: rds::REBOOT_DB_INSTANCE_INTEGRATION,
        action: None,
        member: rds::DB_INSTANCE_IDENTIFIER_MEMBER,
        default: None,
        named: rds::RDS_DB_INSTANCE,
        denied: None,
    },
];

/// The refusal every CloudFront operation this emulator calls models for a call its
/// caller is not authorized to make: the `AccessDenied` error shape, which that service's
/// SDK deserializes into `AccessDeniedException`, answered with HTTP 403.
pub(super) const CLOUDFRONT_ACCESS_DENIED: Denied = Denied {
    exception: "AccessDeniedException",
    code: "AccessDenied",
    status: TARGET_FORBIDDEN_STATUS,
};

/// An Athena workgroup, named by the `WorkGroup` one of that service's query
/// operations takes.
pub(super) const ATHENA_WORK_GROUP: TargetNaming = TargetNaming::Named {
    service: "athena",
    prefix: "workgroup/",
};

/// The workgroup a query runs in when the request names none: "if a workgroup is not
/// specified, a list of available query execution IDs for the queries in the primary
/// workgroup is returned" (`ListQueryExecutionsInput$WorkGroup`), the default
/// workgroup every account carries.
pub(super) const ATHENA_DEFAULT_WORK_GROUP: &str = "primary";

/// A Glue job, named by the `JobName` one of that service's run operations takes.
pub(super) const GLUE_JOB: TargetNaming = TargetNaming::Named {
    service: "glue",
    prefix: "job/",
};

/// A DynamoDB table, named by the table name one of its item-level operations takes.
pub(super) const DYNAMODB_TABLE: TargetNaming = TargetNaming::Named {
    service: "dynamodb",
    prefix: "table/",
};

/// An Amazon EventBridge event bus, named by the `EventBusName` an entry of a
/// publication carries — the resource a publication onto it is taken on.
pub(super) const EVENT_BUS: TargetNaming = TargetNaming::Named {
    service: "events",
    prefix: "event-bus/",
};

/// The path to that member inside a publication's request: the member of each entry of
/// the `Entries` list ([`TargetResource::member`]).
pub(super) const EVENT_BUS_NAME_MEMBER: &str = "Entries.EventBusName";

/// The bus an entry that names none publishes onto, the one every account carries.
pub(super) const DEFAULT_EVENT_BUS: &str = "default";

/// An AWS Backup vault, named by the `BackupVaultName` one of that service's job
/// operations takes.
pub(super) const BACKUP_VAULT: TargetNaming = TargetNaming::Named {
    service: "backup",
    prefix: "backup-vault:",
};

/// What separates the segments of a PATH to the member that names a call's target, for
/// an entry whose resource is named inside the request rather than at its top level
/// ([`TargetResource::member`]).
pub(super) const MEMBER_PATH_SEPARATOR: char = '.';

impl TargetResource {
    /// How `request` ADDRESSES that resource: the member's own value, and the default
    /// the invoked operation documents for a request that OMITS it.
    ///
    /// The FIRST resource the request names, for a request that names several
    /// ([`TargetResource::every_named_in`]), which is the one a caller about a call over
    /// a single resource asks for.
    ///
    /// Public because the calls a run-a-job pattern makes for ITSELF are over the
    /// resource the STARTING request named, which those calls do not carry
    /// ([`RunAJob::acts_on_named`]).
    pub fn named_in(&self, request: &Value) -> Option<String> {
        self.every_named_in(request).into_iter().next()
    }

    /// EVERY resource `request` addresses, in the order the request writes them: the
    /// member's own value, and the default the invoked operation documents for a request
    /// that OMITS it.
    ///
    /// One value for an entry whose member sits at the top level of the request, and one
    /// per element for an entry whose member is reached inside a LIST
    /// ([`TargetResource::member`]) — a publication carries one event per entry, each
    /// naming the bus that event goes onto, so such a call acts on as many resources as
    /// its request names.
    ///
    /// A member the request carries as something other than text addresses no resource
    /// of any name, and the default does not stand in for it: the request named
    /// something, so what to answer for it is the invoked operation's own refusal
    /// rather than an access request over a resource nothing named.
    ///
    /// Each segment is read ignoring CASE, because the spellings a request may write it
    /// in differ from the model's own in case alone — the integration's own PascalCase,
    /// the invoked model's ([`MemberCase`]), and the spelling an AWS SDK gives a member
    /// whose model name carries an acronym — and each of them reaches the operation
    /// under the model's own name ([`model_member`]).
    #[must_use]
    pub fn every_named_in(&self, request: &Value) -> Vec<String> {
        // An action that names no resource type is taken on every resource, so there is
        // no member to read and nothing in the request can change what it is taken on.
        if self.named == TargetNaming::EveryResource {
            return vec![EVERY_RESOURCE.to_string()];
        }
        let path: Vec<&str> = self.member.split(MEMBER_PATH_SEPARATOR).collect();
        self.named_at(request, &path)
    }

    /// The resources the member path `path` names inside `document`: the leaf's own text
    /// where the path runs out, every element's where a LIST is reached, and the
    /// documented default where the object `path` reads does not carry its last segment.
    ///
    /// The walk descends only along `path`, which the entry fixes, so its depth is that
    /// path's length whatever the document a caller built nests inside itself: the
    /// elements of a list are read one by one ([`TargetResource::member_at`]) rather than
    /// walked into again.
    fn named_at(&self, document: &Value, path: &[&str]) -> Vec<String> {
        let Some((member, rest)) = path.split_first() else {
            // The leaf: a text value names the resource, and anything else names none.
            return document.as_str().map(str::to_string).into_iter().collect();
        };
        // A list carries one unit of work per element, each naming a resource of its own,
        // so the member is read in every one of them.
        if let Some(elements) = document.as_array() {
            return elements
                .iter()
                .flat_map(|element| self.member_at(element, member, rest))
                .collect();
        }
        self.member_at(document, member, rest)
    }

    /// The resources the member `member` of `document`, and the `rest` of the path below
    /// it, name.
    ///
    /// A document carrying no such member — and one that is not an object at all, which
    /// carries no member of any name — names the default the invoked operation documents
    /// for that member, where the member is the one the path ENDS at: that is the request
    /// omitting it. A path that stops short of its last segment reached no such member at
    /// all, so nothing stands in for it.
    fn member_at(&self, document: &Value, member: &str, rest: &[&str]) -> Vec<String> {
        let named = document.as_object().and_then(|members| {
            members
                .iter()
                .find(|(written, _)| written.eq_ignore_ascii_case(member))
                .map(|(_, value)| value)
        });
        match named {
            Some(named) => self.named_at(named, rest),
            None if rest.is_empty() => self.default.map(str::to_string).into_iter().collect(),
            None => Vec::new(),
        }
    }
}
