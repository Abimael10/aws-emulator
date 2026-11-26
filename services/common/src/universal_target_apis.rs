// SPDX-License-Identifier: CC0-1.0

//! The APIs a UNIVERSAL target may name, per AWS SDK service: which operations
//! a service has, which of them only READ, and the members an operation's input
//! requires.
//!
//! Several services take a target that names an API OPERATION rather than a
//! resource — EventBridge's subscriber, EventBridge Scheduler's universal
//! target, Step Functions' AWS SDK integration — all written the same way,
//! `arn:<partition>:<service namespace>:::aws-sdk:<sdk service>:<apiAction>`.
//! Each of those services validates the pair when the target is WRITTEN, so
//! each needs the same three facts about the CALLED service, and none of them is
//! a fact of the calling service's own model. They are stated here once.
//!
//! Which WIRE facts a call to one of those services is made with — its endpoint,
//! its signing name, its protocol and the spelling its model gives a member — is
//! the other half of this, stated in the AWS SDK integration family of AWS Step
//! Functions (`state::integration::APIS`, resolved through
//! `state::integration::aws_sdk_call`) because that is the service that MAKES
//! the call. Nothing here makes one: this answers only what a caller validating
//! a written target asks, which is what that table does not hold.
//!
//! It is a LIST rather than a rule because each entry's facts are the called
//! service's own model's, which no crate here can read at runtime: every entry
//! states the model it was read from. A service with NO entry is one this slice
//! knows no APIs for, and nothing is claimed about it — AWS's universal targets
//! span "over 6,000 API operations across more than 270 services", so a service
//! absent from this table is still a service, and a caller refusing a target for
//! it would refuse one the service accepts.
//!
//! Names are compared case-insensitively: a model names an operation in upper
//! camel case (`GetItem`) and a universal target's ARN spells the same action in
//! lower camel case (`getItem`), so one list serves both spellings.

/// One service's APIs, as that service's own model declares them.
#[derive(Debug, Clone, Copy)]
pub struct ServiceApis {
    /// The SDK service name a universal target's ARN names the service by.
    pub service: &'static str,
    /// Every operation the service models, in the model's own spelling.
    pub operations: &'static [&'static str],
    /// The operations of [`Self::operations`] whose only effect is their own
    /// ANSWER: they report the state of a resource and change none of it.
    ///
    /// Stated per operation rather than read off a name, because a model may
    /// mark none of them `@readonly` (Amazon DynamoDB's marks no operation) and
    /// because the spelling does not decide it either way: `ReceiveMessage` and
    /// `PurgeQueue` read like reads and both change the queue.
    pub read_only: &'static [&'static str],
    /// The members the INPUT of an operation requires, for the operations this
    /// slice holds that list for: each entry is an operation of
    /// [`Self::operations`] and the `@required` members of its input shape, in
    /// alphabetical order. An operation with no entry is one whose required
    /// members this slice does not hold, and nothing is claimed about its input.
    pub required_input: &'static [(&'static str, &'static [&'static str])],
}

impl ServiceApis {
    /// Whether this service has the operation `action`.
    #[must_use]
    pub fn names(&self, action: &str) -> bool {
        self.operations
            .iter()
            .any(|declared| declared.eq_ignore_ascii_case(action))
    }

    /// Whether `action` is one of this service's operations that only read.
    #[must_use]
    pub fn only_reads(&self, action: &str) -> bool {
        self.read_only
            .iter()
            .any(|declared| declared.eq_ignore_ascii_case(action))
    }

    /// The members `action`'s input requires, or `None` when this slice holds no
    /// required-member list for it.
    #[must_use]
    pub fn required_input_of(&self, action: &str) -> Option<&'static [&'static str]> {
        self.required_input
            .iter()
            .find(|(named, _)| named.eq_ignore_ascii_case(action))
            .map(|(_, required)| *required)
    }
}

/// The services this slice holds the APIs of.
pub const SERVICE_APIS: &[ServiceApis] = &[
    ServiceApis {
        service: "dynamodb",
        operations: DYNAMODB_OPERATIONS,
        read_only: DYNAMODB_READ_ONLY,
        required_input: DYNAMODB_REQUIRED_INPUT,
    },
    ServiceApis {
        service: "sqs",
        operations: SQS_OPERATIONS,
        read_only: SQS_READ_ONLY,
        required_input: SQS_REQUIRED_INPUT,
    },
    ServiceApis {
        service: "s3",
        operations: S3_OPERATIONS,
        read_only: S3_READ_ONLY,
        // No operation's required members are held for Amazon S3 yet: its
        // operations bind their members to the path, the query and the headers
        // rather than to one request document, so what a caller's own JSON
        // request has to carry is not the input shape's `@required` set alone.
        required_input: &[],
    },
];

