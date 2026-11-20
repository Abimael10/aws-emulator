// SPDX-License-Identifier: CC0-1.0

//! Which ACCOUNT a `PublishType` `Arn` names, over the awsQuery wire.
//!
//! The rule every case here holds the service to: **a publication publishes an extension of
//! the CALLER'S account, so an `Arn` whose account segment is another account's addresses
//! nothing this account's registry could hold and is refused with the registry's modelled
//! `CFNRegistryException` (400) carrying `Type not found in your account.` — fixed text with
//! no variable part. That is read from the ARN alone, like the version-segment rule beside it:
//! before the caller's publisher registration is consulted and before the registry is, so
//! neither what this account registered under the same name nor whether the caller is a
//! publisher changes the answer.**
//!
//! The table spans what the rule generalises over: the ARN of an extension this account HAS
//! registered and of one no registry holds, each spelled for the caller's own account and for
//! another account, with and without a version segment pinned past the extension. Every row is
//! driven twice — once as an account that is not a publisher, once as one that is — so what is
//! asserted is the ORDER the conditions are read in: a row the ARN alone settles answers the
//! same either way, while the one row the ARN does not settle is answered the publisher gate
//! until the registration is there. The recorded trace this suite was written for is the first
//! row, from an account that is not a publisher.
//!
//! `publish_type_publisher_preflight.rs` is the suite for the gate's own position and
//! `publish_type_version_in_arn.rs` for the version segment; this one is about the account
//! segment.

use service_com_amazonaws_cloudformation_cloudformation::state::registry::{
    registry_type, type_arn, type_version_arn,
};

use crate::extension_registry::router_with_registry;
use crate::publish_type::{
    assert_registry_refusal, not_a_verified_publisher, publish_type, register_publisher,
    registry_snapshot, NOT_FOUND_IN_YOUR_ACCOUNT, ONLY_DEFAULT_VERSION,
};
use crate::stack_lifecycle::REGION;

/// An account that is not the caller's, as the modelled `PrivateTypeArn` pattern requires one:
/// twelve digits.
const ANOTHER_ACCOUNT: &str = "123456789012";

/// The private record this crate's registry fixture holds for this account and Region, and a
/// name no registry holds at all.
const REGISTERED: &str = "My::Sample::Resource";
const UNREGISTERED: &str = "No::Such::Resource";

/// An extension ARN spelled for an account that is not the caller's: the form [`type_arn`]
/// mints, with another account in the account segment.
fn arn_of_another_account(type_name: &str) -> String {
    let mine = type_arn(REGION, registry_type::RESOURCE, type_name);
    let (_, resource) = mine
        .split_once(":type/")
        .expect("an extension ARN carries a type/ resource part");
    format!("arn:aws:cloudformation:{REGION}:{ANOTHER_ACCOUNT}:type/{resource}")
}

/// One row: the `Arn` the request carries, the refusal the ARN alone reaches where it has one
/// (the same answer whether or not the caller is a publisher), and the refusal the identifier
/// is owed once the caller IS a publisher.
struct Row {
    case: &'static str,
    arn: String,
    read_from_the_arn: Option<&'static str>,
    served: &'static str,
}

fn rows() -> Vec<Row> {
    let registered = type_arn(REGION, registry_type::RESOURCE, REGISTERED);
    let unregistered = type_arn(REGION, registry_type::RESOURCE, UNREGISTERED);
    vec![
        Row {
            // The recorded class: an ARN of another account pinning a version, naming a type
            // registered nowhere.
            case: "another account's ARN pinning a version, of an unregistered extension",
            arn: type_version_arn(&arn_of_another_account(UNREGISTERED), "00000001"),
            read_from_the_arn: Some(NOT_FOUND_IN_YOUR_ACCOUNT),
            served: NOT_FOUND_IN_YOUR_ACCOUNT,
        },
        Row {
            case: "another account's ARN of an unregistered extension",
            arn: arn_of_another_account(UNREGISTERED),
            read_from_the_arn: Some(NOT_FOUND_IN_YOUR_ACCOUNT),
            served: NOT_FOUND_IN_YOUR_ACCOUNT,
        },
        Row {
            // The name IS registered here — the ARN's account segment is what refuses it, so
            // the version segment is never read either.
            case: "another account's ARN pinning a version, of an extension registered here",
            arn: type_version_arn(&arn_of_another_account(REGISTERED), "00000001"),
            read_from_the_arn: Some(NOT_FOUND_IN_YOUR_ACCOUNT),
            served: NOT_FOUND_IN_YOUR_ACCOUNT,
        },
        Row {
            case: "another account's ARN of an extension registered here",
            arn: arn_of_another_account(REGISTERED),
            read_from_the_arn: Some(NOT_FOUND_IN_YOUR_ACCOUNT),
            served: NOT_FOUND_IN_YOUR_ACCOUNT,
        },
        Row {
            // The other side of the differential: an ARN of the CALLER'S account is parsed
            // further, so a version pinned past the extension is what refuses it.
            case: "this account's ARN pinning a version, of an unregistered extension",
            arn: type_version_arn(&unregistered, "00000001"),
            read_from_the_arn: Some(ONLY_DEFAULT_VERSION),
            served: ONLY_DEFAULT_VERSION,
        },
        Row {
            case: "this account's ARN pinning a version, of an extension registered here",
            arn: type_version_arn(&registered, "00000001"),
            read_from_the_arn: Some(ONLY_DEFAULT_VERSION),
            served: ONLY_DEFAULT_VERSION,
        },
        Row {
            // ...and an ARN of this account naming no registered extension is settled by
            // neither of those rules: it is resolved against this account's registry, which is
            // behind the publisher gate.
            case: "this account's ARN of an unregistered extension",
            arn: unregistered,
            read_from_the_arn: None,
            served: NOT_FOUND_IN_YOUR_ACCOUNT,
        },
    ]
}

/// The account segment decides, ahead of the caller's registration and of the registry: each
/// row the ARN alone settles is refused with the same message whether or not the calling
/// account is a publisher, the one row it does not settle is answered the gate until the
/// registration is there, and no call publishes anything.
#[tokio::test]
async fn an_arn_of_another_account_is_not_found_in_this_one() {
    for row in rows() {
        for registered_publisher in [false, true] {
            // One store per pass: a publication is a write, so no pass may read what another
            // left behind.
            let (router, engine) = router_with_registry();
            if registered_publisher {
                register_publisher(&router).await;
            }
            let before = registry_snapshot(&engine);
            let case = format!("{} (publisher registered: {registered_publisher})", row.case);
            let expected = if registered_publisher {
                row.served.to_string()
            } else {
                row.read_from_the_arn
                    .map_or_else(not_a_verified_publisher, ToString::to_string)
            };

            let (status_code, body) = publish_type(&router, &[("Arn", row.arn.as_str())]).await;
            assert_registry_refusal(&case, status_code, &body, &expected);
            assert_eq!(
                registry_snapshot(&engine),
                before,
                "case {case:?}: expected the refused call to publish nothing"
            );
        }
    }
}
