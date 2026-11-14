// SPDX-License-Identifier: CC0-1.0

//! The ingestion endpoint a pipeline PUBLISHES, and the addressing that endpoint is served
//! at.
//!
//! `Pipeline$IngestEndpointUrls` is "The ingestion endpoints for the pipeline, which you can
//! send data to": not a label a caller reads back, but an address a client then POSTS its
//! records to. So the address a pipeline publishes has to be one THIS emulator answers on,
//! and the ingest plane ([`crate::service::ingest_plane`]) is what answers there.
//!
//! Two forms can stand in that member, and [`ingest_endpoint_urls`] picks between them with
//! the tree's one reading of the question
//! (`service_common::data_plane_addressing::reached_emulator_endpoint` /
//! `served_endpoint_authority`):
//!
//! * the EMULATOR's own endpoint — the one this request reached, else the port this process
//!   is served on — with the pipeline named behind the reserved namespace segment
//!   (`/_aws/osis/<pipeline-name>`), which is the path-style addressing
//!   [`addressed_pipeline`] resolves. A client on the machine this emulator runs on resolves
//!   no host but the one it is already talking to, so this is an address it can reach, and
//!   the pipeline's own configured source path appends to it exactly as it appends to AWS's
//!   host;
//! * the host AWS itself publishes for a pipeline
//!   (`<pipeline-name>-<id>.<region>.osis.amazonaws.com`), for a request that addressed one
//!   of AWS's own hosts on an emulator served on no port at all: there is no own address to
//!   publish in its place.

use ::service_common::data_plane_addressing::{
    first_segment, reached_emulator_endpoint, served_endpoint_authority, EMULATOR_PATH_NAMESPACE,
};

use super::ARN_SERVICE;

/// The segment that names THIS service's ingest plane behind
/// [`EMULATOR_PATH_NAMESPACE`]: the service's own name, so a path under the reserved
/// namespace that names another plane is that plane's and never this one's.
pub const DATA_PLANE_SEGMENT: &str = ARN_SERVICE;

/// The ingestion endpoints a pipeline publishes (see the module docs for which form stands
/// for whom).
///
/// One endpoint per pipeline, which is what the real service publishes. The list is resolved
/// ONCE, by the create that mints the pipeline, and stored: the member is an address a client
/// keeps, so every later reporter answers the one address rather than re-deriving a different
/// one per request — the trade-off being that a client reaching this emulator on another of
/// its own authorities than the create did is handed the create's (it can still reach it, as
/// both are addresses of this one process, and a client that resolves AWS's own hosts is
/// handed the published host either way).
pub fn ingest_endpoint_urls(
    parts: &::http::request::Parts,
    served_base: &str,
    region: &str,
    pipeline_name: &str,
    pipeline_arn: &str,
) -> Vec<String> {
    match emulator_authority(parts, served_base) {
        Some(authority) => vec![ingest_plane_base(&authority, pipeline_name)],
        None => vec![aws_ingest_endpoint_host(region, pipeline_name, pipeline_arn)],
    }
}

/// The address this emulator answers on for the client that made this request: the
/// emulator's own endpoint the request reached, else the port this process is served on, and
/// `None` for a request that addressed one of AWS's own hosts on an emulator served on no
/// port at all — which has no own address to publish.
pub fn emulator_authority(
    parts: &::http::request::Parts,
    served_base: &str,
) -> Option<String> {
    reached_emulator_endpoint(
        &parts.uri.to_string(),
        parts
            .headers
            .get(::http::header::HOST)
            .and_then(|value| value.to_str().ok()),
    )
    .or_else(|| served_endpoint_authority(served_base))
}

/// `<authority>/_aws/osis/<pipeline name>` — the pipeline's ingestion endpoint on an address
/// this emulator answers on, in the path-style addressing [`addressed_pipeline`] resolves.
///
/// No scheme and no trailing separator: what the member carries is the address a client
/// appends the pipeline's own Data Prepper source path to, which is the form AWS publishes
/// it in.
pub fn ingest_plane_base(authority: &str, pipeline_name: &str) -> String {
    format!("{authority}/{EMULATOR_PATH_NAMESPACE}/{DATA_PLANE_SEGMENT}/{pipeline_name}")
}

/// The ingestion endpoint HOST AWS itself publishes for a pipeline, shaped
/// `<pipeline-name>-<pipeline-id>.<region>.osis.amazonaws.com`.
///
/// The id segment is content-derived (a digest of the pipeline's own ARN, so it is stable
/// across reads, differs between two pipelines, and never leaks a clock or a request id) and
/// uses the same alphabet AWS's does. Published only for a caller that resolves AWS's own
/// hosts on an emulator that has no address of its own to publish.
pub fn aws_ingest_endpoint_host(region: &str, pipeline_name: &str, pipeline_arn: &str) -> String {
    let digest = <::sha2::Sha256 as ::sha2::Digest>::digest(pipeline_arn.as_bytes());
    let id = ::hex::encode(&digest[..5]);
    format!("{pipeline_name}-{id}.{region}.{ARN_SERVICE}.amazonaws.com")
}