/// The APIs of the SDK service `service`, or `None` when this slice holds none
/// for it. The service name is compared case-insensitively, as a target's ARN
/// spells it.
#[must_use]
pub fn apis_of(service: &str) -> Option<&'static ServiceApis> {
    SERVICE_APIS
        .iter()
        .find(|entry| entry.service.eq_ignore_ascii_case(service))
}

/// Every operation Amazon DynamoDB models.
///
/// Source: the operations bound to the service shape of
/// `services/service-com-amazonaws-dynamodb-dynamodb-20120810/smithy-model/
/// com-amazonaws-dynamodb-dynamodb-20120810.json`.
const DYNAMODB_OPERATIONS: &[&str] = &[
    "BatchExecuteStatement",
    "BatchGetItem",
    "BatchWriteItem",
    "CreateBackup",
    "CreateGlobalTable",
    "CreateTable",
    "DeleteBackup",
    "DeleteItem",
    "DeleteResourcePolicy",
    "DeleteTable",
    "DescribeBackup",
    "DescribeContinuousBackups",
    "DescribeContributorInsights",
    "DescribeEndpoints",
    "DescribeExport",
    "DescribeGlobalTable",
    "DescribeGlobalTableSettings",
    "DescribeImport",
    "DescribeKinesisStreamingDestination",
    "DescribeLimits",
    "DescribeTable",
    "DescribeTableReplicaAutoScaling",
    "DescribeTimeToLive",
    "DisableKinesisStreamingDestination",
    "EnableKinesisStreamingDestination",
    "ExecuteStatement",
    "ExecuteTransaction",
    "ExportTableToPointInTime",
    "GetItem",
    "GetResourcePolicy",
    "ImportTable",
    "ListBackups",
    "ListBulkOperations",
    "ListContributorInsights",
    "ListExports",
    "ListGlobalTables",
    "ListImports",
    "ListTables",
    "ListTagsOfResource",
    "PutItem",
    "PutResourcePolicy",
    "Query",
    "RestoreTableFromBackup",
    "RestoreTableToPointInTime",
    "Scan",
    "SearchVectors",
    "StartBulkPutItems",
    "TagResource",
    "TransactGetItems",
    "TransactWriteItems",
    "UntagResource",
    "UpdateContinuousBackups",
    "UpdateContributorInsights",
    "UpdateGlobalTable",
    "UpdateGlobalTableSettings",
    "UpdateItem",
    "UpdateKinesisStreamingDestination",
    "UpdateTable",
    "UpdateTableReplicaAutoScaling",
    "UpdateTimeToLive",
];

/// The Amazon DynamoDB operations whose only effect is their own answer: every
/// item, table, backup, export, import, insight, policy and tag READ, and the
/// two transactional and batch reads beside them. Each of the rest changes a
/// table, an item, a backup, a stream destination, a policy or a tag — including
/// the PartiQL ones (`ExecuteStatement`, `BatchExecuteStatement`,
/// `ExecuteTransaction`), whose statement may write.
const DYNAMODB_READ_ONLY: &[&str] = &[
    "BatchGetItem",
    "DescribeBackup",
    "DescribeContinuousBackups",
    "DescribeContributorInsights",
    "DescribeEndpoints",
    "DescribeExport",
    "DescribeGlobalTable",
    "DescribeGlobalTableSettings",
    "DescribeImport",
    "DescribeKinesisStreamingDestination",
    "DescribeLimits",
    "DescribeTable",
    "DescribeTableReplicaAutoScaling",
    "DescribeTimeToLive",
    "GetItem",
    "GetResourcePolicy",
    "ListBackups",
    "ListBulkOperations",
    "ListContributorInsights",
    "ListExports",
    "ListGlobalTables",
    "ListImports",
    "ListTables",
    "ListTagsOfResource",
    "Query",
    "Scan",
    "SearchVectors",
    "TransactGetItems",
];

