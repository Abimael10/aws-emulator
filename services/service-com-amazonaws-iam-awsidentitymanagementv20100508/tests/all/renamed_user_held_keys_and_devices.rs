// SPDX-License-Identifier: CC0-1.0

//! The records an IAM user holds across an `UpdateUser` that moves its name, end to end over
//! the awsQuery wire — its SSH public keys and the MFA devices in use by it.
//!
//! RULE: a record the account holds ABOUT a user belongs to the USER and is reached through
//! it, and an update to a user's name leaves the same user — "the user's unique identifier
//! remains the same" (`UpdateUser`) — so everything the account holds for it moves with it:
//! every reader reports those records under the name the user now carries, the writes that
//! address one (`DeleteSSHPublicKey`) reach it there, and the name the user released holds
//! none of them, so a later user created under that name inherits nothing.
//!
//! `tests/all/user_renames.rs` owns this rule for the records it already covers (the user's
//! policies, access keys, service-specific credentials, console password, memberships and
//! tags); what is pinned here are the two collections it does not reach: the SSH public keys
//! `UploadSSHPublicKey` associates with the user, and the MFA devices of either kind the
//! account keeps for it.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: which halves
//! of the update the request carries (the name, the name and the path, the path alone, a
//! re-spelling of the name the user already carries), how many SSH public keys the user
//! holds and the encoding each one's material arrived in. The key material is built by the
//! crates that implement those encodings, through the fixtures `tests/all/ssh_public_keys.rs`
//! owns; the device fixtures are the ones `tests/all/mfa_device_listing.rs` and
//! `tests/all/mfa_device_reads.rs` own, so a device is enabled and enrolled exactly one way.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the HTTP
//! behaviour of records that were PERSISTED.

use smithy_server_runtime::MultiServiceRouter;

use crate::mfa_device_listing::given_enabled_device;
use crate::mfa_device_reads::record_fido_security_key;
use crate::ssh_public_key_reads::get;
use crate::ssh_public_keys::{PUBLISHED_MINIMUM_BITS, modulus, openssh_line, spki_pem, upload};
use crate::user_lifecycle::{
    account_id, call, element, elements, error_code, router_with_engine,
};

/// The name the fixture user is created under, and the path it is created at.
const CREATED_NAME: &str = "key-and-device-holder";
const CREATED_PATH: &str = "/eng/";

/// One row of the update table: what the update carries, and how many SSH public keys the
/// user holds when it arrives.
struct Row {
    case: &'static str,
    new_user_name: Option<&'static str>,
    new_path: Option<&'static str>,
    held_keys: usize,
}

/// The rows: a rename alone, a rename together with a re-path, a re-path alone (which leaves
/// the name the records are reached under as it is) and a re-spelling of the user's own name
/// (IAM names "are not distinguished by case", so this names the same user).
const ROWS: &[Row] = &[
    Row {
        case: "a rename alone",
        new_user_name: Some("Renamed.Holder"),
        new_path: None,
        held_keys: 2,
    },
    Row {
        case: "a rename and a re-path at once",
        new_user_name: Some("renamed+holder=2,x.y@example"),
        new_path: Some("/eng/platform/"),
        held_keys: 3,
    },
    Row {
        case: "a re-path alone",
        new_user_name: None,
        new_path: Some("/"),
        held_keys: 1,
    },
    Row {
        case: "a re-spelling of the user's own name",
        new_user_name: Some("KEY-AND-DEVICE-HOLDER"),
        new_path: None,
        held_keys: 2,
    },
];

