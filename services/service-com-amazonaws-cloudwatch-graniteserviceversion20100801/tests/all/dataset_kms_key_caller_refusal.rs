// SPDX-License-Identifier: CC0-1.0

//! What `AssociateDatasetKmsKey` SAYS when the dry run it runs with the caller's own credentials
//! finds the calling principal without `kms:Decrypt` on the key the request names, end to end over
//! the wire with CloudWatch and AWS KMS on one engine.
//!
//! ONE rule, asserted over a table: **this service AUTHORS the refusal of its own dry run rather
//! than relaying the authorization decision KMS took — "The calling principal must have kms:Decrypt
//! permission on the key." and "a kms:Decrypt dry-run is run with the caller's credentials to
//! verify that the calling principal can use the new key"
//! (`smithy_query(query="doc",shape="AssociateDatasetKmsKey")`) — so the message names the key the
//! REQUEST named, the permission the CALLER's own policy has to grant on it, and the id of the
//! internal KMS call the dry run was made through, and the refusal leaves the dataset's
//! association exactly as it was.**
//!
//! What a client reads off such a refusal is which of the two policies to repair: the caller's
//! IAM policy, not the key's own condition (`dataset_kms_key_preconditions` tables the refusals
//! that ARE the key's condition). The key ARN is the request's own, the id is per request, and
//! lifting the deny lets the identical call through.
//!
//! The rows differ along what the rule generalises over: how the key policy spells the deny (the
//! calling principal's own ARN, the account root it reaches the key through, a wildcard action, a
//! set of actions), and whether the dataset already carries an association the refusal must leave
//! standing.

use serde_json::{json, Value};

use crate::dataset_kms_key_preconditions::{
    documented_policy, given_key_with, reported_key, service_statement, DOCUMENTED_PERMISSIONS,
};
use crate::harness::{call, error_code, error_message, Emulator};
use crate::sns_action_publish_grant::{emulator_with_kms, key_policy, key_statement, set_key_policy};

/// The wire error code every failure of this operation's dry run surfaces as, and the status the
/// half run with the CALLER's credentials carries — an authorization refusal over that principal
/// and that key.
const DENIED_CODE: &str = "AccessDenied";
const DENIED_STATUS: u16 = 403;

/// The one operation the caller's own dry run performs: "The calling principal must have
/// kms:Decrypt permission on the key." (`AssociateDatasetKmsKey`).
const CALLER_PERMISSION: &str = "kms:Decrypt";

/// The separator the refusal names the id of its internal KMS call after.
const KMS_REQUEST_ID_SEPARATOR: &str = " KMS request id: ";

/// The text this service states before that id, about the key `key_arn` the request named.
///
/// Shared with `dataset_kms_key_preconditions`, which asserts the same sentence from its own
/// table, so the two suites cannot disagree about the wording.
pub(crate) fn stated_refusal(key_arn: &str) -> String {
    format!(
        "Caller lacks {CALLER_PERMISSION} permission on KMS key {key_arn}. Update your IAM policy \
         to include {CALLER_PERMISSION} on this key."
    )
}

/// Assert that `message` is this service's refusal of the caller's own dry run over `key_arn`, and
/// answer the id of the internal KMS call it names.
///
/// The id is per request, so it is asserted as a FORM — the 8-4-4-4-12 shape an AWS request id
/// carries (`service_common::query_error_envelope::is_request_id`) — rather than as a value.
pub(crate) fn assert_caller_refusal(case: &str, message: &str, key_arn: &str) -> String {
    let (stated, tail) = message
        .split_once(KMS_REQUEST_ID_SEPARATOR)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the refusal must name the id of its internal KMS call after \
                 {KMS_REQUEST_ID_SEPARATOR:?}, got {message:?}"
            )
        });
    assert_eq!(
        stated,
        stated_refusal(key_arn),
        "case {case:?}: expected the refusal to state {:?}, got {stated:?}",
        stated_refusal(key_arn)
    );
    let id = tail.strip_suffix('.').unwrap_or_else(|| {
        panic!("case {case:?}: the id the refusal names closes the sentence, got {tail:?}")
    });
    assert!(
        service_common::query_error_envelope::is_request_id(id),
        "case {case:?}: expected the id of the internal KMS call to carry the form a request id \
         has, got {id:?}"
    );
    id.to_string()
}

/// How a row's key policy spells the deny of the caller's one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deny {
    /// The calling principal's own ARN, denied exactly the operation the dry run performs.
    TheCallersArn,
    /// The account root the calling principal reaches the key through, denied that operation —
    /// the statement a new key already carries, turned against it.
    TheAccountRoot,
    /// The calling principal, denied the operation through a prefix wildcard over the action
    /// name — "you can use a wildcard (*) … in an action name" (IAM's policy grammar, the same
    /// matcher `service_common::iam_authorization` applies).
    AWildcardOverTheOperation,
    /// The calling principal, denied a SET of operations the one it needs is among.
    ASetTheOperationIsAmong,
}