/// The `@required` input members of the Amazon DynamoDB operations this slice
/// holds them for, alphabetically within each operation.
///
/// Source: each operation's input in the model named above.
const DYNAMODB_REQUIRED_INPUT: &[(&str, &[&str])] = &[
    ("BatchWriteItem", &["RequestItems"]),
    ("CreateTable", &["TableName"]),
    ("DeleteItem", &["Key", "TableName"]),
    ("DescribeTable", &["TableName"]),
    ("ExecuteStatement", &["Statement"]),
    ("GetItem", &["Key", "TableName"]),
    ("PutItem", &["Item", "TableName"]),
    ("Query", &["TableName"]),
    ("Scan", &["TableName"]),
    ("UpdateItem", &["Key", "TableName"]),
];

/// Every operation Amazon SQS models.
///
/// Source: the operations bound to the service shape of
/// `services/service-com-amazonaws-sqs-amazonsqs/smithy-model/
/// com-amazonaws-sqs-amazonsqs.json`.
const SQS_OPERATIONS: &[&str] = &[
    "AddPermission",
    "CancelMessageMoveTask",
    "ChangeMessageVisibility",
    "ChangeMessageVisibilityBatch",
    "CreateQueue",
    "DeleteMessage",
    "DeleteMessageBatch",
    "DeleteQueue",
    "GetQueueAttributes",
    "GetQueueUrl",
    "ListDeadLetterSourceQueues",
    "ListMessageMoveTasks",
    "ListQueueTags",
    "ListQueues",
    "PurgeQueue",
    "ReceiveMessage",
    "RemovePermission",
    "SendMessage",
    "SendMessageBatch",
    "SetQueueAttributes",
    "StartMessageMoveTask",
    "TagQueue",
    "UntagQueue",
];

/// The Amazon SQS operations whose only effect is their own answer: the two
/// queue lookups, the two listings and the tag read.
///
/// `ReceiveMessage` is NOT one — it starts a message's visibility timeout, so
/// what the next receiver sees depends on it — and neither is `PurgeQueue`,
/// which deletes every message the queue holds.
const SQS_READ_ONLY: &[&str] = &[
    "GetQueueAttributes",
    "GetQueueUrl",
    "ListDeadLetterSourceQueues",
    "ListMessageMoveTasks",
    "ListQueueTags",
    "ListQueues",
];

/// The `@required` input members of the Amazon SQS operations this slice holds
/// them for, alphabetically within each operation.
///
/// Source: each operation's input in the model named above.
const SQS_REQUIRED_INPUT: &[(&str, &[&str])] = &[
    ("SendMessage", &["MessageBody", "QueueUrl"]),
    ("SendMessageBatch", &["Entries", "QueueUrl"]),
];

