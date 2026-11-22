// SPDX-License-Identifier: CC0-1.0

//! Which record an `UpdateSSHPublicKey` writes, end to end over the awsQuery wire.
//!
//! The rule under test: "sets the status of an IAM user's SSH public key to active or
//! inactive" (`UpdateSSHPublicKey`) — the status of the ONE key the named user holds under
//! the named id becomes the value the request carries, so every other key the account holds
//! keeps the status it had, whoever holds it and however its material was written, and each
//! key reports the status its OWN update named.
//!
//! `tests/all/ssh_public_keys.rs` owns the rule for a user holding a single key (every value
//! of `statusType`, read back through each reader) and `tests/all/absent_key_and_role_wording.rs`
//! owns the sentence an id no key is reachable under is refused with. What is pinned here is
//! the dimension neither covers: how many keys the account holds around the one named, which
//! of them the update names, who holds them, and the encoding their material arrived in.
//!
//! The rows differ along exactly those: the number of keys the user holds (one, three, and
//! the published per-user quota), which of them the update names (the first, a middle one,
//! the last), the encoding each key's material was uploaded in, and whether another user
//! holds a key built from the SAME material. The material itself is built by the crates that
//! implement those encodings, through the fixtures `tests/all/ssh_public_keys.rs` owns, so no
//! row depends on a blob written out by hand.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the HTTP
//! behaviour of keys that were PERSISTED.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::MAX_SSH_PUBLIC_KEYS_PER_USER;

use crate::ssh_public_key_reads::get;
use crate::ssh_public_keys::{
    PUBLISHED_MINIMUM_BITS, create_user, modulus, openssh_line, spki_pem, upload,
};
use crate::user_lifecycle::{call, element, elements, router};

/// The status an upload holds a key at, before any update names another one.
const UPLOADED_STATUS: &str = "Active";

/// One `UpdateSSHPublicKey` call.
async fn update_status(
    router: &MultiServiceRouter,
    user_name: &str,
    ssh_public_key_id: &str,
    status: &str,
) -> (u16, String) {
    call(
        router,
        "UpdateSSHPublicKey",
        &[
            ("UserName", user_name),
            ("SSHPublicKeyId", ssh_public_key_id),
            ("Status", status),
        ],
    )
    .await
}

/// The status the key `ssh_public_key_id` reports to a read, which must be served.
async fn read_status(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    ssh_public_key_id: &str,
) -> String {
    let (status, read) = get(router, user_name, ssh_public_key_id, "SSH").await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read of {ssh_public_key_id} must be served, got {status}: {read}"
    );
    element(&read, "Status")
}

/// Material for the key at index `at` of row `row`, written in one encoding or the other so
/// a row spans both encodings an upload takes.
fn material(row: usize, at: usize) -> String {
    let modulus = modulus(PUBLISHED_MINIMUM_BITS, (row * 16 + at) as u8);
    if at.is_multiple_of(2) {
        openssh_line(&modulus, "")
    } else {
        spki_pem(&modulus)
    }
}

/// Create `user_name` and upload each of `bodies` to it, answering the minted ids in upload
/// order.
async fn user_holding(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    bodies: &[String],
) -> Vec<String> {
    create_user(router, case, user_name).await;
    let mut ids = Vec::with_capacity(bodies.len());
    for body in bodies {
        let (status, uploaded) = upload(router, user_name, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture upload must be served, got {status}: {uploaded}"
        );
        assert_eq!(
            element(&uploaded, "Status"),
            UPLOADED_STATUS,
            "case {case:?}: an uploaded key is {UPLOADED_STATUS}, got {uploaded}"
        );
        ids.push(element(&uploaded, "SSHPublicKeyId"));
    }
    ids
}

