// SPDX-License-Identifier: CC0-1.0

//! The POSIX identity an access point holds, as every read of it reports it.
//!
//! The rule, one level above the recorded case (one `CreateAccessPoint` whose `PosixUser`
//! named a `Uid` and a `Gid` and no secondary group):
//!
//! > An OPTIONAL member of a stored structure that the request never set is reported as
//! > ABSENT, not as the empty value of its type: `PosixUser$SecondaryGids` is optional and
//! > carries no `@default` (`smithy_query(query="shape",shape="PosixUser")`), so an access
//! > point created with no secondary group has no such list and restJson1 leaves the member
//! > out of the body, while one created with a list carries exactly the list it was given —
//! > on the create's own answer and on every later read of the same record.
//!
//! The rows span what the rule generalises over: the access point with no `PosixUser` at all
//! (a different absence), one whose identity names no secondary group, and ones whose list
//! holds a single group and the modelled maximum of sixteen (`SecondaryGids` is
//! `@length 0..16`), at both published ends of `Uid`/`Gid` ("values from 0 to 2^32
//! (4294967295)").

use serde_json::{json, Value};

use crate::file_system_lifecycle::{settle, Emulator};

/// One row: the `PosixUser` the create names, and therefore the one every read must report
/// back — the request's own member, unchanged. `None` names no identity at all.
struct Row {
    case: &'static str,
    posix_user: Option<Value>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no POSIX identity at all",
            posix_user: None,
        },
        Row {
            case: "an identity naming no secondary group",
            posix_user: Some(json!({ "Uid": 1000, "Gid": 1000 })),
        },
        Row {
            case: "an identity with one secondary group",
            posix_user: Some(json!({ "Uid": 0, "Gid": 0, "SecondaryGids": [4] })),
        },
        Row {
            case: "an identity with the modelled maximum of sixteen secondary groups",
            posix_user: Some(json!({
                "Uid": 4_294_967_295u64,
                "Gid": 4_294_967_295u64,
                "SecondaryGids": (101..=116).collect::<Vec<i64>>(),
            })),
        },
    ]
}

/// The `PosixUser` one access-point description reports, as a JSON value — `Null` for a
/// description that carries the member not at all, which is what an absent member reads as.
fn reported(description: &Value) -> Value {
    description
        .get("PosixUser")
        .cloned()
        .unwrap_or(Value::Null)
}

/// RULE: the identity an access point reports is the one the request named, member for
/// member, with no member the request left out filled in — on the create and on the read.
#[tokio::test]
async fn an_access_point_reports_the_posix_identity_its_request_named() {
    let efs = Emulator::new();
    let file_system_id = efs.available_file_system("posix-identity").await;

    let mut created: Vec<(&'static str, String, Value)> = Vec::new();
    for (index, row) in rows().into_iter().enumerate() {
        let mut body = json!({
            "ClientToken": format!("posix-identity-{index}"),
            "FileSystemId": file_system_id,
        });
        if let Some(identity) = row.posix_user.clone() {
            body["PosixUser"] = identity;
        }
        let reply = efs.create_access_point(body).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: expected 200 from CreateAccessPoint, got {} {:?}",
            row.case, reply.status, reply.body
        );
        let expected = row.posix_user.clone().unwrap_or(Value::Null);
        assert_eq!(
            reported(&reply.body),
            expected,
            "case {:?}: expected the create to report {expected:?}, got {:?}",
            row.case,
            reply.body
        );
        created.push((row.case, reply.string("AccessPointId"), expected));
    }

    // And the same, read back off the stored record once the access point has settled.
    settle().await;
    for (case, access_point_id, expected) in &created {
        let reply = efs
            .describe_access_points(&format!("AccessPointId={access_point_id}"))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200 from DescribeAccessPoints, got {} {:?}",
            reply.status, reply.body
        );
        let described = reply.get("AccessPoints")[0].clone();
        assert_eq!(
            reported(&described),
            *expected,
            "case {case:?}: expected the read to report {expected:?}, got {described:?}"
        );
    }
}