/// Every operation Amazon S3 models.
///
/// Source: the operations bound to the service shape of
/// `services/service-com-amazonaws-s3-amazons3/smithy-model/
/// com-amazonaws-s3-amazons3.json`.
const S3_OPERATIONS: &[&str] = &[
    "AbortMultipartUpload",
    "CompleteMultipartUpload",
    "CopyObject",
    "CreateBucket",
    "CreateBucketMetadataConfiguration",
    "CreateBucketMetadataTableConfiguration",
    "CreateMultipartUpload",
    "CreateSession",
    "DeleteBucket",
    "DeleteBucketAnalyticsConfiguration",
    "DeleteBucketCors",
    "DeleteBucketEncryption",
    "DeleteBucketIntelligentTieringConfiguration",
    "DeleteBucketInventoryConfiguration",
    "DeleteBucketLifecycle",
    "DeleteBucketMetadataConfiguration",
    "DeleteBucketMetadataTableConfiguration",
    "DeleteBucketMetricsConfiguration",
    "DeleteBucketOwnershipControls",
    "DeleteBucketPolicy",
    "DeleteBucketReplication",
    "DeleteBucketTagging",
    "DeleteBucketWebsite",
    "DeleteObject",
    "DeleteObjectAnnotation",
    "DeleteObjectTagging",
    "DeleteObjects",
    "DeletePublicAccessBlock",
    "GetBucketAbac",
    "GetBucketAccelerateConfiguration",
    "GetBucketAcl",
    "GetBucketAnalyticsConfiguration",
    "GetBucketCors",
    "GetBucketEncryption",
    "GetBucketIntelligentTieringConfiguration",
    "GetBucketInventoryConfiguration",
    "GetBucketLifecycleConfiguration",
    "GetBucketLocation",
    "GetBucketLogging",
    "GetBucketMetadataConfiguration",
    "GetBucketMetadataTableConfiguration",
    "GetBucketMetricsConfiguration",
    "GetBucketNotificationConfiguration",
    "GetBucketOwnershipControls",
    "GetBucketPolicy",
    "GetBucketPolicyStatus",
    "GetBucketReplication",
    "GetBucketRequestPayment",
    "GetBucketTagging",
    "GetBucketVersioning",
    "GetBucketWebsite",
    "GetObject",
    "GetObjectAcl",
    "GetObjectAnnotation",
    "GetObjectAttributes",
    "GetObjectLegalHold",
    "GetObjectLockConfiguration",
    "GetObjectRetention",
    "GetObjectTagging",
    "GetObjectTorrent",
    "GetPublicAccessBlock",
    "HeadBucket",
    "HeadObject",
    "ListBucketAnalyticsConfigurations",
    "ListBucketIntelligentTieringConfigurations",
    "ListBucketInventoryConfigurations",
    "ListBucketMetricsConfigurations",
    "ListBuckets",
    "ListDirectoryBuckets",
    "ListMultipartUploads",
    "ListObjectAnnotations",
    "ListObjectVersions",
    "ListObjects",
    "ListObjectsV2",
    "ListParts",
    "PutBucketAbac",
    "PutBucketAccelerateConfiguration",
    "PutBucketAcl",
    "PutBucketAnalyticsConfiguration",
    "PutBucketCors",
    "PutBucketEncryption",
    "PutBucketIntelligentTieringConfiguration",
    "PutBucketInventoryConfiguration",
    "PutBucketLifecycleConfiguration",
    "PutBucketLogging",
    "PutBucketMetricsConfiguration",
    "PutBucketNotificationConfiguration",
    "PutBucketOwnershipControls",
    "PutBucketPolicy",
    "PutBucketReplication",
    "PutBucketRequestPayment",
    "PutBucketTagging",
    "PutBucketVersioning",
    "PutBucketWebsite",
    "PutObject",
    "PutObjectAcl",
    "PutObjectAnnotation",
    "PutObjectLegalHold",
    "PutObjectLockConfiguration",
    "PutObjectRetention",
    "PutObjectTagging",
    "PutPublicAccessBlock",
    "RenameObject",
    "RestoreObject",
    "SelectObjectContent",
    "UpdateBucketMetadataAnnotationTableConfiguration",
    "UpdateBucketMetadataInventoryTableConfiguration",
    "UpdateBucketMetadataJournalTableConfiguration",
    "UpdateObjectEncryption",
    "UploadPart",
    "UploadPartCopy",
    "WriteGetObjectResponse",
];

/// The Amazon S3 operations whose only effect is their own answer: every bucket
/// and object READ, every listing, the two existence checks and the object
/// query.
///
/// `CreateSession` is not one — it mints a session — and neither is
/// `RestoreObject` or `WriteGetObjectResponse`, whose names read like reads: the
/// first starts a restore and the second writes the answer of a transformed
/// GET. `SelectObjectContent` IS one: it runs a query over an object and
/// changes nothing.
const S3_READ_ONLY: &[&str] = &[
    "GetBucketAbac",
    "GetBucketAccelerateConfiguration",
    "GetBucketAcl",
    "GetBucketAnalyticsConfiguration",
    "GetBucketCors",
    "GetBucketEncryption",
    "GetBucketIntelligentTieringConfiguration",
    "GetBucketInventoryConfiguration",
    "GetBucketLifecycleConfiguration",
    "GetBucketLocation",
    "GetBucketLogging",
    "GetBucketMetadataConfiguration",
    "GetBucketMetadataTableConfiguration",
    "GetBucketMetricsConfiguration",
    "GetBucketNotificationConfiguration",
    "GetBucketOwnershipControls",
    "GetBucketPolicy",
    "GetBucketPolicyStatus",
    "GetBucketReplication",
    "GetBucketRequestPayment",
    "GetBucketTagging",
    "GetBucketVersioning",
    "GetBucketWebsite",
    "GetObject",
    "GetObjectAcl",
    "GetObjectAnnotation",
    "GetObjectAttributes",
    "GetObjectLegalHold",
    "GetObjectLockConfiguration",
    "GetObjectRetention",
    "GetObjectTagging",
    "GetObjectTorrent",
    "GetPublicAccessBlock",
    "HeadBucket",
    "HeadObject",
    "ListBucketAnalyticsConfigurations",
    "ListBucketIntelligentTieringConfigurations",
    "ListBucketInventoryConfigurations",
    "ListBucketMetricsConfigurations",
    "ListBuckets",
    "ListDirectoryBuckets",
    "ListMultipartUploads",
    "ListObjectAnnotations",
    "ListObjectVersions",
    "ListObjects",
    "ListObjectsV2",
    "ListParts",
    "SelectObjectContent",
];

