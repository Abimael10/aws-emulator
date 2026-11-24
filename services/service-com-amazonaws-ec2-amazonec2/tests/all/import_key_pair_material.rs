// SPDX-License-Identifier: CC0-1.0

//! What an `ImportKeyPair` reads the public key material AS, and what a later read reports
//! the key pair's public key AS — the two halves of one rule, driven over the ec2Query wire
//! this service speaks.
//!
//! "Imports the public key from an RSA or ED25519 key pair that you created using a
//! third-party tool" (`ImportKeyPair`), and the import requirements it points a caller at
//! name the two public-key forms a key may arrive in: the single-LINE
//! `key-type key-body [comment]` form `ssh-keygen` writes, and the armoured "SSH public key
//! file format as specified in RFC 4716". So:
//!
//! * the material carries ONE key and nothing else, in the form that key's own ALGORITHM is
//!   read in — an ED25519 public key in the OpenSSH line form, which is the form EC2 names
//!   when it reports this algorithm's material unreadable, the armoured file carrying the RSA
//!   keys whose lengths those requirements publish beside it. Material with a second key, with
//!   prose or a blank line on a line of its own, or presenting an ED25519 key in that armour is
//!   refused before anything is stored, with the code and the one sentence EC2 words that
//!   error class with; and
//! * "the public key material" (`KeyPairInfo$PublicKey`) a read reports is the imported KEY as
//!   the one-line OpenSSH public key commented with the key PAIR's name — the algorithm and
//!   the body are the imported key's own, whichever form it arrived in, and the comment is the
//!   name the pair is held under rather than whatever the file it was read out of carried.
//!
//! The rows of each table differ along what those rules generalise over — where the extra
//! content sits, which algorithm the key is, which form it arrived in, and how many words the
//! comment the material carried has — so no row is the specification and the one material a
//! recording happens to carry is one case among them.
//!
//! [`super::import_key_pair`] is the import's own suite — the algorithms, the published RSA
//! lengths and the two fingerprint rules — and this file builds its rows with that suite's
//! builders rather than second copies of them. [`super::key_pairs`] covers the same reported
//! public key for a pair this service GENERATED.

use ::base64::Engine as _;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{error_code, error_message};
use super::import_key_pair::{
    described_pair, ed25519_blob, expected_ed25519_fingerprint, expected_rsa_fingerprint, import,
    import_params, openssh_line, rfc4716_file, rsa_blob,
};
use super::wire::{child_text, document, param};
use service_com_amazonaws_ec2_amazonec2 as ec2;

