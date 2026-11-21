// SPDX-License-Identifier: CC0-1.0

//! The encryption a snapshot started from a `ParentSnapshotId` has, as both services that
//! report it describe it.
//!
//! RULE: "The encryption status of the snapshot depends on the values that you specify for
//! Encrypted, KmsKeyArn, and ParentSnapshotId"
//! (`smithy_query(query="doc",shape="com.amazonaws.ebs#StartSnapshotRequest",member="Encrypted")`,
//! repeated verbatim on `KmsKeyArn` and on `ParentSnapshotId`) — so a snapshot
//! `ebs:StartSnapshot` creates from a parent has the parent's encryption WHOLE: whether it is
//! encrypted, the server-side encryption type it reports and the key it is encrypted under,
//! all three together, however many generations down the lineage that encryption was
//! introduced. A request that names a `KmsKeyArn` encrypts the new snapshot under THAT key
//! instead, with or without a parent. Consequently the three facts a reader sees never
//! disagree: a snapshot reports an SSE type exactly when it is encrypted, and a key reference
//! only beside the KMS type.
//!
//! This suite lives beside the router because the rule spans two services. The EBS direct
//! APIs model no read of a snapshot's state and their `StartSnapshotResponse` carries no
//! `Encrypted` member at all, so "is this snapshot encrypted" is only ever observable through
//! `ec2:DescribeSnapshots`, which projects the very record `ebs:StartSnapshot` persisted —
//! and the EBS crate's own tests see no EC2. Both services are mounted on ONE engine here, as
//! `core/router/src/lib.rs` mounts them, through the wire helpers the sibling suite
//! `ebs_refused_block_write_then_ec2_snapshot_completion` exports.
//!
//! The rows differ along what the rule generalises over: which encryption the lineage carries
//! (none, the EBS-managed key, a KMS key of this partition and one of another, which is what
//! `KmsKeyArn`'s own pattern admits), WHERE in the lineage it was introduced (at the first
//! generation, midway down, and by the new snapshot itself), how MANY generations separate the
//! new snapshot from it (one, two and three), whether the request overrides the inherited key
//! with one of its own, the volume size each generation declares, how many blocks the lineage
//! holds, and the no-parent requests the same sentence covers.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ebs_refused_block_write_then_ec2_snapshot_completion::{
    checksum, complete_snapshot, describe, ebs, emulator, member, payload_for, put_block, Reply,
};
use crate::query_wire::{element, opt_element};

/// The Region every request of this suite is signed for, which is the Region a key ARN it
/// names lives in. The wire helpers sign for it; this is the same value, for the ARNs below.
const REGION: &str = "us-west-2";

/// The server-side encryption types the two models spell the same way: `SSEType`'s `sse-ebs`
/// (the EBS-managed key) and `sse-kms` (a caller-named KMS key)
/// (`smithy_query(query="shape",shape="com.amazonaws.ebs#SSEType")`).
const SSE_EBS: &str = "sse-ebs";
const SSE_KMS: &str = "sse-kms";

/// Key ids for the ARNs the rows name: a UUID, as KMS mints, and one in upper case, since a
/// key id is not what `KmsKeyArn`'s pattern constrains. No KMS is mounted in this emulator, so
/// a well-formed reference is accepted as given (the EBS crate's `state::kms_reference` says
/// why), and the suite that pins the resolving call is
/// `ebs_start_snapshot_resolves_its_kms_key`.
const KEY_ID_FIRST: &str = "1a2b3c4d-5e6f-4071-8293-a4b5c6d7e8f9";
const KEY_ID_SECOND: &str = "F00DCAFE-1111-4222-8333-444455556666";

/// A key ARN of ANOTHER partition, which `KmsKeyArn`'s pattern
/// `^arn:aws[a-z\-]*:kms:.*:[0-9]{12}:key/` admits as readily as the commercial one — so no
/// assertion here is a property of the `aws` partition.
const KEY_ARN_OTHER_PARTITION: &str =
    "arn:aws-us-gov:kms:us-gov-west-1:000000000000:key/9f8e7d6c-5b4a-4938-9706-fedcba987654";

