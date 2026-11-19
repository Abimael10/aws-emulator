// SPDX-License-Identifier: CC0-1.0

//! The wire facts of every service a `Task` state's integration call is made to, as one
//! table ([`APIS`]): the facts of those services' own models, which this crate cannot
//! read, so each entry states where it was read from.
//!
//! Kept in a file of its own because it is one concern and one list: the readings over it
//! — which family named a service, which operation of it is called, and how a request to
//! it is built — are the parent module's ([`super::read`], [`super::wired`]).

use super::*;

/// The services a `Task` state's integration call is made to, and how a request to
/// each is bound to HTTP ([`Wire`]) — so the document the state built IS the
/// operation's input, wherever that protocol puts it.
///
/// One entry per SERVICE, never per action: for an AWS JSON service the action of a
/// `Resource` names the operation, so every operation it models is reachable. A REST
/// service binds each operation's members to the path, the query, the headers and one
/// payload separately, so its entry lists the operations whose bindings are stated and
/// [`read`] makes no call for any other.
///
/// It is a LIST rather than a rule because each entry's wire facts are that
/// service's own model's, which this crate cannot read; a service whose model is
/// mounted here but has no entry yet is one no call is made for, and [`read`] says
/// exactly that rather than claiming anything about its protocol.
pub const APIS: &[Api] = &[
    // `services/service-com-amazonaws-athena-amazonathena/smithy-model/
    // com-amazonaws-athena-amazonathena.json`: awsJson1_1, sigv4Name `athena`,
    // endpointPrefix `athena`, sdkId `Athena`, service shape `AmazonAthena`.
    Api {
        namespace: "athena",
        action_prefix: None,
        signing_name: "athena",
        endpoint_prefix: "athena",
        wire: Wire::Json {
            target_prefix: "AmazonAthena",
            version: "1.1",
        },
        sdk_id: "Athena",
        error_prefix: "Athena",
        // The AWS SDK names the service `Athena`, which its own `Athena` client carries.
        sdk_error_prefix: "Athena",
        // The client class `AmazonAthenaClient`, whose base exception is
        // `AmazonAthenaException` — the model's service shape `AmazonAthena`.
        sdk_client: "AmazonAthena",
        sdk_exception: "AmazonAthenaException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `StartQueryExecution`'s input: `QueryString`, `WorkGroup`.
        member_case: MemberCase::Pascal,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/
    // com-amazonaws-backup-cryocontrollerusermanager.json`: restJson1, sigv4Name
    // `backup`, endpointPrefix `backup`, arnNamespace `backup`, sdkId `Backup` — a
    // REST protocol, so each operation's own bindings are what a request to it is
    // built from ([`BACKUP_REST`]).
    Api {
        namespace: "backup",
        action_prefix: None,
        signing_name: "backup",
        endpoint_prefix: "backup",
        wire: Wire::Rest(BACKUP_REST),
        sdk_id: "Backup",
        error_prefix: "Backup",
        // The AWS SDK names the service `Backup`, which its own `Backup` client carries.
        sdk_error_prefix: "Backup",
        // The client class `AWSBackupClient`, whose base exception is
        // `AWSBackupException`; the model's service shape is the service's internal
        // name (`CryoControllerUserManager`), which the SDK's own names do not carry.
        sdk_client: "AWSBackup",
        sdk_exception: "AWSBackupException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `StartBackupJob`'s input: `BackupVaultName`, `ResourceArn`, `IamRoleArn`.
        member_case: MemberCase::Pascal,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-dynamodb-dynamodb-20120810/smithy-model/
    // com-amazonaws-dynamodb-dynamodb-20120810.json`: awsJson1_0, sigv4Name
    // `dynamodb`, endpointPrefix `dynamodb`, sdkId `DynamoDB`, service shape
    // `DynamoDB_20120810`.
    Api {
        namespace: "dynamodb",
        action_prefix: None,
        signing_name: "dynamodb",
        endpoint_prefix: "dynamodb",
        wire: Wire::Json {
            target_prefix: "DynamoDB_20120810",
            version: "1.0",
        },
        sdk_id: "DynamoDB",
        error_prefix: "DynamoDB",
        // The AWS SDK names the service `DynamoDb`, which its own `DynamoDb` client
        // carries — the one of these two spellings of the id that is not the other.
        sdk_error_prefix: "DynamoDb",
        // The one client whose names follow neither the model's service shape
        // (`DynamoDB_20120810`) nor its SDK id: the client is `AmazonDynamoDBv2` and
        // its base exception `AmazonDynamoDBException`, which is how a DynamoDB
        // refusal already reports itself to a `Task` state.
        sdk_client: "AmazonDynamoDBv2",
        sdk_exception: "AmazonDynamoDBException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `PutItem`'s input: `TableName`, `Item`.
        member_case: MemberCase::Pascal,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-sqs-amazonsqs/smithy-model/
    // com-amazonaws-sqs-amazonsqs.json`: awsJson1_0, sigv4Name `sqs`,
    // endpointPrefix `sqs`, sdkId `SQS`, service shape `AmazonSQS`.
    Api {
        namespace: SQS_NAMESPACE,
        action_prefix: None,
        signing_name: "sqs",
        endpoint_prefix: "sqs",
        wire: Wire::Json {
            target_prefix: "AmazonSQS",
            version: "1.0",
        },
        sdk_id: "SQS",
        error_prefix: "SQS",
        // The AWS SDK names the service `Sqs`, which its own `Sqs` client carries.
        sdk_error_prefix: "Sqs",
        // The client class `AmazonSQSClient`, whose base exception is
        // `AmazonSQSException` — the model's service shape `AmazonSQS`.
        sdk_client: "AmazonSQS",
        sdk_exception: "AmazonSQSException",
        access_denied_status: TARGET_FORBIDDEN_STATUS,
        // Amazon SQS's front door answers an unauthorized call `AccessDenied` rather than
        // the AWS-common code, and names the action it refused in the lower case its own
        // authorization layer reports (`sqs:sendmessage`), which is what a client parsing
        // the relayed cause reads.
        access_denied_code: "AccessDenied",
        lowercases_refused_action: true,
        // `SendMessage`'s input: `QueueUrl`, `MessageBody`.
        member_case: MemberCase::Pascal,
        // Amazon SQS is the one service here whose model carries
        // `aws.protocols#awsQueryCompatible`, the modifier that keeps its errors'
        // legacy query codes (`AWS.SimpleQueueService.NonExistentQueue`) beside their
        // shapes.
        query_compatible: true,
    },
    // `services/service-com-amazonaws-sns-amazonsimplenotificationservice/smithy-model/
    // com-amazonaws-sns-amazonsimplenotificationservice.json`: awsQuery, sigv4Name
    // `sns`, endpointPrefix `sns`, arnNamespace `sns`, sdkId `SNS`, version
    // `2010-03-31` — a FORM protocol, so the operation's input goes on the wire as form
    // fields beside the action it names ([`Wire::Query`]).
    Api {
        namespace: SNS_NAMESPACE,
        // Amazon SNS publishes its actions as `SNS:<ApiName>` ("Actions defined by
        // Amazon Simple Notification Service", IAM Service Authorization Reference),
        // which is the ONE spelling this tree holds for them — the same one this
        // emulator's SNS service words its own refusals with — so a refused publish
        // names `SNS:Publish` rather than the ARN namespace.
        action_prefix: Some("SNS"),
        signing_name: SNS_NAMESPACE,
        endpoint_prefix: SNS_NAMESPACE,
        wire: Wire::Query {
            version: "2010-03-31",
            exceptions: SNS_QUERY_EXCEPTIONS,
        },
        sdk_id: "SNS",
        error_prefix: "SNS",
        // The AWS SDK names the service `Sns`, which its own `Sns` client carries.
        sdk_error_prefix: "Sns",
        // The client class `AmazonSNSClient`, whose base exception is
        // `AmazonSNSException`; the model's service shape is the service's full name
        // (`AmazonSimpleNotificationService`), which the SDK's own names do not carry.
        sdk_client: "AmazonSNS",
        sdk_exception: "AmazonSNSException",
        access_denied_status: TARGET_FORBIDDEN_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `Publish`'s input: `TopicArn`, `Message`, `MessageAttributes`.
        member_case: MemberCase::Pascal,
        // Its model declares the `awsQuery` protocol itself rather than the
        // compatibility modifier, so an error of it is named by its own query code and
        // there is no second name to ask for.
        query_compatible: false,
    },
    // `services/service-com-amazonaws-eventbridge-awsevents/smithy-model/
    // com-amazonaws-eventbridge-awsevents.json`: awsJson1_1, sigv4Name `events`,
    // endpointPrefix `events`, sdkId `EventBridge`, service shape `AWSEvents`.
    Api {
        namespace: "events",
        action_prefix: None,
        signing_name: "events",
        endpoint_prefix: "events",
        wire: Wire::Json {
            target_prefix: "AWSEvents",
            version: "1.1",
        },
        sdk_id: "EventBridge",
        error_prefix: "EventBridge",
        // The AWS SDK names the service `EventBridge`, which its own `EventBridge`
        // client carries.
        sdk_error_prefix: "EventBridge",
        // The client the events API is reached through is
        // `AmazonCloudWatchEventsClient`, whose base exception is
        // `AmazonCloudWatchEventsException`.
        sdk_client: "AmazonCloudWatchEvents",
        sdk_exception: "AmazonCloudWatchEventsException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `PutEvents`' input: `Entries`, `EndpointId`.
        member_case: MemberCase::Pascal,
        query_compatible: false,
    },
    // This service itself, for a task that starts another execution:
    // `services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/
    // com-amazonaws-sfn-awsstepfunctions.json`: awsJson1_0, sigv4Name `states`,
    // endpointPrefix `states`, sdkId `SFN`, service shape `AWSStepFunctions`.
    Api {
        namespace: STATES_NAMESPACE,
        action_prefix: None,
        signing_name: STATES_NAMESPACE,
        endpoint_prefix: STATES_NAMESPACE,
        wire: Wire::Json {
            target_prefix: "AWSStepFunctions",
            version: "1.0",
        },
        sdk_id: "SFN",
        // A failed `states:startExecution` task is reported under the service's own
        // name rather than under the `SFN` id its SDK carries
        // (`StepFunctions.AWSStepFunctionsException` for a refusal it does not model).
        error_prefix: "StepFunctions",
        // The AWS SDK names the service by its id, `Sfn`, which its own `Sfn` client
        // carries — the family's own token for it is that id lower-cased.
        sdk_error_prefix: "Sfn",
        // The client class `AWSStepFunctionsClient`, whose base exception is
        // `AWSStepFunctionsException` — the model's service shape `AWSStepFunctions`.
        sdk_client: "AWSStepFunctions",
        sdk_exception: "AWSStepFunctionsException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `StartExecution`'s input: `stateMachineArn`, `name`, `input`.
        member_case: MemberCase::LowerCamel,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-kinesis-kinesis-20131202/smithy-model/
    // com-amazonaws-kinesis-kinesis-20131202.json`: awsJson1_1, sigv4Name
    // `kinesis`, endpointPrefix `kinesis`, sdkId `Kinesis`, service shape
    // `Kinesis_20131202`.
    Api {
        namespace: "kinesis",
        action_prefix: None,
        signing_name: "kinesis",
        endpoint_prefix: "kinesis",
        wire: Wire::Json {
            target_prefix: "Kinesis_20131202",
            version: "1.1",
        },
        sdk_id: "Kinesis",
        error_prefix: "Kinesis",
        // The AWS SDK names the service `Kinesis`, which its own `Kinesis` client
        // carries.
        sdk_error_prefix: "Kinesis",
        // The client class `AmazonKinesisClient`, whose base exception is
        // `AmazonKinesisException`.
        sdk_client: "AmazonKinesis",
        sdk_exception: "AmazonKinesisException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `PutRecord`'s input: `StreamName`, `Data`, `PartitionKey`.
        member_case: MemberCase::Pascal,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-ssm-amazonssm/smithy-model/
    // com-amazonaws-ssm-amazonssm.json`: awsJson1_1, sigv4Name `ssm`,
    // endpointPrefix `ssm`, sdkId `SSM`, service shape `AmazonSSM`.
    Api {
        namespace: "ssm",
        action_prefix: None,
        signing_name: "ssm",
        endpoint_prefix: "ssm",
        wire: Wire::Json {
            target_prefix: "AmazonSSM",
            version: "1.1",
        },
        sdk_id: "SSM",
        error_prefix: "SSM",
        // The AWS SDK names the service `Ssm`, which its own `Ssm` client carries.
        sdk_error_prefix: "Ssm",
        // The client class `AmazonSSMClient`, whose base exception is
        // `AmazonSSMException` — the model's service shape `AmazonSSM`.
        sdk_client: "AmazonSSM",
        sdk_exception: "AmazonSSMException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `GetParameter`'s input: `Name`, `WithDecryption`.
        member_case: MemberCase::Pascal,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/
    // smithy-model/com-amazonaws-ecs-amazonec2containerservicev20141113.json`:
    // awsJson1_1, sigv4Name `ecs`, endpointPrefix `ecs`, sdkId `ECS`, service shape
    // `AmazonEC2ContainerServiceV20141113`.
    Api {
        namespace: "ecs",
        action_prefix: None,
        signing_name: "ecs",
        endpoint_prefix: "ecs",
        wire: Wire::Json {
            target_prefix: "AmazonEC2ContainerServiceV20141113",
            version: "1.1",
        },
        sdk_id: "ECS",
        error_prefix: "ECS",
        // The AWS SDK names the service `Ecs`, which its own `Ecs` client carries.
        sdk_error_prefix: "Ecs",
        // The client class `AmazonECSClient`, whose base exception is
        // `AmazonECSException`; the model's service shape carries the API's date
        // (`AmazonEC2ContainerServiceV20141113`), which the SDK's own names do not.
        sdk_client: "AmazonECS",
        sdk_exception: "AmazonECSException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `RunTask`'s input: `cluster`, `taskDefinition`, `overrides`.
        member_case: MemberCase::LowerCamel,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-glue-awsglue/smithy-model/
    // com-amazonaws-glue-awsglue.json`: awsJson1_1, sigv4Name `glue`, endpointPrefix
    // `glue`, sdkId `Glue`, service shape `AWSGlue`.
    Api {
        namespace: "glue",
        action_prefix: None,
        signing_name: "glue",
        endpoint_prefix: "glue",
        wire: Wire::Json {
            target_prefix: "AWSGlue",
            version: "1.1",
        },
        sdk_id: "Glue",
        error_prefix: "Glue",
        // The AWS SDK names the service `Glue`, which its own `Glue` client carries.
        sdk_error_prefix: "Glue",
        // The client class `AWSGlueClient`, whose base exception is
        // `AWSGlueException` — the model's service shape `AWSGlue`.
        sdk_client: "AWSGlue",
        sdk_exception: "AWSGlueException",
        access_denied_status: TARGET_CLIENT_ERROR_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `StartJobRun`'s input: `JobName`, `Arguments`, `Timeout`.
        member_case: MemberCase::Pascal,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/
    // com-amazonaws-lambda-awsgirapiservice.json`: restJson1, sigv4Name `lambda`,
    // endpointPrefix `lambda`, sdkId `Lambda` — a REST protocol, so each operation's
    // own bindings are what a request to it is built from ([`LAMBDA_REST`]).
    Api {
        namespace: LAMBDA_NAMESPACE,
        action_prefix: None,
        signing_name: LAMBDA_NAMESPACE,
        endpoint_prefix: LAMBDA_NAMESPACE,
        wire: Wire::Rest(LAMBDA_REST),
        sdk_id: "Lambda",
        error_prefix: "Lambda",
        // The AWS SDK names the service `Lambda`, which its own `Lambda` client carries.
        sdk_error_prefix: "Lambda",
        // The client class `AWSLambdaClient`, whose base exception is
        // `AWSLambdaException` — the model's service shape `AWSLambda`.
        sdk_client: "AWSLambda",
        sdk_exception: "AWSLambdaException",
        access_denied_status: TARGET_FORBIDDEN_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `Invoke`'s input: `FunctionName`, `Payload`, `InvocationType`.
        member_case: MemberCase::Pascal,
        query_compatible: false,
    },
    // `services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/
    // smithy-model/com-amazonaws-sts-awssecuritytokenservicev20110615.json`: awsQuery,
    // sigv4Name `sts`, endpointPrefix `sts`, sdkId `STS`, version `2011-06-15` — a FORM
    // protocol, so the operation's input goes on the wire as form fields beside the
    // action it names ([`Wire::Query`]).
    Api {
        namespace: "sts",
        action_prefix: None,
        signing_name: "sts",
        endpoint_prefix: "sts",
        wire: Wire::Query {
            version: "2011-06-15",
            // Every error of its model is bound to the query code that is its own shape
            // name, with or without the `Exception` suffix a class name ends in
            // (`MalformedPolicyDocument` -> `MalformedPolicyDocumentException`,
            // `RegionDisabledException` -> `RegionDisabledException`), so the suffix rule
            // already names each class and there is no pair to state.
            exceptions: &[],
        },
        sdk_id: "STS",
        error_prefix: "STS",
        // The AWS SDK names the service `Sts`, which its own `Sts` client carries.
        sdk_error_prefix: "Sts",
        // The client class `AWSSecurityTokenServiceClient`, whose base exception is
        // `AWSSecurityTokenServiceException` — the model's service shape
        // `AWSSecurityTokenServiceV20110615` without the version it carries.
        sdk_client: "AWSSecurityTokenService",
        sdk_exception: "AWSSecurityTokenServiceException",
        access_denied_status: TARGET_FORBIDDEN_STATUS,
        access_denied_code: COMMON_ACCESS_DENIED_CODE,
        lowercases_refused_action: false,
        // `AssumeRole`'s input: `RoleArn`, `RoleSessionName`, `DurationSeconds`.
        member_case: MemberCase::Pascal,
        // Its model declares the `awsQuery` protocol itself rather than the
        // compatibility modifier, so an error of it is named by its own query code and
        // there is no second name to ask for.
        query_compatible: false,
    },
];
