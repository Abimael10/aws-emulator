// SPDX-License-Identifier: CC0-1.0

//! What a file DELIVERED onto a Transfer Family server's Amazon S3 domain is stored
//! with, as the client that later reads the object sees it.
//!
//! RULE under test: an object a Transfer Family server stores in its Amazon S3 domain
//! for an authenticated SFTP session carries the provenance of that session as the
//! object's user metadata — `user-agent` holding the fixed text naming the service that
//! wrote the file, and `user-agent-id` holding `<session user name>@<server id>` — so a
//! later `GetObject` or `HeadObject` of the delivered object serves both back on the
//! `x-amz-meta-*` headers S3 binds `Metadata` to, together with the full-object checksum
//! S3 stores for a write that names no algorithm.
//!
//! The rows differ along what the rule generalises over: WHOSE session delivers the file
//! (four `UserName` forms, which is the variable half of the provenance id), how the
//! destination resolves (a `PATH` user's bucket root, a `PATH` user's own home
//! directory, a `LOGICAL` user's mapping), whether the request names a
//! `RemoteDirectoryPath` at all, and how many files one transfer carries. The recorded
//! divergence is one such row.
//!
//! Nothing of the SOURCE object travels with it: an SFTP transfer carries a file's
//! bytes, so each row stores its source with user metadata of its own and asserts the
//! delivered object holds the session's provenance instead of it.
//!
//! The target, its bucket and its session are the shared ones
//! ([`crate::connector_gated_workflow::Target`]), with Amazon S3 and Secrets Manager
//! mounted beside the transfer service, because the delivery is carried out by the real
//! emulated S3 over the in-process callback.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::connector_gated_workflow::{results, s3, Target, COMPLETED, FAILED};
use crate::harness::{member_of, poll_until};

/// The `Metadata` key, and the fixed value, of the entry naming WHAT wrote a file into
/// a Transfer Family server's storage: served back as `x-amz-meta-user-agent`.
///
/// Recorded behavior — the S3 model says nothing about which metadata keys an object
/// carries — so the key and the text are pinned here as the requirement rather than
/// read out of the crate under test.
const AGENT_KEY: &str = "user-agent";
const AGENT: &str = "AWSTransfer";

/// The `Metadata` key of the entry naming WHICH session delivered the file: served back
/// as `x-amz-meta-user-agent-id`, holding `<session user name>@<server id>`.
const AGENT_ID_KEY: &str = "user-agent-id";

/// The user metadata each row stores its SOURCE object with, to show that what the
/// delivered object carries is the session's provenance and not the source's metadata.
const SOURCE_METADATA: (&str, &str) = ("source-stamp", "written-by-the-client");

/// One row: whose session delivers, where the files land, and how many there are.
struct Delivery {
    case: &'static str,
    /// The `UserName` of the server user the connector's secret authenticates as — the
    /// variable half of the provenance id (`UserName` is `len 3..100`, pattern
    /// `^[\w][\w@.-]{2,99}$`, from `smithy_query(query="shape",shape="UserName")`).
    user_name: &'static str,
    /// `HomeDirectoryType`: `PATH` or `LOGICAL`.
    home_directory_type: &'static str,
    /// The user's `HomeDirectory`, relative to the bucket; empty for the bucket root.
    home_directory: &'static str,
    /// The `HomeDirectoryMappings` a `LOGICAL` user is created with, as a template whose
    /// `{bucket}` is this target's bucket; empty for a `PATH` user.
    mapping_target: &'static str,
    /// `RemoteDirectoryPath` as the request sends it, with `{bucket}` filled in — or
    /// `None` to send none, which lands the files in the user's home directory.
    remote_directory: Option<&'static str>,
    /// Where the files land in the bucket: the key prefix, empty for the bucket root.
    landing_prefix: &'static str,
    /// How many files this row's transfer carries.
    files: usize,
}