/// The encryption one `StartSnapshot` request asks for, and equally the encryption a snapshot
/// ends up with: the two are the same vocabulary, which is what lets a row state its
/// expectation as one value instead of three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encryption {
    /// No encryption input at all, and no encryption: no type and no key either.
    Unencrypted,
    /// `Encrypted` without a key — the EBS-managed key, which no key reference names.
    EbsManaged,
    /// `KmsKeyArn` naming the key `key_id` of this emulator's account in [`REGION`].
    Key(&'static str),
    /// `KmsKeyArn` written out in full, for an ARN this emulator's own Region and partition
    /// do not produce.
    KeyArn(&'static str),
}

impl Encryption {
    /// Whether a snapshot with this encryption is encrypted.
    fn encrypted(self) -> bool {
        self != Self::Unencrypted
    }

    /// The `SseType` a snapshot with this encryption reports, or `None` where it reports
    /// none.
    fn sse_type(self) -> Option<&'static str> {
        match self {
            Self::Unencrypted => None,
            Self::EbsManaged => Some(SSE_EBS),
            Self::Key(_) | Self::KeyArn(_) => Some(SSE_KMS),
        }
    }

    /// The key ARN a snapshot with this encryption reports, or `None` where it reports none.
    fn key_arn(self) -> Option<String> {
        match self {
            Self::Unencrypted | Self::EbsManaged => None,
            Self::Key(key_id) => Some(format!(
                "arn:aws:kms:{REGION}:{account}:key/{key_id}",
                account = engine::EMULATOR_ACCOUNT_ID
            )),
            Self::KeyArn(arn) => Some(arn.to_string()),
        }
    }
}

/// One row: the encryption each generation of the lineage asks for, oldest first and empty for
/// a request with no parent at all; what the new snapshot itself asks for; the encryption it
/// must then have; the volume size the oldest generation declares; and the block indexes
/// written into that generation, which the lineage carries down with it.
struct Row {
    case: &'static str,
    lineage: &'static [Encryption],
    asks: Encryption,
    expected: Encryption,
    volume_size: i64,
    written: &'static [i32],
}