/// The Region every call here is signed for: a key pair carries no Region member, so the
/// Region only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// The one key pair `DescribeKeyPairs` reports for `key_name`, with its public key material,
/// as the `(keyFingerprint, publicKey)` pair of that item.
///
/// Read through the describe rather than out of the store, so what is asserted is what a
/// client sees on the wire — and read out of the body by the sibling suite's one reader of a
/// reported key pair ([`described_pair`]).
async fn described(case: &str, router: &MultiServiceRouter, key_name: &str) -> (String, String) {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeKeyPairs",
        &[
            param("KeyName.1", key_name),
            param("IncludePublicKey", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe of {key_name:?} must succeed, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    let item = described_pair(case, &parsed);
    (
        child_text(item, "keyFingerprint")
            .unwrap_or_else(|| panic!("case {case:?}: no <keyFingerprint>: {body}")),
        child_text(item, "publicKey")
            .unwrap_or_else(|| panic!("case {case:?}: no <publicKey>: {body}")),
    )
}

/// How many key pairs the account holds, as the unscoped describe reports them.
async fn stored_key_pairs(case: &str, router: &MultiServiceRouter) -> usize {
    let (status, body) = super::ec2_query::call(router, REGION, "DescribeKeyPairs", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must succeed, got {status} with body {body}"
    );
    super::ec2_query::elements(&body, "keyPairId").len()
}

/// One import that has to be refused with the code and the sentence EC2 words for material it
/// cannot read as a key.
async fn refused(case: &str, router: &MultiServiceRouter, key_name: &str, material: &str) {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "ImportKeyPair",
        &import_params(key_name, material, &[]),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: material EC2 cannot read as one key must be refused, got {status} \
         with body {body}"
    );
    assert_eq!(
        error_code(&body),
        ec2::state::INVALID_KEY_FORMAT,
        "case {case:?}: expected the code {}, got {body}",
        ec2::state::INVALID_KEY_FORMAT
    );
    assert_eq!(
        error_message(&body),
        ec2::state::UNREADABLE_PUBLIC_KEY_MESSAGE,
        "case {case:?}: expected the sentence EC2 words this class with, got {}",
        error_message(&body)
    );
}

/// One import that has to succeed, and the body it answered: the sibling suite's own import
/// hop ([`import`]), with no tags.
async fn imported(
    case: &str,
    router: &MultiServiceRouter,
    key_name: &str,
    material: &str,
) -> String {
    import(case, router, key_name, material, &[]).await
}

/// Material that carries a readable key AND something else, each row putting that something
/// else somewhere different.
///
/// Every row's key is a key an import takes on its own — the positive rows of
/// [`a_single_key_is_what_the_material_may_carry`] import exactly it — so what each row
/// varies is only what accompanies it.
fn extra_content_table() -> Vec<(&'static str, String)> {
    let blob = ed25519_blob(71);
    let line = openssh_line("ssh-ed25519", &blob, "fixture@example.com");
    let other = openssh_line("ssh-rsa", &rsa_blob(2048, 72), "second@example.com");
    let body = ::base64::engine::general_purpose::STANDARD.encode(&blob);
    vec![
        (
            "a second key on a line of its own",
            format!("{line}\n{other}"),
        ),
        (
            "the same key twice, as an authorized_keys file holds two",
            format!("{line}\n{line}\n"),
        ),
        (
            "prose after the key",
            format!("{line}\nthe key above is the one to import\n"),
        ),
        (
            "a line before the key",
            format!("# the key for the fixture\n{line}"),
        ),
        // A key line is read from the first byte of the material: nothing precedes it, not
        // even the blank line a tool may have written before it.
        ("a blank line before the key", format!("\n{line}")),
        (
            "the key's own body broken across lines",
            format!(
                "ssh-ed25519 {head}\n{tail} fixture@example.com",
                head = &body[..body.len() / 2],
                tail = &body[body.len() / 2..]
            ),
        ),
        (
            "an armoured file with a key line after it",
            format!("{armour}{line}\n", armour = rfc4716_file(&rsa_blob(2048, 73))),
        ),
    ]
}

/// One row of the reported-public-key table: the case, the key pair's name, the material the
/// request carries, and the key that material carries — its algorithm token and its blob,
/// which the reported line is built from.
struct MaterialCase {
    case: &'static str,
    key_name: &'static str,
    material: String,
    algorithm: &'static str,
    blob: Vec<u8>,
    key_type: &'static str,
    fingerprint: String,
}

/// The materials whose key a read has to report under the pair's own name: both algorithms,
/// both forms the import requirements name, and the comment field in each of the shapes an
/// OpenSSH line carries it in — including none at all.
fn material_table() -> Vec<MaterialCase> {
    let ed25519 = ed25519_blob(81);
    let rsa = rsa_blob(2048, 82);
    let ed25519_row = |case: &'static str, key_name: &'static str, comment: &str| MaterialCase {
        case,
        key_name,
        material: openssh_line("ssh-ed25519", &ed25519, comment),
        algorithm: "ssh-ed25519",
        blob: ed25519.clone(),
        key_type: ec2::state::KEY_TYPE_ED25519,
        fingerprint: expected_ed25519_fingerprint(&ed25519),
    };
    vec![
        ed25519_row(
            "a comment of one word",
            "material-one-word",
            "fixture-kp-4c1e",
        ),
        ed25519_row(
            "a comment of several words",
            "material-several-words",
            "the fixture's own key",
        ),
        ed25519_row(
            "a comment that is an address",
            "material-address",
            "fixture-probe-kp-4c1e@host.example",
        ),
        ed25519_row("no comment at all", "material-no-comment", ""),
        // The SAME key as the rows above, imported a second time under another name: one key,
        // two pairs, and each pair reports the key under ITS OWN name.
        ed25519_row(
            "the same key under a second name",
            "material-second-name",
            "fixture-kp-4c1e",
        ),
        MaterialCase {
            case: "the line a key file holds, newline terminated",
            key_name: "material-terminated",
            material: format!("{}\n", openssh_line("ssh-ed25519", &ed25519, "fixture@host")),
            algorithm: "ssh-ed25519",
            blob: ed25519.clone(),
            key_type: ec2::state::KEY_TYPE_ED25519,
            fingerprint: expected_ed25519_fingerprint(&ed25519),
        },
        MaterialCase {
            case: "an rsa key line with a comment",
            key_name: "material-rsa",
            material: openssh_line("ssh-rsa", &rsa, "fixture-rsa-kp"),
            algorithm: "ssh-rsa",
            blob: rsa.clone(),
            key_type: ec2::state::KEY_TYPE_RSA,
            fingerprint: expected_rsa_fingerprint(2048, 82),
        },
        MaterialCase {
            case: "the armoured file of that same rsa key",
            key_name: "material-rsa-armoured",
            material: rfc4716_file(&rsa),
            algorithm: "ssh-rsa",
            blob: rsa,
            key_type: ec2::state::KEY_TYPE_RSA,
            fingerprint: expected_rsa_fingerprint(2048, 82),
        },
    ]
}

#[tokio::test]
async fn a_single_key_is_what_the_material_may_carry() {
    let router = emulator();

    for (index, (case, material)) in extra_content_table().into_iter().enumerate() {
        refused(case, &router, &format!("extra-{index}"), &material).await;
    }

    // Nothing above reached the store: a refusal happens before anything is written.
    assert_eq!(
        stored_key_pairs("nothing stored", &router).await,
        0,
        "material EC2 cannot read must store no key pair"
    );

    // The SAME key, alone, is imported — which is what makes the rows above a rule about what
    // ACCOMPANIES the key rather than a refusal of the key itself. The rows differ in the line
    // terminator the file the key was read out of ended with, which carries no second key.
    let line = openssh_line("ssh-ed25519", &ed25519_blob(71), "fixture@example.com");
    for (index, (case, material)) in [
        ("the line alone", line.clone()),
        ("the line, newline terminated", format!("{line}\n")),
        (
            "the line, terminated as a windows tool writes it",
            format!("{line}\r\n"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        imported(case, &router, &format!("single-{index}"), &material).await;
    }
}

#[tokio::test]
async fn an_ed25519_key_is_read_in_the_openssh_form() {
    let router = emulator();
    let ed25519 = ed25519_blob(91);

    // The armoured file of an ED25519 key, with the headers RFC 4716 allows and without any:
    // the form is not the one this algorithm's keys are read in, whatever the file carries.
    for (index, (case, material)) in [
        ("an ed25519 key in the armoured file", rfc4716_file(&ed25519)),
        (
            "an ed25519 key in a headerless armoured file",
            format!(
                "---- BEGIN SSH2 PUBLIC KEY ----\n{body}\n---- END SSH2 PUBLIC KEY ----\n",
                body = ::base64::engine::general_purpose::STANDARD.encode(&ed25519)
            ),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        refused(
            case,
            &router,
            &format!("armoured-ed25519-{index}"),
            &material,
        )
        .await;
    }
    assert_eq!(
        stored_key_pairs("nothing stored", &router).await,
        0,
        "an ED25519 key in a form it is not read in must store no key pair"
    );

    // That same key, as the OpenSSH line, imports — and so does an RSA key in the armoured
    // file, so the rows above are about the FORM this algorithm is read in rather than about
    // the armour or about the key.
    imported(
        "the same ed25519 key as an openssh line",
        &router,
        "armoured-twin-line",
        &openssh_line("ssh-ed25519", &ed25519, "fixture@example.com"),
    )
    .await;
    let case = "an rsa key in the armoured file";
    let rsa = rsa_blob(2048, 92);
    imported(case, &router, "armoured-rsa", &rfc4716_file(&rsa)).await;
    let (_, armoured_public_key) = described(case, &router, "armoured-rsa").await;
    assert_eq!(
        armoured_public_key,
        openssh_line("ssh-rsa", &rsa, "armoured-rsa"),
        "case {case:?}: an rsa key imported in the armoured file must be reported as that \
         key's own line, got {armoured_public_key:?}"
    );
}

#[tokio::test]
async fn a_read_reports_the_key_pairs_public_key_under_the_pairs_name() {
    let router = emulator();

    for row in material_table() {
        let case = row.case;
        let body = imported(case, &router, row.key_name, &row.material).await;
        let imported_fingerprint = super::ec2_query::element(&body, "keyFingerprint")
            .unwrap_or_else(|| panic!("case {case:?}: no <keyFingerprint> in {body}"));
        assert_eq!(
            imported_fingerprint, row.fingerprint,
            "case {case:?}: the import must report the fingerprint this key's own type rule \
             gives it, got {imported_fingerprint}"
        );

        // The key the material carried, under the name the pair is held as: the comment the
        // material itself carried is not what a read reports.
        let expected = openssh_line(row.algorithm, &row.blob, row.key_name);
        let (fingerprint, public_key) = described(case, &router, row.key_name).await;
        assert_eq!(
            public_key, expected,
            "case {case:?}: the reported public key must be the imported key commented with \
             the pair's name, got {public_key:?}"
        );
        assert_eq!(
            fingerprint, row.fingerprint,
            "case {case:?}: one key pair has one fingerprint however it is read, got \
             {fingerprint}"
        );
        // The ED25519 rule in full: "the base64-encoded SHA-256 digest" is the digest,
        // base64-encoded — padding and all, and carrying no algorithm name of its own.
        if row.key_type == ec2::state::KEY_TYPE_ED25519 {
            assert!(
                !fingerprint.contains(':') && fingerprint.ends_with('='),
                "case {case:?}: an imported ED25519 pair's fingerprint is the padded base64 \
                 digest with no algorithm name, got {fingerprint}"
            );
        }
    }
}