/// The pipeline a path-style ingest address NAMES and the path WITHIN that pipeline's
/// endpoint — the path the pipeline's own Data Prepper source is read against — or `None`
/// when the path does not address this plane at all.
///
/// The addressing is the one [`ingest_plane_base`] publishes: `/_aws/osis/<pipeline-name>`
/// and then the source's own path, so `/_aws/osis/my-pipeline/ingest` addresses `/ingest` of
/// `my-pipeline`.
pub fn addressed_pipeline(path: &str) -> Option<(&str, &str)> {
    let (namespace, rest) = first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    if plane != DATA_PLANE_SEGMENT {
        return None;
    }
    let (pipeline_name, source_path) = first_segment(rest)?;
    (!pipeline_name.is_empty()).then_some((pipeline_name, source_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request parts of a call that addressed `host`.
    fn addressed(host: &str) -> ::http::request::Parts {
        ::http::Request::builder()
            .uri("/2022-01-01/osis/createPipeline")
            .header(::http::header::HOST, host)
            .body(())
            .expect("a well-formed request")
            .into_parts()
            .0
    }

    /// RULE: a pipeline publishes the emulator's own endpoint — the one the request reached,
    /// else the port this process is served on — with the pipeline named in the path, and
    /// AWS's own host only when there is no own address to publish in its place.
    #[test]
    fn a_pipeline_publishes_the_emulators_own_address_wherever_there_is_one() {
        /// One case: who called, where this emulator is served, and the endpoint published.
        struct Row {
            case: &'static str,
            host: &'static str,
            served_base: &'static str,
            published: String,
        }
        let name = "orders-pipeline";
        let region = "us-west-2";
        let arn = super::super::pipeline_arn(region, name);
        let served = "https://127.0.0.1:39355";
        let rows: Vec<Row> = vec![
            Row {
                case: "a client on the emulator's own loopback endpoint",
                host: "127.0.0.1:9297",
                served_base: served,
                published: format!("127.0.0.1:9297/_aws/osis/{name}"),
            },
            Row {
                case: "a client on a loopback name, this emulator served on no port",
                host: "localhost:41657",
                served_base: ::smithy_server_runtime::UNSERVED_URL_BASE,
                published: format!("localhost:41657/_aws/osis/{name}"),
            },
            Row {
                case: "a client that resolves AWS's own hosts, this emulator served",
                host: "osis.us-west-2.amazonaws.com",
                served_base: served,
                published: format!("127.0.0.1:39355/_aws/osis/{name}"),
            },
            Row {
                case: "a client that resolves AWS's own hosts, served on no port at all",
                host: "osis.us-west-2.amazonaws.com",
                served_base: ::smithy_server_runtime::UNSERVED_URL_BASE,
                published: aws_ingest_endpoint_host(region, name, &arn),
            },
        ];
        for Row {
            case,
            host,
            served_base,
            published,
        } in &rows
        {
            let urls = ingest_endpoint_urls(&addressed(host), served_base, region, name, &arn);
            assert_eq!(
                urls,
                vec![published.clone()],
                "case {case:?}: expected the one published endpoint {published:?}, got {urls:?}"
            );
        }
    }

    /// RULE: a path-style ingest address resolves to the pipeline it names and the source
    /// path within its endpoint; anything else addresses this plane not at all.
    #[test]
    fn a_path_style_address_resolves_the_pipeline_and_the_source_path() {
        /// One case: the path addressed, and the pipeline and source path it resolves to.
        struct Row {
            case: &'static str,
            path: &'static str,
            resolved: Option<(&'static str, &'static str)>,
        }
        let rows: &[Row] = &[
            Row {
                case: "the published base itself",
                path: "/_aws/osis/orders",
                resolved: Some(("orders", "/")),
            },
            Row {
                case: "the configured source path below it",
                path: "/_aws/osis/orders/ingest",
                resolved: Some(("orders", "/ingest")),
            },
            Row {
                case: "a deeper source path",
                path: "/_aws/osis/orders/log/ingest",
                resolved: Some(("orders", "/log/ingest")),
            },
            Row {
                case: "another plane under the same namespace",
                path: "/_aws/aoss/orders/ingest",
                resolved: None,
            },
            Row {
                case: "a control-plane path",
                path: "/2022-01-01/osis/getPipeline/orders",
                resolved: None,
            },
            Row {
                case: "the namespace with no pipeline named",
                path: "/_aws/osis",
                resolved: None,
            },
            Row {
                case: "a path that is not absolute",
                path: "orders/ingest",
                resolved: None,
            },
        ];
        for Row {
            case,
            path,
            resolved: expected,
        } in rows
        {
            let resolved = addressed_pipeline(path);
            assert_eq!(
                resolved, *expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }
}