/// The rows, differing along which encryption the lineage carries, where it was introduced,
/// how far down the new snapshot sits from it, whether the request overrides it, the volume
/// sizes declared and the blocks the lineage holds.
const ROWS: &[Row] = &[
    Row {
        case: "an unencrypted parent, inherited by a request asking for nothing",
        lineage: &[Encryption::Unencrypted],
        asks: Encryption::Unencrypted,
        expected: Encryption::Unencrypted,
        volume_size: 1,
        written: &[],
    },
    Row {
        case: "the EBS-managed key inherited from the parent",
        lineage: &[Encryption::EbsManaged],
        asks: Encryption::Unencrypted,
        expected: Encryption::EbsManaged,
        volume_size: 1,
        written: &[0],
    },
    Row {
        case: "a KMS key inherited from the parent, key reference included",
        lineage: &[Encryption::Key(KEY_ID_FIRST)],
        asks: Encryption::Unencrypted,
        expected: Encryption::Key(KEY_ID_FIRST),
        volume_size: 2,
        written: &[0, 1],
    },
    Row {
        case: "a KMS key of another partition inherited from the parent",
        lineage: &[Encryption::KeyArn(KEY_ARN_OTHER_PARTITION)],
        asks: Encryption::Unencrypted,
        expected: Encryption::KeyArn(KEY_ARN_OTHER_PARTITION),
        volume_size: 3,
        written: &[7],
    },
    Row {
        case: "a request naming its own key over an EBS-managed parent",
        lineage: &[Encryption::EbsManaged],
        asks: Encryption::Key(KEY_ID_SECOND),
        expected: Encryption::Key(KEY_ID_SECOND),
        volume_size: 1,
        written: &[],
    },
    Row {
        case: "a request re-encrypting under another key than its KMS parent's",
        lineage: &[Encryption::Key(KEY_ID_FIRST)],
        asks: Encryption::Key(KEY_ID_SECOND),
        expected: Encryption::Key(KEY_ID_SECOND),
        volume_size: 4,
        written: &[2],
    },
    Row {
        case: "a request naming a key over an unencrypted parent",
        lineage: &[Encryption::Unencrypted],
        asks: Encryption::Key(KEY_ID_FIRST),
        expected: Encryption::Key(KEY_ID_FIRST),
        volume_size: 1,
        written: &[1],
    },
    Row {
        case: "two generations down from the EBS-managed key",
        lineage: &[Encryption::EbsManaged, Encryption::Unencrypted],
        asks: Encryption::Unencrypted,
        expected: Encryption::EbsManaged,
        volume_size: 1,
        written: &[0],
    },
    Row {
        case: "three generations down from a KMS key",
        lineage: &[
            Encryption::Key(KEY_ID_FIRST),
            Encryption::Unencrypted,
            Encryption::Unencrypted,
        ],
        asks: Encryption::Unencrypted,
        expected: Encryption::Key(KEY_ID_FIRST),
        volume_size: 2,
        written: &[3],
    },
    Row {
        case: "a key introduced midway down an unencrypted lineage",
        lineage: &[
            Encryption::Unencrypted,
            Encryption::Key(KEY_ID_SECOND),
            Encryption::Unencrypted,
        ],
        asks: Encryption::Unencrypted,
        expected: Encryption::Key(KEY_ID_SECOND),
        volume_size: 1,
        written: &[],
    },
    Row {
        case: "no parent at all, asking for the EBS-managed key",
        lineage: &[],
        asks: Encryption::EbsManaged,
        expected: Encryption::EbsManaged,
        volume_size: 1,
        written: &[],
    },
    Row {
        case: "no parent at all, asking for a KMS key",
        lineage: &[],
        asks: Encryption::Key(KEY_ID_FIRST),
        expected: Encryption::Key(KEY_ID_FIRST),
        volume_size: 5,
        written: &[],
    },
    Row {
        case: "no parent and no encryption input",
        lineage: &[],
        asks: Encryption::Unencrypted,
        expected: Encryption::Unencrypted,
        volume_size: 1,
        written: &[],
    },
];

/// The `StartSnapshot` body one generation sends: the size it declares, the parent it starts
/// from and the encryption it asks for.
///
/// `Encrypted` and `ParentSnapshotId` cannot be sent together ("If you specify both
/// parameters, the request fails with ValidationException"), so a row that asked for the
/// EBS-managed key under a parent would be testing that refusal instead of this rule — the
/// guard below keeps the table honest about which one it states.
fn start_body(volume_size: i64, parent: Option<&str>, encryption: Encryption) -> Value {
    assert!(
        parent.is_none() || encryption != Encryption::EbsManaged,
        "a row asking for the EBS-managed key beside a ParentSnapshotId states the refusal of \
         that combination, not the inheritance rule"
    );
    let mut body = json!({ "VolumeSize": volume_size });
    if let Some(parent) = parent {
        body["ParentSnapshotId"] = json!(parent);
    }
    match encryption {
        Encryption::Unencrypted => {}
        Encryption::EbsManaged => body["Encrypted"] = json!(true),
        Encryption::Key(_) | Encryption::KeyArn(_) => {
            body["KmsKeyArn"] = json!(encryption
                .key_arn()
                .expect("an encryption asking for a key names one"));
        }
    }
    body
}

/// `ebs:StartSnapshot`, asserting the modelled 201 and answering the whole reply.
async fn start_snapshot(router: &MultiServiceRouter, case: &str, body: &Value) -> Reply {
    let reply = ebs(
        router,
        "POST",
        "/snapshots",
        &[("content-type", "application/json".to_string())],
        serde_json::to_vec(body).expect("a serializable StartSnapshot body"),
    )
    .await;
    assert_eq!(
        reply.status,
        201,
        "case {case:?}: ebs:StartSnapshot of {body} must answer 201, got {} with {}",
        reply.status,
        reply.text()
    );
    reply
}