impl Deny {
    /// The key policy this deny is written into: the documented grant to this service's
    /// principal, so only the CALLER's half of the dry run can refuse the association.
    fn document(self) -> String {
        let caller = service_common::caller_principal::ambient_user_arn();
        let (principal, actions) = match self {
            Self::TheCallersArn => (json!({"AWS": caller}), json!(CALLER_PERMISSION)),
            Self::TheAccountRoot => (
                json!({"AWS": format!("arn:aws:iam::{}:root", engine::EMULATOR_ACCOUNT_ID)}),
                json!(CALLER_PERMISSION),
            ),
            Self::AWildcardOverTheOperation => (json!({"AWS": caller}), json!("kms:Decry*")),
            Self::ASetTheOperationIsAmong => (
                json!({"AWS": caller}),
                json!(["kms:ReEncryptFrom", CALLER_PERMISSION, "kms:GenerateDataKeyPair"]),
            ),
        };
        key_policy(vec![
            service_statement("Allow", json!(DOCUMENTED_PERMISSIONS), json!({})),
            key_statement("Deny", principal, actions, json!({})),
        ])
    }
}

/// Associate `key_arn` with this Region's default dataset, answering the status and body.
async fn associate(emulator: &Emulator, key_arn: &str) -> (u16, Value) {
    call(
        &emulator.router,
        "AssociateDatasetKmsKey",
        &json!({"DatasetIdentifier": "default", "KmsKeyArn": key_arn}),
    )
    .await
}

/// RULE: the refusal of the caller's own dry run names the key the request named and the
/// permission the caller's policy has to grant on it, and writes nothing.
#[tokio::test]
async fn the_callers_refusal_names_the_key_and_the_permission_to_grant() {
    for deny in [
        Deny::TheCallersArn,
        Deny::TheAccountRoot,
        Deny::AWildcardOverTheOperation,
        Deny::ASetTheOperationIsAmong,
    ] {
        let case = format!("a key policy denying the caller through {deny:?}");
        // One emulator per row, so the dataset each row reads starts with no association.
        let emulator = emulator_with_kms();
        let key_arn = given_key_with(&emulator, &case, &deny.document(), None).await;
        let (status, answered) = associate(&emulator, &key_arn).await;
        assert_eq!(
            status, DENIED_STATUS,
            "case {case:?}: expected HTTP {DENIED_STATUS}, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            DENIED_CODE,
            "case {case:?}: expected the {DENIED_CODE} code, got {answered}"
        );
        assert_caller_refusal(&case, &error_message(&answered), &key_arn);
        assert_eq!(
            reported_key(&emulator, &case).await,
            None,
            "case {case:?}: a refused association must leave the dataset with no key"
        );
    }
}

/// Each refusal names the id of ITS OWN internal KMS call: two identical calls carry two ids, so
/// the handle a client correlates on is per request rather than per key.
#[tokio::test]
async fn each_refusal_names_the_id_of_its_own_internal_kms_call() {
    let emulator = emulator_with_kms();
    let case = "two identical calls against one denied key";
    let key_arn = given_key_with(
        &emulator,
        case,
        &Deny::TheCallersArn.document(),
        None,
    )
    .await;

    let mut ids = Vec::new();
    for attempt in 1..=3 {
        let case = format!("{case} (attempt {attempt})");
        let (status, answered) = associate(&emulator, &key_arn).await;
        assert_eq!(
            status, DENIED_STATUS,
            "case {case:?}: expected HTTP {DENIED_STATUS}, got {status} with {answered}"
        );
        ids.push(assert_caller_refusal(
            &case,
            &error_message(&answered),
            &key_arn,
        ));
    }
    let mut distinct = ids.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "case {case:?}: each refusal names the id of its own KMS call, got {ids:?}"
    );
}

/// "If any of these checks on the new key fails, the operation fails and the existing key
/// association (if any) remains unchanged." — the refusal leaves the key the dataset already
/// carries in place, and lifting the deny lets the IDENTICAL call through.
#[tokio::test]
async fn a_refused_replacement_stands_until_the_deny_is_lifted() {
    let emulator = emulator_with_kms();
    let case = "replacing a usable key with one the caller may not use";
    let held = given_key_with(&emulator, case, &documented_policy(), None).await;
    let (status, answered) = associate(&emulator, &held).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first association must be accepted, got {status} with {answered}"
    );

    let denied = given_key_with(&emulator, case, &Deny::TheCallersArn.document(), None).await;
    let (status, answered) = associate(&emulator, &denied).await;
    assert_eq!(
        status, DENIED_STATUS,
        "case {case:?}: expected HTTP {DENIED_STATUS}, got {status} with {answered}"
    );
    assert_caller_refusal(case, &error_message(&answered), &denied);
    assert_eq!(
        reported_key(&emulator, case).await,
        Some(held.clone()),
        "case {case:?}: the dataset must still report the key it already carried"
    );

    // The key's owner lifts the deny, leaving the grant this service needs: the identical call
    // now takes effect, so what the refusal withheld was the caller's authority over that key
    // and nothing this service remembered about the request.
    set_key_policy(&emulator, case, &denied, &documented_policy()).await;
    let (status, answered) = associate(&emulator, &denied).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the identical call must be accepted once the deny is lifted, got {status} \
         with {answered}"
    );
    assert_eq!(
        reported_key(&emulator, case).await,
        Some(denied),
        "case {case:?}: the dataset must report the key the repaired call associated"
    );
}
