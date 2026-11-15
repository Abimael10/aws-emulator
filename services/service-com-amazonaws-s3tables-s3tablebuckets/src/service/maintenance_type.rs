// SPDX-License-Identifier: CC0-1.0

//! The maintenance TYPE a put addresses a configuration by: where it sits in the
//! request, and which types each resource has.
//!
//! Each maintenance put names the task it configures in the request PATH, as the
//! last segment of the URI its `@http` trait binds —
//! `/tables/{tableBucketARN}/{namespace}/{name}/maintenance/{type}` for a TABLE's
//! tasks and `/buckets/{tableBucketARN}/maintenance/{type}` for a table BUCKET's
//! (`smithy_query(query="operation",
//! operation="PutTableMaintenanceConfiguration,
//! PutTableBucketMaintenanceConfiguration")`). Reading that label is what
//! [`super::maintenance_put`] needs before the body is interpreted, and this
//! module is the one place it is read: WHERE the label sits comes off the
//! operation's own modelled URI rather than a path written out again here.

use aws_smithy_runtime_api::http::Request as RtRequest;
use smithy_server_runtime::Operation;

/// The `@httpLabel` the maintenance puts bind the task to, as the model's `@http`
/// URI spells its placeholder.
const TYPE_LABEL: &str = "{type}";

/// The value `path` fills `label` with in `pattern` — the operation's modelled
/// `@http` URI — or `None` when the path reaches no such segment.
///
/// Segments are counted the way the workspace's reader of one counts them
/// (`service_common::nth_path_segment`: a leading `/` ignored, nothing else
/// skipped), on the pattern and on the path alike, so the position read off the
/// model is the position read off the request. The segment is percent-decoded the
/// way the codec decodes a path label
/// (`service_common::percent_decode_path_label`), so a type spelled with an
/// escape is the value the caller sent.
fn label_value(pattern: &str, path: &str, label: &str) -> Option<String> {
    let at = pattern
        .trim_start_matches('/')
        .split('/')
        .position(|segment| segment == label)?;
    let segment = ::service_common::nth_path_segment(path, at)?;
    Some(::service_common::percent_decode_path_label(segment))
}

/// The maintenance type the path of `req` addresses the operation `O` with, read
/// off `O`'s own modelled URI, or `None` when the path fills that label with
/// nothing.
pub(super) fn addressed_type<O: Operation>(req: &RtRequest) -> Option<String> {
    let pattern = O::INPUT_SCHEMA.http()?.uri();
    label_value(pattern, ::service_common::uri_path(req.uri()), TYPE_LABEL)
}