/// RULE: an update moves the status of the one key the named user holds under the named id,
/// and of no other key the account holds — not the same user's other keys, and not another
/// user's key, even one built from the same material.
#[tokio::test]
async fn an_update_moves_the_status_of_only_the_key_it_names() {
    let router = router();

    // (case, how many keys the user holds, which of them the update names, the status named)
    let rows: &[(&str, usize, usize, &str)] = &[
        ("the only key a user holds", 1, 0, "Inactive"),
        ("the first of three", 3, 0, "Expired"),
        ("the middle of three", 3, 1, "Inactive"),
        (
            "the last at the published per-user quota",
            MAX_SSH_PUBLIC_KEYS_PER_USER,
            MAX_SSH_PUBLIC_KEYS_PER_USER - 1,
            "Expired",
        ),
    ];

    for (row, (case, held, target, named)) in rows.iter().enumerate() {
        let user_name = format!("scopeholder{row}");
        let bodies: Vec<String> = (0..*held).map(|at| material(row, at)).collect();
        let ids = user_holding(&router, case, &user_name, &bodies).await;

        // A second user holding a key built from the SAME material as the one the update
        // names: a key another user holds is a different key, so nothing of it moves.
        let bystander = format!("scopebystander{row}");
        let bystander_ids =
            user_holding(&router, case, &bystander, &bodies[*target..=*target]).await;

        let (status, answered) = update_status(&router, &user_name, &ids[*target], named).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to be served, got {status}: {answered}"
        );

        for (at, id) in ids.iter().enumerate() {
            let expected = if at == *target {
                *named
            } else {
                UPLOADED_STATUS
            };
            let reported = read_status(&router, case, &user_name, id).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the key at {at} of {held} to report {expected:?}, got \
                 {reported:?}"
            );
        }

        for id in &bystander_ids {
            let reported = read_status(&router, case, &bystander, id).await;
            assert_eq!(
                reported, UPLOADED_STATUS,
                "case {case:?}: expected another user's key to keep {UPLOADED_STATUS}, got \
                 {reported:?}"
            );
        }
    }
}

/// RULE, per record: each of a user's keys holds the status its own update named, so updating
/// one after another leaves every key at the value its last update carried — the records are
/// independent, and the readers of the collection agree with the reads of each key.
#[tokio::test]
async fn each_key_holds_the_status_its_own_update_named() {
    let router = router();
    let case = "one update per key";
    let user_name = "scopeindependent";
    let bodies: Vec<String> = (0..MAX_SSH_PUBLIC_KEYS_PER_USER)
        .map(|at| material(9, at))
        .collect();
    let ids = user_holding(&router, case, user_name, &bodies).await;

    // One status per key, spanning every value `statusType` declares
    // (`smithy_query(query="shape", shape="statusType")`), including the value the upload
    // already holds a key at.
    let named: Vec<&str> = ["Inactive", "Active", "Expired", "Expired", "Inactive"]
        .into_iter()
        .take(ids.len())
        .collect();

    for (id, status_value) in ids.iter().zip(&named) {
        let (status, answered) = update_status(&router, user_name, id, status_value).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update of {id} to {status_value:?} to be served, got \
             {status}: {answered}"
        );
    }

    for ((at, id), status_value) in ids.iter().enumerate().zip(&named) {
        let reported = read_status(&router, case, user_name, id).await;
        assert_eq!(
            reported, *status_value,
            "case {case:?}: expected the key at {at} to report the status its own update named \
             ({status_value:?}), got {reported:?}"
        );
    }

    // The listing reads the same records, so it reports the same statuses beside the same ids.
    let (status, listed) = call(&router, "ListSSHPublicKeys", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the list must be served, got {status}: {listed}"
    );
    let mut listed_pairs: Vec<(String, String)> = elements(&listed, "SSHPublicKeyId")
        .into_iter()
        .zip(elements(&listed, "Status"))
        .collect();
    listed_pairs.sort();
    let mut expected_pairs: Vec<(String, String)> = ids
        .iter()
        .cloned()
        .zip(named.iter().map(ToString::to_string))
        .collect();
    expected_pairs.sort();
    assert_eq!(
        listed_pairs, expected_pairs,
        "case {case:?}: expected the listing to report each key's own status, got {listed}"
    );
}
