// SPDX-License-Identifier: CC0-1.0

//! What a `DeleteSSHPublicKey` takes away, measured against the per-user bound on what an
//! IAM user may hold, end to end over the awsQuery wire.
//!
//! RULE: "deletes the specified SSH public key" (`DeleteSSHPublicKey`) removes the ONE
//! record the named user holds under the named id from the collection every reader of that
//! user's keys reads, so what the account holds for that user afterwards is one record
//! fewer — and every question asked of that collection is asked of what it holds NOW: the
//! bound on how many keys one user may hold, and whether the user already holds a given
//! public key. A delete therefore frees exactly the slot the key it removed occupied, and a
//! delete that removes nothing frees none.
//!
//! `tests/all/ssh_public_keys.rs` owns the rule that a deleted key stops being reported and
//! that a delete naming no key the user holds is refused `NoSuchEntity`; this suite pins the
//! dimension it does not cover: what the delete leaves the per-user quota and the
//! duplicate-key test standing at.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: how many
//! keys the user holds when the delete arrives (one, three, the published quota), which of
//! them it names (the first, a middle one, the last), the encoding and the length of each
//! key's material, and how many of the user's keys are deleted before the next upload. The
//! material is built by the crates that implement those encodings, through the fixtures
//! `tests/all/ssh_public_keys.rs` owns, so no row depends on a blob written out by hand.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the
//! HTTP behaviour of keys that were PERSISTED.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::MAX_SSH_PUBLIC_KEYS_PER_USER;

use crate::ssh_public_key_reads::get;
use crate::ssh_public_keys::{
    PUBLISHED_MINIMUM_BITS, create_user, modulus, openssh_line, spki_pem, upload,
};
use crate::user_lifecycle::{call, element, elements, error_code, router};

/// One `DeleteSSHPublicKey` call.
async fn delete(
    router: &MultiServiceRouter,
    user_name: &str,
    ssh_public_key_id: &str,
) -> (u16, String) {
    call(
        router,
        "DeleteSSHPublicKey",
        &[
            ("UserName", user_name),
            ("SSHPublicKeyId", ssh_public_key_id),
        ],
    )
    .await
}

/// The ids `ListSSHPublicKeys` reports for `user_name`, sorted, from a listing that must be
/// served.
async fn listed_ids(router: &MultiServiceRouter, case: &str, user_name: &str) -> Vec<String> {
    let (status, listed) = call(router, "ListSSHPublicKeys", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing of {user_name}'s keys must be served, got {status}: {listed}"
    );
    let mut ids = elements(&listed, "SSHPublicKeyId");
    ids.sort();
    ids
}

/// The material for slot `slot` of group `group`: a public key of a length the upload takes,
/// written in one of the two encodings it takes — so the keys of one user differ in both
/// their length and the encoding that carried them.
///
/// One key per `(group, slot)` pair and no two pairs the same key, so an upload of this
/// suite is never refused as a duplicate of another: the pair picks the seed the modulus is
/// built from, and groups stay under 16 so no pair's seed wraps onto another's.
fn material(group: usize, slot: usize) -> String {
    let seed = (group * 16 + slot + 1) as u8;
    let bits = if seed.is_multiple_of(3) {
        PUBLISHED_MINIMUM_BITS
    } else {
        3072
    };
    let modulus = modulus(bits, seed);
    if seed.is_multiple_of(2) {
        openssh_line(&modulus, "")
    } else {
        spki_pem(&modulus)
    }
}

/// Upload `bodies` to `user_name`, which the fixture creates, answering the minted ids in
/// upload order.
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
        ids.push(element(&uploaded, "SSHPublicKeyId"));
    }
    ids
}

/// RULE: the delete takes exactly the record it names out of the collection, so the user
/// holds one key fewer — the slot it occupied is free for another upload, and the material
/// it carried is no longer a key THAT user holds, so it can be associated again under a new
/// id.
#[tokio::test]
async fn a_delete_frees_the_slot_and_the_material_of_the_key_it_removed() {
    let router = router();

    // (case, how many keys the user holds, which of them the delete names)
    let rows: &[(&str, usize, usize)] = &[
        ("the only key a user holds", 1, 0),
        ("the first of three", 3, 0),
        ("the middle of three", 3, 1),
        ("the last of three", 3, 2),
        (
            "one held at the published per-user quota",
            MAX_SSH_PUBLIC_KEYS_PER_USER,
            MAX_SSH_PUBLIC_KEYS_PER_USER / 2,
        ),
    ];

    for (row, (case, held, target)) in rows.iter().enumerate() {
        let user_name = format!("freeholder{row}");
        let bodies: Vec<String> = (0..*held).map(|at| material(row, at)).collect();
        let ids = user_holding(&router, case, &user_name, &bodies).await;

        let (status, answered) = delete(&router, &user_name, &ids[*target]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to be served, got {status}: {answered}"
        );

        // One record fewer, and the one that went is the one the delete named.
        let mut expected: Vec<String> = ids.clone();
        expected.remove(*target);
        expected.sort();
        assert_eq!(
            listed_ids(&router, case, &user_name).await,
            expected,
            "case {case:?}: expected the user to hold its other {} key(s)",
            held - 1
        );
        let (status, read) = get(&router, &user_name, &ids[*target], "SSH").await;
        assert_eq!(
            (status, error_code(&read)),
            (404, "NoSuchEntity".to_string()),
            "case {case:?}: expected the deleted key to be unreadable, got {status}: {read}"
        );

        // The material the deleted key carried is not a key this user holds any more, so
        // associating it again is an upload rather than the duplicate refusal — and it is a
        // NEW record, under a newly minted id.
        let (status, reuploaded) = upload(&router, &user_name, &bodies[*target]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the deleted material to be uploadable again, got \
             {status}: {reuploaded}"
        );
        let reuploaded_id = element(&reuploaded, "SSHPublicKeyId");
        assert_ne!(
            reuploaded_id, ids[*target],
            "case {case:?}: expected a newly minted id for the re-associated key, got \
             {reuploaded_id:?}"
        );

        // That brought the user back to the count it held before the delete, so the next
        // upload is answered by the bound the user now stands at: served while the user is
        // inside the quota, refused once it is at it.
        let (status, following) = upload(&router, &user_name, &material(row, *held)).await;
        if *held >= MAX_SSH_PUBLIC_KEYS_PER_USER {
            assert_eq!(
                (status, error_code(&following)),
                (409, "LimitExceeded".to_string()),
                "case {case:?}: expected the upload past the quota to be refused, got \
                 {status}: {following}"
            );
            assert_eq!(
                listed_ids(&router, case, &user_name).await.len(),
                MAX_SSH_PUBLIC_KEYS_PER_USER,
                "case {case:?}: a refused upload must store nothing"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected an upload inside the quota to be served, got \
                 {status}: {following}"
            );
            assert_eq!(
                listed_ids(&router, case, &user_name).await.len(),
                held + 1,
                "case {case:?}: expected the user to hold one key more"
            );
        }
    }
}