/// The material for key `slot` of row `row`: a public key of a length the upload takes,
/// written in one of the two encodings it takes, with one key per `(row, slot)` pair so no
/// upload of this suite repeats another's key.
fn material(row: usize, slot: usize) -> String {
    let seed = (row * 16 + slot + 1) as u8;
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

/// One `UpdateUser` call carrying only the halves the row names.
async fn update(
    router: &MultiServiceRouter,
    user_name: &str,
    new_user_name: Option<&str>,
    new_path: Option<&str>,
) -> (u16, String) {
    let mut params = vec![("UserName", user_name)];
    if let Some(new_user_name) = new_user_name {
        params.push(("NewUserName", new_user_name));
    }
    if let Some(new_path) = new_path {
        params.push(("NewPath", new_path));
    }
    call(router, "UpdateUser", &params).await
}

/// The ids `ListSSHPublicKeys` reports for `user_name`, sorted, beside the user name each is
/// reported under, from a listing that must be served.
async fn listed_keys(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
) -> (Vec<String>, Vec<String>) {
    let (status, listed) = call(router, "ListSSHPublicKeys", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing of {user_name}'s keys must be served, got {status}: {listed}"
    );
    let mut ids = elements(&listed, "SSHPublicKeyId");
    ids.sort();
    (ids, elements(&listed, "UserName"))
}

/// The serials `ListMFADevices` reports for `user_name`, sorted, beside the user name each is
/// reported under, from a listing that must be served.
async fn listed_devices(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
) -> (Vec<String>, Vec<String>) {
    let (status, listed) = call(router, "ListMFADevices", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing of {user_name}'s devices must be served, got {status}: \
         {listed}"
    );
    let mut serials = elements(&listed, "SerialNumber");
    serials.sort();
    (serials, elements(&listed, "UserName"))
}

/// Assert the call `action` against `user_name` is the modelled absence refusal, so nothing
/// of the user's is reachable under a name the account holds no user under.
async fn assert_not_found(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        (status, error_code(&body)),
        (404, "NoSuchEntity".to_string()),
        "case {case:?}: expected {action} against the released name to be refused, got \
         {status}: {body}"
    );
}