/// The `SnapshotId` a `StartSnapshot` reply reports.
fn snapshot_id(case: &str, reply: &Reply) -> String {
    reply
        .json()
        .get("SnapshotId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartSnapshot reported no SnapshotId, got {}",
                reply.text()
            )
        })
        .to_string()
}

/// An optional member of a `StartSnapshot` reply, or `None` where the reply omits it.
fn opt_member(reply: &Reply, name: &str) -> Option<String> {
    reply
        .json()
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `ebs:PutSnapshotBlock` of a full, correctly checksummed block at `index`, through the
/// sibling suite's own request construction.
async fn write_block(router: &MultiServiceRouter, snapshot_id: &str, index: i32) -> Reply {
    let payload = payload_for(index);
    let supplied = checksum(&payload);
    put_block(router, snapshot_id, index, payload, &supplied).await
}

/// What `ec2:DescribeSnapshots` reports about one snapshot's encryption: the three members
/// that are the same fact, beside the answer they were read from.
struct Reported {
    encrypted: String,
    sse_type: Option<String>,
    kms_key_id: Option<String>,
    body: String,
}

/// The encryption `ec2:DescribeSnapshots` reports for `snapshot_id` — the read through which a
/// client sees whether a direct-API snapshot is encrypted at all.
async fn reported(router: &MultiServiceRouter, case: &str, snapshot_id: &str) -> Reported {
    let (status, body) = describe(router, snapshot_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ec2:DescribeSnapshots must report {snapshot_id}, got {status} with {body}"
    );
    assert_eq!(
        member(&body, "snapshotId"),
        snapshot_id,
        "case {case:?}: the read must report the snapshot it was asked for, got {body}"
    );
    Reported {
        encrypted: element(&body, "encrypted"),
        sse_type: opt_element(&body, "sseType"),
        kms_key_id: opt_element(&body, "kmsKeyId"),
        body,
    }
}

/// Assert that one snapshot has `expected` as BOTH services report it, and that the three
/// facts cohere: a snapshot reports an SSE type exactly when it is encrypted, and a key
/// reference only beside the KMS type.
async fn assert_encryption(
    router: &MultiServiceRouter,
    case: &str,
    stage: &str,
    snapshot_id: &str,
    start: Option<&Reply>,
    expected: Encryption,
) {
    if let Some(reply) = start {
        assert_eq!(
            opt_member(reply, "SseType").as_deref(),
            expected.sse_type(),
            "case {case:?} ({stage}): StartSnapshot must report the SseType of the snapshot it \
             stored, got {}",
            reply.text()
        );
        assert_eq!(
            opt_member(reply, "KmsKeyArn"),
            expected.key_arn(),
            "case {case:?} ({stage}): StartSnapshot must report the key the snapshot is \
             encrypted under, got {}",
            reply.text()
        );
    }

    let seen = reported(router, case, snapshot_id).await;
    assert_eq!(
        seen.encrypted,
        expected.encrypted().to_string(),
        "case {case:?} ({stage}): expected encrypted {}, got {}",
        expected.encrypted(),
        seen.body
    );
    assert_eq!(
        seen.sse_type.as_deref(),
        expected.sse_type(),
        "case {case:?} ({stage}): expected sseType {:?}, got {}",
        expected.sse_type(),
        seen.body
    );
    assert_eq!(
        seen.kms_key_id,
        expected.key_arn(),
        "case {case:?} ({stage}): expected kmsKeyId {:?}, got {}",
        expected.key_arn(),
        seen.body
    );

    // The invariant the three members hold between them, asserted on the values that came
    // back rather than on the row: an unencrypted snapshot with a server-side encryption
    // type, or a key beside no type, is a record no reader can interpret.
    assert_eq!(
        seen.encrypted == "true",
        seen.sse_type.is_some(),
        "case {case:?} ({stage}): a snapshot reports an SSE type exactly when it is \
         encrypted, got {}",
        seen.body
    );
    assert!(
        seen.kms_key_id.is_none() || seen.sse_type.as_deref() == Some(SSE_KMS),
        "case {case:?} ({stage}): a key reference belongs only beside the {SSE_KMS} type, \
         got {}",
        seen.body
    );
}

/// RULE: every generation of a lineage has the encryption its parent had, unless its own
/// request names a key; and the three facts that encryption is reported through always agree.
#[tokio::test]
async fn a_snapshot_started_from_a_parent_has_the_parents_whole_encryption() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();

        // The lineage, oldest first: each generation starts from the one before it, carries
        // the blocks written into the oldest, and is SEALED — only a completed snapshot may
        // be used as a ParentSnapshotId. Every generation's own encryption is asserted too,
        // so a row's premise is pinned rather than assumed.
        let mut parent: Option<String> = None;
        let mut carried = Encryption::Unencrypted;
        for (generation, asks) in row.lineage.iter().enumerate() {
            let stage = format!("generation {generation}");
            let size = row.volume_size + i64::try_from(generation).expect("a generation index");
            let reply = start_snapshot(
                &router,
                case,
                &start_body(size, parent.as_deref(), *asks),
            )
            .await;
            let id = snapshot_id(case, &reply);

            // A key the generation names is its own encryption; otherwise it carries down
            // whatever the generation above had.
            if *asks != Encryption::Unencrypted {
                carried = *asks;
            }
            assert_encryption(&router, case, &stage, &id, Some(&reply), carried).await;

            // Only the OLDEST generation writes blocks of its own; the rest inherit them, so
            // each later completion declares none written to it.
            let mut written = 0;
            if generation == 0 {
                for index in row.written {
                    let put = write_block(&router, &id, *index).await;
                    assert_eq!(
                        put.status,
                        201,
                        "case {case:?} ({stage}): a block write at {index} must answer 201, got \
                         {} with {}",
                        put.status,
                        put.text()
                    );
                    written += 1;
                }
            }
            let sealed = complete_snapshot(&router, &id, written).await;
            assert_eq!(
                sealed.status,
                202,
                "case {case:?} ({stage}): CompleteSnapshot must answer 202, got {} with {}",
                sealed.status,
                sealed.text()
            );
            parent = Some(id);
        }

        // The snapshot the rule is about: started from the last generation, or from no parent
        // at all for the rows that carry none.
        let size = row.volume_size + i64::try_from(row.lineage.len()).expect("a lineage length");
        let reply = start_snapshot(&router, case, &start_body(size, parent.as_deref(), row.asks))
            .await;
        let id = snapshot_id(case, &reply);
        assert_eq!(
            opt_member(&reply, "ParentSnapshotId"),
            parent.clone(),
            "case {case:?}: StartSnapshot must report the parent it was started from, got {}",
            reply.text()
        );
        assert_encryption(&router, case, "the new snapshot", &id, Some(&reply), row.expected).await;

        // The record carries it, not just the create's answer: the same encryption is read
        // back after the snapshot is sealed, through the only read that reports it.
        let sealed = complete_snapshot(&router, &id, 0).await;
        assert_eq!(
            sealed.status,
            202,
            "case {case:?}: CompleteSnapshot of the new snapshot must answer 202, got {} with {}",
            sealed.status,
            sealed.text()
        );
        assert_encryption(&router, case, "sealed and read back", &id, None, row.expected).await;

        // Creating a snapshot FROM a parent leaves that parent exactly as it was: the
        // inheritance is a read of the parent, not a change to it.
        if let Some(parent) = parent.as_deref() {
            assert_encryption(&router, case, "the parent afterwards", parent, None, carried).await;
        }
    }
}