/// RULE, at the bound: the per-user quota is taken against what the user holds NOW, so a
/// user standing at it admits exactly as many further uploads as deletes removed keys from
/// it — and a delete that removes nothing frees nothing.
#[tokio::test]
async fn the_quota_admits_exactly_as_many_uploads_as_deletes_freed_slots() {
    // (case, how many of the user's keys are deleted before the next uploads)
    let rows: &[(&str, usize)] = &[
        ("one slot freed", 1),
        ("two slots freed", 2),
        ("every slot freed", MAX_SSH_PUBLIC_KEYS_PER_USER),
    ];

    for (row, (case, freed)) in rows.iter().enumerate() {
        // One router per row, so what each row measures is the bound on ONE user's keys
        // rather than anything a previous row left behind.
        let router = router();
        let user_name = "quotaholder";
        // A group of its own per row, so no row's key is another's.
        let group = row + 8;
        let bodies: Vec<String> = (0..MAX_SSH_PUBLIC_KEYS_PER_USER)
            .map(|at| material(group, at))
            .collect();
        let ids = user_holding(&router, case, user_name, &bodies).await;

        // At the quota, the next upload is refused — the state every row starts its deletes
        // from.
        let over_the_bound = material(group, MAX_SSH_PUBLIC_KEYS_PER_USER);
        let (status, refused) = upload(&router, user_name, &over_the_bound).await;
        assert_eq!(
            (status, error_code(&refused)),
            (409, "LimitExceeded".to_string()),
            "case {case:?}: expected the upload at the quota to be refused, got {status}: \
             {refused}"
        );

        // A delete that removes nothing cannot free a slot: the id below is one this
        // account never minted, which `publicKeyIdType` (len 20..128, pattern `^[\w]+$`)
        // admits, so what refuses it is what the account holds.
        let (status, nothing) = delete(&router, user_name, "APKANOSUCHKEYEXAMPLE").await;
        assert_eq!(
            (status, error_code(&nothing)),
            (404, "NoSuchEntity".to_string()),
            "case {case:?}: expected the delete of an unheld id to be refused, got {status}: \
             {nothing}"
        );
        let (status, still_refused) = upload(&router, user_name, &over_the_bound).await;
        assert_eq!(
            (status, error_code(&still_refused)),
            (409, "LimitExceeded".to_string()),
            "case {case:?}: a refused delete must free no slot, got {status}: {still_refused}"
        );

        for id in ids.iter().take(*freed) {
            let (status, answered) = delete(&router, user_name, id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the delete of {id} to be served, got {status}: \
                 {answered}"
            );
        }
        assert_eq!(
            listed_ids(&router, case, user_name).await.len(),
            MAX_SSH_PUBLIC_KEYS_PER_USER - freed,
            "case {case:?}: expected {freed} key(s) to have left the user's collection"
        );

        // Exactly `freed` further uploads fit, and the one after them is refused again.
        for at in 0..*freed {
            let body = material(group, MAX_SSH_PUBLIC_KEYS_PER_USER + 1 + at);
            let (status, uploaded) = upload(&router, user_name, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected upload {at} of {freed} into the freed slots to be \
                 served, got {status}: {uploaded}"
            );
        }
        let body = material(group, MAX_SSH_PUBLIC_KEYS_PER_USER * 2 + 1);
        let (status, over) = upload(&router, user_name, &body).await;
        assert_eq!(
            (status, error_code(&over)),
            (409, "LimitExceeded".to_string()),
            "case {case:?}: expected the upload past the refilled quota to be refused, got \
             {status}: {over}"
        );
        assert_eq!(
            listed_ids(&router, case, user_name).await.len(),
            MAX_SSH_PUBLIC_KEYS_PER_USER,
            "case {case:?}: expected the user to stand at the quota again"
        );
    }
}