/// The rows. Each one's `landing_prefix` is where that row's destination resolution puts
/// the file, which is the object whose metadata the rule is read off.
const DELIVERIES: &[Delivery] = &[
    Delivery {
        case: "a three-character user name, into the remote directory the request names",
        user_name: "abc",
        home_directory_type: "PATH",
        home_directory: "",
        mapping_target: "",
        remote_directory: Some("/{bucket}/landing"),
        landing_prefix: "landing/",
        files: 1,
    },
    Delivery {
        case: "a user name carrying the dot, hyphen and underscore the pattern allows, \
               into the user's own home directory",
        user_name: "sim2real.probe-tu_42",
        home_directory_type: "PATH",
        home_directory: "home/partner",
        mapping_target: "",
        remote_directory: None,
        landing_prefix: "home/partner/",
        files: 2,
    },
    Delivery {
        case: "a user name carrying the at sign the pattern allows, into a nested remote \
               directory",
        user_name: "partner@example.com",
        home_directory_type: "PATH",
        home_directory: "",
        mapping_target: "",
        remote_directory: Some("/{bucket}/deep/nested/inbox"),
        landing_prefix: "deep/nested/inbox/",
        files: 1,
    },
    Delivery {
        case: "a LOGICAL user, whose mapping resolves where the file lands",
        user_name: "logical-session-user",
        home_directory_type: "LOGICAL",
        home_directory: "",
        mapping_target: "/{bucket}/logical",
        remote_directory: Some("/inbox"),
        landing_prefix: "logical/",
        files: 1,
    },
];

