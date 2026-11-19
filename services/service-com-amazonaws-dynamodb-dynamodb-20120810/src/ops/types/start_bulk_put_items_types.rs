// SPDX-License-Identifier: CC0-1.0

//! Type definitions reached only by the `start_bulk_put_items` operation.
//!
//! Each structure/union is declared exactly once across this service's `types/`
//! modules, and `types/mod.rs` re-exports them all.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-dynamodb-dynamodb-20120810.json");

#[smithy_macros::smithy(shape = "com.amazonaws.dynamodb#StartBulkPutItemsInput")]
#[derive(Debug, Clone, Default)]
pub struct StartBulkPutItemsInput {
    pub table_name: String,
    pub input_s3_configuration: BulkPutInputS3Configuration,
    pub output_s3_configuration: Option<BulkPutOutputS3Configuration>,
    pub max_write_request_units: Option<i64>,
    pub performance_tier: Option<String>,
    pub client_token: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.dynamodb#BulkPutInputS3Configuration")]
#[derive(Debug, Clone, Default)]
pub struct BulkPutInputS3Configuration {
    pub s3_bucket: String,
    pub s3_key_prefix: Option<String>,
    pub s3_bucket_owner: Option<String>,
    pub input_format: Option<String>,
    pub input_compression_type: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.dynamodb#BulkPutOutputS3Configuration")]
#[derive(Debug, Clone, Default)]
pub struct BulkPutOutputS3Configuration {
    pub s3_bucket: String,
    pub s3_key_prefix: Option<String>,
    pub s3_bucket_owner: Option<String>,
    pub output_type: Option<Vec<String>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.dynamodb#StartBulkPutItemsOutput")]
#[derive(Debug, Clone, Default)]
pub struct StartBulkPutItemsOutput {
    pub bulk_operation_description: BulkOperationDescription,
}

#[smithy_macros::smithy(shape = "com.amazonaws.dynamodb#BulkOperationDescription")]
#[derive(Debug, Clone, Default)]
pub struct BulkOperationDescription {
    pub bulk_operation_arn: Option<String>,
    pub bulk_operation_status: Option<String>,
    pub table_arn: Option<String>,
    pub client_token: Option<String>,
    pub input_s3_configuration: Option<BulkPutInputS3Configuration>,
    pub output_s3_configuration: Option<BulkPutOutputS3Configuration>,
    pub max_write_request_units: Option<i64>,
    pub performance_tier: Option<String>,
    pub start_time: Option<::aws_smithy_types::DateTime>,
    pub end_time: Option<::aws_smithy_types::DateTime>,
    pub processed_item_count: Option<i64>,
    pub error_count: Option<i64>,
}
