// SPDX-License-Identifier: CC0-1.0

//! The REAL-TIME LOG RECORD a viewer request produces when that viewer DIALS the address the
//! distribution published as its `DomainName` — a socket of its own, no router call — and
//! what the delivery role decides about it.
//!
//! `Distribution$DomainName` is "the domain name that CloudFront assigns when you create a
//! distribution", the address a viewer is handed and fetches `https://<it>/<object>` from. So
//! the value a distribution publishes there is an address a client on the machine this
//! emulator runs on can DIAL, and the real-time log plane is reached THROUGH it: a request
//! dialled at the published address is served by that distribution's edge, and each served
//! request the matched cache behaviour's real-time log configuration covers puts ONE record on
//! every data stream whose endpoint role may write it — "an IAM role that CloudFront can use to
//! send real-time log data to your Kinesis data stream" — and nothing on the streams whose role
//! may not. A pipeline whose published address answers nothing is one where the permitted and
//! the denied role are indistinguishable, because every stream stays empty.
//!
//! The rows span what the rule generalises over:
//!
//! * the state of the delivery role's policy about its own stream — the put allowed on it,
//!   allowed through a wildcard over the service's actions, left out of the policy altogether,
//!   and explicitly denied beside a wildcard allow — so of the endpoints of ONE configuration
//!   attached to ONE distribution, only those whose role may write receive anything;
//! * the viewer requests dialled at that one address: two objects the origin serves, on
//!   different paths, so each permitted stream holds one record PER request, in the order the
//!   requests were served, and each record describes the request it belongs to.
//!
//! Every value asserted comes from the dialled answer itself (its status and the identifier the
//! edge stamped on it) or from the address the viewer dialled, and every record is read back
//! from KINESIS, through the same `ListShards` / `GetShardIterator` / `GetRecords` walk a client
//! polling the stream makes.
//!
//! The Kinesis, IAM and real-time-log-configuration harness is
//! [`crate::edge_realtime_logs`]'s and the viewer dial is [`crate::published_edge_address`]'s,
//! reused rather than copied; what is new here is the viewer being a SOCKET client of the
//! published address rather than a request dispatched in process.

use crate::distribution_lifecycle::{awaited_deployment, is_reachable_authority, API_VERSION};
use crate::edge_realtime_logs::{
    assert_record, awaited_active, data_stream, delivery_role, distribution,
    emulator_with_kinesis_and_iam, policy, realtime_log_config, stream_records, window, EndPoint,
    Probe, DEFAULT_BEHAVIOR_PATTERN,
};
use crate::published_edge_address::dial;

/// The fields the configuration under test sends, each one a value THIS request or THIS answer
/// carries: when it was served, where the viewer came from, what it asked for, at which host,
/// what it was answered, how the edge classified that answer, and the identifier the edge
/// stamped on it.
const FIELDS: [&str; 9] = [
    "timestamp",
    "c-ip",
    "cs-method",
    "cs-uri-stem",
    "cs-uri-query",
    "cs-host",
    "sc-status",
    "x-edge-result-type",
    "x-edge-request-id",
];

/// One row of the role table: what it is about, the name of the stream it delivers to, what
/// that stream's delivery role states about it, and how many records the stream is owed for
/// each served viewer request.
struct RoleRow {
    case: &'static str,
    name: &'static str,
    statements: &'static [(&'static str, &'static str)],
    per_request: usize,
}

/// What one dialled viewer request asked for.
struct Request {
    case: &'static str,
    method: &'static str,
    path: String,
}

/// What the published address answered one dialled request with: the status of its status line
/// and the `X-Amz-Cf-Id` the edge stamped on it.
struct Answered {
    status: u16,
    request_id: String,
}

impl Answered {
    /// Read one HTTP/1.1 answer as it arrived on the socket.
    fn read(case: &str, raw: &str) -> Self {
        let status = raw
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or_else(|| panic!("case {case}: the answer has no status line: {raw:?}"));
        let request_id = raw
            .lines()
            .take_while(|line| !line.trim().is_empty())
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("x-amz-cf-id")
                    .then(|| value.trim().to_string())
            })
            .unwrap_or_default();
        Self { status, request_id }
    }
}