/// RULE: every file a session delivers onto the server is stored with that session's
/// provenance as user metadata, which `GetObject` and `HeadObject` both serve back on
/// `x-amz-meta-user-agent` and `x-amz-meta-user-agent-id`, beside the full-object
/// checksum S3 computes over the delivered bytes.
#[tokio::test]
async fn a_delivered_file_carries_the_provenance_of_the_session_that_delivered_it() {
    for (row, delivery) in DELIVERIES.iter().enumerate() {
        let case = delivery.case;
        let home = if delivery.home_directory.is_empty() {
            String::new()
        } else {
            format!("/transfer-connector-bucket/{}", delivery.home_directory)
        };
        let mappings = if delivery.mapping_target.is_empty() {
            Value::Null
        } else {
            json!([{
                "Entry": "/inbox",
                "Target": delivery
                    .mapping_target
                    .replace("{bucket}", "transfer-connector-bucket"),
            }])
        };
        let target = Target::stand_up_as(
            delivery.user_name,
            delivery.home_directory_type,
            &home,
            mappings,
        )
        .await;
        assert_eq!(
            target.user_name, delivery.user_name,
            "case {case:?}: expected the session to be the named user's, got {:?}",
            target.user_name
        );

        let secret = target
            .store_secret(
                "aws/transfer/provenance",
                &target.user_name,
                &target.user_key.private,
            )
            .await;
        let connector_id = target
            .create_connector(
                case,
                json!({"UserSecretId": &secret, "TrustedHostKeys": [&target.host_key]}),
                None,
            )
            .await;

        // Each file carries its own bytes, so a landed file is compared with its own
        // source, and the sizes differ across the table.
        let storage = s3(&target.emulator);
        let sources: Vec<(String, Vec<u8>)> = (0..delivery.files)
            .map(|index| {
                (
                    format!("outbox/r{row}/f{index}.csv"),
                    format!("row,{row},file,{index}{}\n", "!".repeat(row + index)).into_bytes(),
                )
            })
            .collect();
        for (key, body) in &sources {
            storage
                .put_object()
                .bucket(&target.bucket)
                .key(key)
                .body(aws_sdk_s3::primitives::ByteStream::from(body.clone()))
                .metadata(SOURCE_METADATA.0, SOURCE_METADATA.1)
                // The SOURCE is stored under the algorithm the delivered copy gets by
                // default, with the value computed by the SDK client rather than by the
                // emulator — so the checksum this suite compares the delivered object's
                // against is an independent digest of the same bytes.
                .checksum_algorithm(aws_sdk_s3::types::ChecksumAlgorithm::Crc64Nvme)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the emulated S3 stores the source object: {error}")
                });
        }

        let sent: Vec<String> = sources
            .iter()
            .map(|(key, _)| format!("/{}/{key}", target.bucket))
            .collect();
        let mut request = json!({"ConnectorId": &connector_id, "SendFilePaths": &sent});
        if let Some(directory) = delivery.remote_directory {
            request["RemoteDirectoryPath"] = json!(directory.replace("{bucket}", &target.bucket));
        }
        let accepted = target.emulator.call("StartFileTransfer", request).await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: expected StartFileTransfer 200, got {} with body {}",
            accepted.status, accepted.body
        );
        let transfer_id = accepted
            .get("TransferId")
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a TransferId, got {}", accepted.body))
            .to_string();

        // Waited the way a client waits: on the per-file result reaching a terminal
        // status, not on a span read out of the crate.
        let reported = poll_until(
            case,
            "every ListFileTransferResults entry reaches a terminal StatusCode",
            || async {
                let reported = results(&target, &connector_id, &transfer_id).await;
                let settled = reported.len() == delivery.files
                    && reported.iter().all(|result| {
                        matches!(
                            member_of(result, "StatusCode").as_str(),
                            Some(COMPLETED) | Some(FAILED)
                        )
                    });
                (settled, Value::Array(reported))
            },
        )
        .await;
        let reported = reported
            .as_array()
            .expect("the polled results are the list the read reported")
            .clone();

        let expected_agent_id = format!("{}@{}", delivery.user_name, target.server_id);
        for (index, ((key, body), result)) in sources.iter().zip(&reported).enumerate() {
            assert_eq!(
                member_of(result, "StatusCode").as_str(),
                Some(COMPLETED),
                "case {case:?}: expected file {index} to reach {COMPLETED}, got {result}"
            );
            let file_name = key.rsplit('/').next().expect("the source key names a file");
            let landed = format!("{}{file_name}", delivery.landing_prefix);

            let read = storage
                .get_object()
                .bucket(&target.bucket)
                .key(&landed)
                .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: expected a delivered object at {landed:?}: {error}")
                });
            // Read off the answer before its body is consumed.
            let metadata = user_metadata(read.metadata());
            let delivered_checksum = read.checksum_crc64_nvme().map(str::to_string);
            let served = read
                .body
                .collect()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the delivered object's body reads: {error}")
                })
                .to_vec();
            assert_eq!(
                served, *body,
                "case {case:?}: expected the delivered object at {landed:?} to hold its \
                 own bytes, got {served:?}"
            );

            // The rule: the provenance of the session that delivered it, and nothing of
            // the source object's own metadata.
            assert_eq!(
                metadata.get(AGENT_KEY).map(String::as_str),
                Some(AGENT),
                "case {case:?}: expected {landed:?} to carry {AGENT_KEY}={AGENT:?}, got \
                 {metadata:?}"
            );
            assert_eq!(
                metadata.get(AGENT_ID_KEY).map(String::as_str),
                Some(expected_agent_id.as_str()),
                "case {case:?}: expected {landed:?} to carry \
                 {AGENT_ID_KEY}={expected_agent_id:?}, got {metadata:?}"
            );
            assert_eq!(
                metadata.keys().map(String::as_str).collect::<Vec<&str>>(),
                vec![AGENT_KEY, AGENT_ID_KEY],
                "case {case:?}: expected {landed:?} to carry the session's provenance \
                 and nothing of the source object's metadata, got {metadata:?}"
            );

            // A HEAD of the same object reports the same provenance: a client that only
            // asks for the headers learns the same thing.
            let headed = storage
                .head_object()
                .bucket(&target.bucket)
                .key(&landed)
                .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: expected a HEAD of {landed:?} to answer: {error}")
                });
            assert_eq!(
                user_metadata(headed.metadata()),
                metadata,
                "case {case:?}: expected a HEAD of {landed:?} to report the metadata the \
                 GET reported, got {:?}",
                user_metadata(headed.metadata())
            );

            // And the full-object checksum S3 stores for a write that names no
            // algorithm: computed over the delivered bytes, so it is the digest the
            // client computed itself for the source object of the same bytes.
            let source_checksum = storage
                .get_object()
                .bucket(&target.bucket)
                .key(key)
                .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: expected the source object to read: {error}")
                })
                .checksum_crc64_nvme()
                .map(str::to_string);
            assert!(
                delivered_checksum.is_some(),
                "case {case:?}: expected {landed:?} to report a CRC64NVME checksum, got \
                 {delivered_checksum:?}"
            );
            assert_eq!(
                delivered_checksum, source_checksum,
                "case {case:?}: expected {landed:?} to report the checksum of its own \
                 bytes, got {delivered_checksum:?} against the source's \
                 {source_checksum:?}"
            );
        }
    }
}

/// The user metadata an S3 answer reports, ordered, so a case's assertion names every
/// entry the object carries.
fn user_metadata(
    metadata: Option<&::std::collections::HashMap<String, String>>,
) -> BTreeMap<String, String> {
    metadata
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}
