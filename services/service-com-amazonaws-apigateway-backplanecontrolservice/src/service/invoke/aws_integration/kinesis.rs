// SPDX-License-Identifier: CC0-1.0

//! The `AWS` integration whose `uri` names AMAZON KINESIS DATA STREAMS: the wire facts of that
//! api, which are the only thing placing a call against it needs.
//!
//! The stream api is a JSON-RPC one (`awsJson1_1`, `smithy_query(query="service_info")`), so
//! HOW a method request becomes one of its calls is the rule [`super::json_rpc`] states for
//! every such api — the `action` form of the `uri` names the operation, the rendered request
//! template IS that operation's input document, and the resource the call is authorized on is
//! the one that document names. This module states what this api publishes for that rule to
//! read.

use super::json_rpc::JsonRpcApi;

/// The subdomain field that names Amazon Kinesis Data Streams, from the form `Integration$uri`
/// documents, and the namespace its IAM actions are published under (`arnNamespace: kinesis`,
/// `smithy_query(query="service_info")`).
const KINESIS_SUBDOMAIN: &str = "kinesis";

/// Amazon Kinesis Data Streams as an `AWS` integration addresses it.
///
/// * `target_prefix` is that model's service shape, which an `awsJson1_1` `X-Amz-Target` names
///   an operation under (`Kinesis_20131202`, `smithy_query(query="service_info")`);
/// * `media_type` is the media type `awsJson1_1` carries;
/// * `resource_type` is the resource field of a stream ARN, whose published form this model
///   states as the `@pattern` of `StreamARN`
///   (`^arn:aws.*:kinesis:.*:\d{12}:stream/\S+$`, `smithy_query(query="shape",
///   shape="StreamARN")`);
/// * `arn_members` are the members an input names its resource in by ARN, in the order a
///   request narrows what it addresses: `ResourceARN` (the tagging and resource-policy
///   operations), `ChannelARN`, `ConsumerARN` (a registered consumer, its own resource type)
///   and `StreamARN` — "It is recommended that you use the StreamARN input parameter when you
///   invoke this API" (`PutRecord`), so an ARN the request carried wins over a name beside it;
/// * `name_members` are the members that name a resource instead: `StreamName`, and
///   `ChannelName`, which a channel create addresses its channel by.
pub(super) const API: JsonRpcApi = JsonRpcApi {
    subdomain: KINESIS_SUBDOMAIN,
    service_name: "Amazon Kinesis",
    target_prefix: "Kinesis_20131202",
    media_type: "application/x-amz-json-1.1",
    resource_type: "stream",
    arn_members: &["ResourceARN", "ChannelARN", "ConsumerARN", "StreamARN"],
    name_members: &[("StreamName", "stream"), ("ChannelName", "channel")],
};
