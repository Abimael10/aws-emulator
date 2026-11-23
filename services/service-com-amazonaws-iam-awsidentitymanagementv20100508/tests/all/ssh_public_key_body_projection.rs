// SPDX-License-Identifier: CC0-1.0

//! What `SSHPublicKeyBody` carries when a stored SSH public key is READ back, end to end over
//! the awsQuery wire.
//!
//! The rule under test: a read reports the KEY the account holds, written out in the encoding
//! the request asks for, and never the text that carried it in — so nothing a submitted body
//! carried BESIDE the key comes back (the comment token an OpenSSH line may end with, the PEM
//! label the material happened to be framed under), and one key reads back identically
//! however it arrived. The upload's OWN response is the other half of the contrast: it
//! reports the body as it was given, which is what a client that just sent it is told it
//! sent.
//!
//! The rows differ along what the rule generalises over: the encoding the key arrived in (an
//! OpenSSH `ssh-rsa` line, a `SubjectPublicKeyInfo` PEM, a PKCS#1 `RSAPublicKey` PEM), what
//! the line carried after the key (no comment, a one-token comment, a comment of several
//! words, a comment that looks like more key material), the key's length (the published
//! minimum and twice it) and the encoding the read asks for (both values `encodingType`
//! declares). No single request is the specification.
//!
//! The material fixtures (`modulus`, `openssh_line`, `spki_pem`, `pkcs1_pem`), the upload and
//! the user fixture are the ones `tests/all/ssh_public_keys.rs` owns, the read
//! `tests/all/ssh_public_key_reads.rs`'s, and the awsQuery harness
//! `tests/all/user_lifecycle.rs`'s.

use crate::ssh_public_key_reads::get;
use crate::ssh_public_keys::{
    create_user, modulus, openssh_line, pkcs1_pem, spki_pem, upload, PUBLISHED_MINIMUM_BITS,
};
use crate::user_lifecycle::element;

/// One row: the case, the key it is built on, the body the upload carries, and what that body
/// carried after the key (the empty string for a body that carries no comment at all).
struct Row {
    case: &'static str,
    material: Vec<u8>,
    body: String,
    comment: &'static str,
}

/// One row, from the key and the way that key is written out.
fn row(
    case: &'static str,
    material: Vec<u8>,
    comment: &'static str,
    write: impl Fn(&[u8], &str) -> String,
) -> Row {
    Row {
        body: write(&material, comment),
        case,
        material,
        comment,
    }
}

/// The rows: every form a body arrives in, with and without a trailing comment.
fn rows() -> Vec<Row> {
    vec![
        row(
            "an ssh-rsa line with no comment",
            modulus(PUBLISHED_MINIMUM_BITS, 201),
            "",
            openssh_line,
        ),
        row(
            "an ssh-rsa line with a one-token comment",
            modulus(PUBLISHED_MINIMUM_BITS, 202),
            "developer@workstation",
            openssh_line,
        ),
        row(
            "an ssh-rsa line with a comment of several words",
            modulus(PUBLISHED_MINIMUM_BITS, 203),
            "the laptop under the desk",
            openssh_line,
        ),
        row(
            "an ssh-rsa line whose comment looks like key material",
            modulus(PUBLISHED_MINIMUM_BITS, 204),
            "QmFzZTY0TG9va2luZ0NvbW1lbnQ",
            openssh_line,
        ),
        row(
            "an ssh-rsa line twice the published minimum length, with a comment",
            modulus(PUBLISHED_MINIMUM_BITS * 2, 205),
            "big-key@workstation",
            openssh_line,
        ),
        row(
            "a SubjectPublicKeyInfo PEM",
            modulus(PUBLISHED_MINIMUM_BITS, 206),
            "",
            |material, _| spki_pem(material),
        ),
        row(
            "a PKCS#1 RSA public key PEM",
            modulus(PUBLISHED_MINIMUM_BITS, 207),
            "",
            |material, _| pkcs1_pem(material),
        ),
        row(
            "a PKCS#1 RSA public key PEM twice the published minimum length",
            modulus(PUBLISHED_MINIMUM_BITS * 2, 208),
            "",
            |material, _| pkcs1_pem(material),
        ),
    ]
}

