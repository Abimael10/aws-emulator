// SPDX-License-Identifier: CC0-1.0

//! The `AWS` integration whose `uri` names AMAZON DYNAMODB: the wire facts of that api, which
//! are the only thing placing a call against it needs.
//!
//! Amazon DynamoDB's api is a JSON-RPC one (`awsJson1_0`), so HOW a method request becomes one
//! of its calls is the rule [`super::json_rpc`] states for every such api — the `action` form
//! of the `uri` names the operation, the rendered request template IS that operation's input
//! document, and the resource the call is authorized on is the one that document names. This
//! module states what this api publishes for that rule to read, and nothing else: the two
//! things those facts decide — the action a call of it is authorized by and the resource it
//! addresses — are asserted beside the rule itself, as rows of [`super::json_rpc`]'s own
//! tables, so this api's reading and the stream api's are checked against one statement of it.

use super::json_rpc::JsonRpcApi;

/// The subdomain field that names Amazon DynamoDB, from the form `Integration$uri` documents,
/// and the namespace its IAM actions are published under (`arnNamespace: dynamodb`,
/// `smithy_query(query="service_info")`).
const DYNAMODB_SUBDOMAIN: &str = "dynamodb";

/// Amazon DynamoDB as an `AWS` integration addresses it.
///
/// * `target_prefix` is that model's service shape, which an `awsJson1_0` `X-Amz-Target` names
///   an operation under (`DynamoDB_20120810`, `smithy_query(query="service_info")`);
/// * `media_type` is the media type `awsJson1_0` carries;
/// * `resource_type` is the resource field of a table ARN (`table/{TableName}`), and
///   `name_members` the member every table action names its table in (`TableName`, required on
///   each of them). The api names no table BY ARN, so a call addresses one by name alone.
pub(super) const API: JsonRpcApi = JsonRpcApi {
    subdomain: DYNAMODB_SUBDOMAIN,
    service_name: "Amazon DynamoDB",
    target_prefix: "DynamoDB_20120810",
    media_type: "application/x-amz-json-1.0",
    resource_type: "table",
    arn_members: &[],
    name_members: &[("TableName", "table")],
};