/// RULE: the SSH public keys and the MFA devices the account holds for a user move with it
/// when an update changes its name, so each is reported, read, written and refused under the
/// name the user now carries — and the released name holds none of them.
#[tokio::test]
async fn the_keys_and_devices_a_user_holds_follow_its_rename() {
    for (
        row,
        Row {
            case,
            new_user_name,
            new_path,
            held_keys,
        },
    ) in ROWS.iter().enumerate()
    {
        // One router per row: what the row measures is where ONE user's records end up.
        let (router, engine) = router_with_engine();
        let (status, created) = call(
            &router,
            "CreateUser",
            &[("UserName", CREATED_NAME), ("Path", CREATED_PATH)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateUser must succeed, got {status}: {created}"
        );
        let user_id = element(&created, "UserId");

        // The SSH public keys the user holds, each carried in by a different encoding.
        let mut key_ids: Vec<String> = Vec::with_capacity(*held_keys);
        for slot in 0..*held_keys {
            let body = material(row, slot);
            let (status, uploaded) = upload(&router, CREATED_NAME, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fixture upload must be served, got {status}: {uploaded}"
            );
            key_ids.push(element(&uploaded, "SSHPublicKeyId"));
        }
        key_ids.sort();

        // One MFA device of each kind the account keeps: a virtual device `EnableMFADevice`
        // enabled for the user, and a FIDO security key, which no operation of this service
        // enrolls, so the fact is written into the store the handlers read.
        let device_serial = given_enabled_device(&router, case, CREATED_NAME, "holder-phone").await;
        let key_serial = format!("arn:aws:iam::{}:u2f/user/{CREATED_NAME}/key-1", account_id());
        record_fido_security_key(&engine, &key_serial, CREATED_NAME, &[("FIDO", "L1")]);
        let mut device_serials = vec![device_serial.clone(), key_serial.clone()];
        device_serials.sort();

        let (status, updated) = update(&router, CREATED_NAME, *new_user_name, *new_path).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateUser to be served, got {status}: {updated}"
        );
        let now_named = new_user_name.unwrap_or(CREATED_NAME);

        // The same user, so what it held is still its own.
        let (status, read) = call(&router, "GetUser", &[("UserName", now_named)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the updated user to read back, got {status}: {read}"
        );
        assert_eq!(
            element(&read, "UserId"),
            user_id,
            "case {case:?}: expected the updated user to keep its minted id, got {read}"
        );

        // Its SSH public keys are reported under the name it now carries, and each of them
        // reads back there.
        let (reported_ids, reported_for) = listed_keys(&router, case, now_named).await;
        assert_eq!(
            reported_ids, key_ids,
            "case {case:?}: expected the user to keep its {held_keys} SSH public key(s)"
        );
        assert_eq!(
            reported_for,
            vec![now_named.to_string(); *held_keys],
            "case {case:?}: expected every key to be reported for {now_named:?}, got \
             {reported_for:?}"
        );
        for id in &key_ids {
            let (status, got) = get(&router, now_named, id, "SSH").await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {id} to read back for {now_named:?}, got {status}: {got}"
            );
            assert_eq!(
                element(&got, "UserName"),
                now_named,
                "case {case:?}: expected the read to report {now_named:?}, got {got}"
            );
        }

        // And the MFA devices in use by it, of both kinds.
        let (reported_serials, reported_for) = listed_devices(&router, case, now_named).await;
        assert_eq!(
            reported_serials, device_serials,
            "case {case:?}: expected the user to keep both its MFA devices"
        );
        assert_eq!(
            reported_for,
            vec![now_named.to_string(); device_serials.len()],
            "case {case:?}: expected every device to be reported for {now_named:?}, got \
             {reported_for:?}"
        );
        let (status, device) = call(
            &router,
            "GetMFADevice",
            &[("SerialNumber", &key_serial), ("UserName", now_named)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the security key to read back for {now_named:?}, got \
             {status}: {device}"
        );

        // A write that addresses one of those keys reaches it under the name the user now
        // carries, and takes it away.
        let deleted = key_ids
            .last()
            .expect("a row holds at least one key")
            .clone();
        let (status, answered) = call(
            &router,
            "DeleteSSHPublicKey",
            &[("UserName", now_named), ("SSHPublicKeyId", &deleted)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete under {now_named:?} to be served, got {status}: \
             {answered}"
        );
        let (reported_ids, _) = listed_keys(&router, case, now_named).await;
        assert_eq!(
            reported_ids,
            key_ids[..held_keys - 1].to_vec(),
            "case {case:?}: expected the deleted key to have left the user's collection"
        );

        if now_named.eq_ignore_ascii_case(CREATED_NAME) {
            // The update kept the user's name, so there is no released name to ask about.
            continue;
        }

        // Nothing is reachable under the released name: it names no user, so every reader
        // and the delete refuse it with the one absence error each models.
        let remaining = key_ids[0].clone();
        assert_not_found(
            &router,
            case,
            "ListSSHPublicKeys",
            &[("UserName", CREATED_NAME)],
        )
        .await;
        assert_not_found(&router, case, "ListMFADevices", &[("UserName", CREATED_NAME)]).await;
        assert_not_found(
            &router,
            case,
            "DeleteSSHPublicKey",
            &[
                ("UserName", CREATED_NAME),
                ("SSHPublicKeyId", remaining.as_str()),
            ],
        )
        .await;
        assert_not_found(
            &router,
            case,
            "GetMFADevice",
            &[
                ("SerialNumber", key_serial.as_str()),
                ("UserName", CREATED_NAME),
            ],
        )
        .await;

        // And a user later created under that name inherits none of it.
        let (status, recreated) = call(
            &router,
            "CreateUser",
            &[("UserName", CREATED_NAME), ("Path", CREATED_PATH)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the re-created user must succeed, got {status}: {recreated}"
        );
        let (inherited_keys, _) = listed_keys(&router, case, CREATED_NAME).await;
        assert!(
            inherited_keys.is_empty(),
            "case {case:?}: expected the re-created user to hold no SSH public key, got \
             {inherited_keys:?}"
        );
        let (inherited_devices, _) = listed_devices(&router, case, CREATED_NAME).await;
        assert!(
            inherited_devices.is_empty(),
            "case {case:?}: expected the re-created user to hold no MFA device, got \
             {inherited_devices:?}"
        );

        // The key the delete above left the renamed user is still the renamed user's own.
        let (status, got) = get(&router, now_named, &remaining, "SSH").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {remaining} to stay the renamed user's key, got {status}: \
             {got}"
        );
    }
}