#[cfg(test)]
mod tests {
    use super::{apis_of, SERVICE_APIS};

    /// RULE: an operation a service's model declares is one of its APIs,
    /// whatever case it is spelled in, and one it does not declare is not.
    ///
    /// The rows span the two services the table holds, both spellings of an
    /// operation, an operation of ANOTHER service in the table, and a service
    /// the table holds nothing for.
    #[test]
    fn an_api_of_a_service_is_one_its_model_declares() {
        // (case, service, action, whether it is an API of that service)
        let rows: [(&str, &str, &str, bool); 8] = [
            ("a write, as a target ARN spells it", "dynamodb", "putItem", true),
            ("the same operation in the model's case", "dynamodb", "PutItem", true),
            ("a read of the same service", "dynamodb", "query", true),
            ("an operation no model declares", "dynamodb", "frobnicate", false),
            ("another service's operation", "dynamodb", "sendMessage", false),
            ("a queue write", "sqs", "sendMessage", true),
            ("a queue operation that does not exist", "sqs", "sendMessages", false),
            ("a service the table holds nothing for", "lambda", "invoke", false),
        ];
        for (case, service, action, expected) in rows {
            let read = apis_of(service).is_some_and(|apis| apis.names(action));
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected} for {service}:{action}, got {read}"
            );
        }
    }

    /// RULE: an operation that only reads is one the table says so about, and
    /// the spelling decides nothing — a read whose name opens with a write verb
    /// and a write whose name opens with a read verb are both classed by the
    /// entry rather than by the prefix.
    #[test]
    fn an_operation_that_only_reads_is_the_one_the_entry_names() {
        // (case, service, action, whether it only reads)
        let rows: [(&str, &str, &str, bool); 12] = [
            ("an item read", "dynamodb", "getItem", true),
            ("a transactional read", "dynamodb", "transactGetItems", true),
            ("an item write", "dynamodb", "putItem", false),
            ("a PartiQL statement, which may write", "dynamodb", "executeStatement", false),
            ("a queue lookup", "sqs", "getQueueUrl", true),
            ("a receive, which starts a visibility timeout", "sqs", "receiveMessage", false),
            ("a purge, which deletes every message", "sqs", "purgeQueue", false),
            ("a send", "sqs", "sendMessage", false),
            ("an object read", "s3", "getObject", true),
            ("an object query, which changes nothing", "s3", "selectObjectContent", true),
            ("a restore, which starts one", "s3", "restoreObject", false),
            ("an object write", "s3", "putObject", false),
        ];
        for (case, service, action, expected) in rows {
            let read = apis_of(service).is_some_and(|apis| apis.only_reads(action));
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected} for {service}:{action}, got {read}"
            );
        }
    }

    /// RULE: every entry holds a non-empty operation list, no service appears
    /// twice, and every operation it classes or states required members for is
    /// one of its own operations, those members in order.
    #[test]
    fn each_entry_is_consistent_with_its_own_operation_list() {
        for entry in SERVICE_APIS {
            assert!(
                !entry.operations.is_empty(),
                "case {:?}: a service in the table must hold an operation list",
                entry.service
            );
            let entries = SERVICE_APIS
                .iter()
                .filter(|other| other.service.eq_ignore_ascii_case(entry.service))
                .count();
            assert_eq!(
                entries, 1,
                "case {:?}: expected one entry for the service, got {entries}",
                entry.service
            );
            for operation in entry.read_only {
                assert!(
                    entry.names(operation),
                    "case {:?}: {operation} must be an operation of the service",
                    entry.service
                );
            }
            for (operation, required) in entry.required_input {
                assert!(
                    entry.names(operation),
                    "case {:?}: {operation} must be an operation of the service",
                    entry.service
                );
                assert!(
                    !required.is_empty(),
                    "case {:?}: {operation}'s entry must name the members it requires",
                    entry.service
                );
                let mut sorted = required.to_vec();
                sorted.sort_unstable();
                assert_eq!(
                    sorted, *required,
                    "case {:?}: {operation}'s required members must be in order",
                    entry.service
                );
                assert_eq!(
                    entry.required_input_of(operation),
                    Some(*required),
                    "case {:?}: {operation}'s required members must be found",
                    entry.service
                );
            }
        }
    }
}