/// A viewer that dials the published `DomainName` is served by that distribution's edge, and
/// each request it serves delivers one record to every endpoint whose role may put one — and
/// none to the endpoints whose role may not.
#[tokio::test]
async fn a_viewer_dialling_the_published_domain_delivers_a_record_through_each_permitted_role() {
    let router = emulator_with_kinesis_and_iam();
    let rows: [RoleRow; 4] = [
        RoleRow {
            case: "a role allowed to put a record on its stream",
            name: "dialled-realtime-logs-allowed",
            statements: &[("Allow", "kinesis:PutRecord")],
            per_request: 1,
        },
        RoleRow {
            case: "a role allowed every Kinesis action on its stream",
            name: "dialled-realtime-logs-wildcard",
            statements: &[("Allow", "kinesis:*")],
            per_request: 1,
        },
        RoleRow {
            case: "a role whose policy omits the put",
            name: "dialled-realtime-logs-omitted",
            statements: &[("Allow", "kinesis:DescribeStreamSummary")],
            per_request: 0,
        },
        RoleRow {
            case: "a role explicitly denied the put",
            name: "dialled-realtime-logs-denied",
            statements: &[("Allow", "kinesis:*"), ("Deny", "kinesis:PutRecord")],
            per_request: 0,
        },
    ];

    // Every stream and its own delivery role, created before any of them is waited for: the
    // clock owes each stream its creation from the moment it was asked for.
    let mut end_points: Vec<EndPoint> = Vec::new();
    let mut streams: Vec<(&RoleRow, String)> = Vec::new();
    for row in &rows {
        let stream_arn = data_stream(&router, row.name).await;
        let role_arn = delivery_role(&router, row.name, &policy(row.statements, &stream_arn)).await;
        end_points.push(EndPoint {
            role_arn,
            stream_arn: stream_arn.clone(),
        });
        streams.push((row, stream_arn));
    }

    // ONE configuration naming every endpoint, on ONE distribution: what decides a stream's
    // outcome is its own role and nothing else.
    let (config_arn, reported) = realtime_log_config(
        &router,
        "dialled-edge-realtime-logs",
        100,
        &FIELDS,
        &end_points,
    )
    .await;
    let subject = distribution(
        &router,
        "dialled-realtime-logs",
        Some(&config_arn),
        None,
    )
    .await;
    for row in &rows {
        awaited_active(&router, row.name).await;
    }
    awaited_deployment(&router, "dialled-realtime-logs", &subject.id).await;

    // The published name is the address of this distribution's own listener, which is what
    // makes the dial below the thing a viewer handed that name does.
    assert!(
        is_reachable_authority(&subject.domain),
        "expected a published DomainName a viewer can dial, got {:?}",
        subject.domain
    );

    let requests = [
        Request {
            case: "a viewer that dialled the published address for one object",
            method: "GET",
            path: format!("/{API_VERSION}/distribution"),
        },
        Request {
            case: "the same viewer dialling it for another object",
            method: "GET",
            path: format!("/{API_VERSION}/origin-access-control"),
        },
    ];
    let clock = window();
    let mut expected: Vec<(&str, Probe)> = Vec::new();
    for request in &requests {
        let raw = dial(&subject.domain, request.method, &request.path).await;
        let answered = Answered::read(request.case, &raw);
        assert_eq!(
            answered.status, 200,
            "case {}: expected the published address to serve the origin's answer, got {} with \
             {raw:?}",
            request.case, answered.status
        );
        assert!(
            !answered.request_id.is_empty(),
            "case {}: expected the edge to stamp an X-Amz-Cf-Id on the answer, got {raw:?}",
            request.case
        );
        expected.push((
            request.case,
            Probe {
                method: request.method,
                path: request.path.clone(),
                query: "",
                // The host the viewer addressed IS the published address it dialled, which is
                // what a record reports as the distribution's name.
                domain: subject.domain.clone(),
                status: answered.status,
                // The origin produced this answer for this request, which is the edge's miss
                // classification.
                result_type: "Miss",
                request_id: answered.request_id,
                pattern: DEFAULT_BEHAVIOR_PATTERN.to_string(),
                window: clock,
            },
        ));
    }

    for (row, stream_arn) in streams {
        let records = stream_records(&router, &stream_arn).await;
        let owed = row.per_request * expected.len();
        assert_eq!(
            records.len(),
            owed,
            "case {}: expected {owed} record(s) on this stream for {} dialled request(s), got \
             {records:?}",
            row.case,
            expected.len()
        );
        // A stream holds one producer's records in the order they were written, so each record
        // is the one the request served at that position owes.
        for (record, (case, probe)) in records.iter().zip(&expected) {
            assert_record(&format!("{} / {case}", row.case), &reported, record, probe);
        }
    }
}