/// RULE: the read reports the key itself in the encoding asked for — the OpenSSH line for
/// `SSH` and the public-key PEM for `PEM`, both written out from the key the account holds —
/// so a body that arrived carrying a comment, or framed under the other PEM label, reads back
/// as the canonical rendering of that key and of nothing else. The upload's own response is
/// the contrast: it reports the body as it was given.
#[tokio::test]
async fn a_read_reports_the_key_rather_than_the_text_that_carried_it() {
    let router = crate::user_lifecycle::router();

    for (index, row) in rows().into_iter().enumerate() {
        let user_name = format!("bodyreader{index}");
        create_user(&router, row.case, &user_name).await;

        let (status, uploaded) = upload(&router, &user_name, &row.body).await;
        assert_eq!(
            status, 200,
            "case {:?}: the fixture upload must be served, got {status}: {uploaded}",
            row.case
        );
        // The upload echoes what it was handed, comment and PEM label included.
        assert_eq!(
            element(&uploaded, "SSHPublicKeyBody"),
            row.body,
            "case {:?}: expected the upload to report the body it was given, got {uploaded}",
            row.case
        );
        let held = element(&uploaded, "SSHPublicKeyId");

        // Asked for as an OpenSSH line: the key type and the key blob, and nothing after
        // them.
        let (status, read) = get(&router, &user_name, &held, "SSH").await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the SSH read to be served, got {status}: {read}",
            row.case
        );
        let ssh_body = element(&read, "SSHPublicKeyBody");
        assert_eq!(
            ssh_body,
            openssh_line(&row.material, ""),
            "case {:?}: expected the canonical ssh-rsa line for the stored key, got \
             {ssh_body:?}",
            row.case
        );
        assert_eq!(
            ssh_body.split_whitespace().count(),
            2,
            "case {:?}: expected the line to carry the key type and the blob alone, got \
             {ssh_body:?}",
            row.case
        );
        if !row.comment.is_empty() {
            assert!(
                !read.contains(row.comment),
                "case {:?}: expected nothing of the comment {:?} in the read, got {read}",
                row.case,
                row.comment
            );
        }

        // Asked for as PEM: the public-key PEM for the same key, whichever label the
        // material arrived under.
        let (status, read) = get(&router, &user_name, &held, "PEM").await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the PEM read to be served, got {status}: {read}",
            row.case
        );
        let pem_body = element(&read, "SSHPublicKeyBody");
        assert_eq!(
            pem_body,
            spki_pem(&row.material),
            "case {:?}: expected the public-key PEM for the stored key, got {pem_body:?}",
            row.case
        );
    }
}

/// RULE's invariant: the body a read reports is a function of the KEY alone, so the same key
/// uploaded under different comments — and under a different encoding altogether — reads back
/// byte for byte alike, and a different key reads back differently.
#[tokio::test]
async fn one_key_reads_back_alike_however_it_arrived() {
    let router = crate::user_lifecycle::router();
    let material = modulus(PUBLISHED_MINIMUM_BITS, 211);
    let other = modulus(PUBLISHED_MINIMUM_BITS, 212);

    // The SAME key written four ways, each uploaded for its own user because one user holds
    // one copy of a key, and a different key beside them.
    let arrivals: Vec<(&str, String, bool)> = vec![
        ("the key as a bare ssh-rsa line", openssh_line(&material, ""), true),
        (
            "the key as an ssh-rsa line with a comment",
            openssh_line(&material, "ops@bastion"),
            true,
        ),
        (
            "the key as a SubjectPublicKeyInfo PEM",
            spki_pem(&material),
            true,
        ),
        (
            "the key as a PKCS#1 RSA public key PEM",
            pkcs1_pem(&material),
            true,
        ),
        (
            "another key as an ssh-rsa line with the same comment",
            openssh_line(&other, "ops@bastion"),
            false,
        ),
    ];

    let mut same_key: Vec<String> = Vec::new();
    let mut different: Vec<String> = Vec::new();
    for (index, (case, body, is_the_same_key)) in arrivals.into_iter().enumerate() {
        let user_name = format!("bodyinvariant{index}");
        create_user(&router, case, &user_name).await;
        let (status, uploaded) = upload(&router, &user_name, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture upload must be served, got {status}: {uploaded}"
        );
        let held = element(&uploaded, "SSHPublicKeyId");
        for encoding in ["SSH", "PEM"] {
            let (status, read) = get(&router, &user_name, &held, encoding).await;
            assert_eq!(
                status, 200,
                "case {case:?} as {encoding}: expected the read to be served, got \
                 {status}: {read}"
            );
            let reported = element(&read, "SSHPublicKeyBody");
            if is_the_same_key {
                same_key.push(reported);
            } else {
                different.push(reported);
            }
        }
    }

    // Four arrivals of one key, read in two encodings: the SSH readings agree and so do the
    // PEM readings.
    for (position, reported) in same_key.iter().enumerate() {
        let encoding = if position % 2 == 0 { "SSH" } else { "PEM" };
        let first = &same_key[position % 2];
        assert_eq!(
            reported, first,
            "case {encoding:?} reading {position}: expected one key to read back alike \
             however it arrived, got {reported:?} against {first:?}"
        );
    }
    for (position, reported) in different.iter().enumerate() {
        let encoding = if position % 2 == 0 { "SSH" } else { "PEM" };
        assert_ne!(
            reported,
            &same_key[position % 2],
            "case {encoding:?}: expected a different key to read back differently, got \
             {reported:?}"
        );
    }
}
