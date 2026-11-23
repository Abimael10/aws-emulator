// SPDX-License-Identifier: CC0-1.0

//! WHICH `Id` values the Anycast static IP list surface can READ, end to end over HTTP.
//!
//! RULE: `Id` is a plain `string` path label on every operation addressed by one
//! (`smithy_query(query="operation",operation="GetAnycastIpList,UpdateAnycastIpList")` — no
//! `@pattern`, no `@length`), so the only values such an operation may refuse unread are the
//! ones written in NO form CloudFront writes an identifier in. A value written under a
//! namespace label (`ipl-…`, whatever label, body width, body alphabet or separator it
//! carries) or in the alphabet CloudFront writes an ENTITY identifier in (ASCII digits and
//! upper-case letters) could have been issued, so it is resolved against the STORE and one no
//! list answers to is the list reported ABSENT — `EntityNotFound` (404) with its fixed
//! sentence. A value written in neither form — around no separator in lower case, as a UUID,
//! as a word, as a name, or in that alphabet with one character outside it — was never an
//! identifier at all and is the argument the operation cannot read, `InvalidArgument` (400)
//! with the sentence naming the member it arrived in.
//!
//! Because the TARGET is resolved before anything the body submits is read, every row drives
//! the update twice — once with a body the update can apply and once with a body carrying an
//! unparseable CIDR range — and both answer the row's own refusal: what the request says about
//! the list cannot change what the service says about an identifier that addresses nothing.
//!
//! The rows span what the rule generalises over: the namespace label and separator, the body's
//! width and alphabet, the two forms an identifier takes at all, the alphabet broken in each
//! way a value breaks it, and the two operations addressed by such an identifier. The list
//! that really EXISTS is driven through both operations as well, so the reading is shown not to
//! refuse the calls it must serve.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the create and read
//! [`crate::anycast_ip_list_lifecycle`]'s and the update writer
//! [`crate::anycast_ip_list_update`]'s, reused rather than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    id_format_invalid_message, ANYCAST_IP_LIST_NOT_FOUND_MESSAGE, IP_ADDRESS_TYPE_IPV4,
    IP_ADDRESS_TYPE_IPV6,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::anycast_ip_list_lifecycle::{create_anycast_ip_list, get_anycast_ip_list, Create};
use crate::anycast_ip_list_update::update_anycast_ip_list;
use crate::distribution_lifecycle::{emulator, Answer};

/// The name the live list is created under: a value carrying no separator, so the row that
/// addresses the list by its NAME is a value of no identifier form rather than of one.
const LIST_NAME: &str = "idformslist";

/// An IPAM CIDR configuration the update cannot apply: `300.1.1.0/24` is no CIDR range at all
/// (no octet of an IPv4 address reaches 300), so a row submitting it proves the identifier is
/// resolved before the body is read.
const UNPARSEABLE_RANGE: [(&str, &str); 1] = [(
    "300.1.1.0/24",
    "arn:aws:ec2::000000000000:ipam-pool/ipam-pool-unparseable",
)];

/// One identifier under test, and which half of the rule it falls in.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    id: String,
    /// Whether the value is written in a form CloudFront writes an identifier in, so the
    /// operation resolves it against the store instead of refusing it unread.
    readable: bool,
}

/// Every identifier the rule is asserted over.
///
/// The readable rows vary the parts nothing in the model constrains — the namespace label, the
/// separator, and the body's width and alphabet — and then the OTHER form an identifier takes,
/// the entity-identifier alphabet, at several widths and compositions, including another
/// resource kind's own minted identifier (that alphabet is shared, and which identifier of it
/// was issued for what is a property of the store alone). The unreadable rows break that
/// alphabet in each way a value breaks it.
fn rows() -> Vec<Row> {
    let readable = [
        (
            "the label, separator and body width this service mints under",
            "ipl-00000000000000000".to_string(),
        ),
        (
            "the minted label at another body width and alphabet",
            "ipl-notaminted-id".to_string(),
        ),
        (
            "another namespace label, under the other separator",
            "aip_1A2B3C4D5E6F7G8H9J0K1L".to_string(),
        ),
        (
            "a one-letter label and a one-character body",
            "a_0".to_string(),
        ),
        (
            "the entity-identifier alphabet, letters over digits",
            "IAL1A2B3C4D5E6".to_string(),
        ),
        (
            "the entity-identifier alphabet, letters alone",
            "ZZZZZZZZZZZZZ".to_string(),
        ),
        (
            "the entity-identifier alphabet, digits alone",
            "0123456789".to_string(),
        ),
        (
            "the entity-identifier alphabet, far wider than any id this service mints",
            "Q".repeat(64),
        ),
        (
            "another resource kind's minted identifier, of that shared alphabet",
            "EDFDVBD6EXAMPLE".to_string(),
        ),
    ];
    let unreadable = [
        (
            "the minted label with its separator dropped",
            "ipl0123456789abcdef0".to_string(),
        ),
        (
            "the entity-identifier alphabet with one lower-case character",
            "IAL1a2B3C4D5E6".to_string(),
        ),
        (
            "the entity-identifier alphabet with a character no identifier carries",
            "IAL1A2B3C4D5E6.".to_string(),
        ),
        (
            "a UUID, the shape of a kind whose ids carry no namespace",
            "f9a6a0b4-8d39-4a1e-9f67-1b0f7f5d2c3e".to_string(),
        ),
        ("a bare word", "nope".to_string()),
        ("the list's own name", LIST_NAME.to_string()),
        ("a lone separator", "-".to_string()),
    ];
    readable
        .into_iter()
        .map(|(case, id)| Row {
            case,
            id,
            readable: true,
        })
        .chain(unreadable.into_iter().map(|(case, id)| Row {
            case,
            id,
            readable: false,
        }))
        .collect()
}

/// What `row` must be answered with: `(status, code, message)`.
fn expected(row: &Row) -> (u16, &'static str, String) {
    if row.readable {
        (
            404,
            "EntityNotFound",
            ANYCAST_IP_LIST_NOT_FOUND_MESSAGE.to_string(),
        )
    } else {
        (400, "InvalidArgument", id_format_invalid_message("Id"))
    }
}

/// Assert one answer against `row`.
fn assert_row(answer: &Answer, row: &Row, leg: &str) {
    let (status, code, message) = expected(row);
    assert_eq!(
        answer.status, status,
        "case {:?} ({leg}, {:?}): expected HTTP {status}, got {} ({})",
        row.case, row.id, answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {:?} ({leg}, {:?}): expected {code}, got {:?} ({})",
        row.case,
        row.id,
        answer.error_code(),
        answer.raw
    );
    let actual = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        actual, message,
        "case {:?} ({leg}, {:?}): expected {message:?}, got {actual:?} in {}",
        row.case, row.id, answer.raw
    );
}

/// Create the one list every row is asserted beside and report `(Id, ETag)`.
async fn live_list(router: &MultiServiceRouter) -> (String, String) {
    let create = Create {
        case: "the list the identifier rule is asserted around",
        name: LIST_NAME.to_string(),
        ip_count: 3,
        ip_address_type: Some(IP_ADDRESS_TYPE_IPV4),
        ipam_cidr_configs: Vec::new(),
        tags: Vec::new(),
    };
    let answer = create_anycast_ip_list(router, &create.xml()).await;
    assert_eq!(
        answer.status, 202,
        "case {}: expected HTTP 202 from the create, got {} ({})",
        create.case, answer.status, answer.raw
    );
    (
        answer.root().text_at("Id").unwrap_or_default().to_string(),
        answer.header("etag").to_string(),
    )
}

#[tokio::test]
async fn an_identifier_of_either_form_is_resolved_and_one_of_neither_is_unreadable() {
    let router = emulator();
    let (id, etag) = live_list(&router).await;

    for row in rows() {
        // The READ of the resource and the UPDATE of it read the identifier the same way, so
        // the two sides of the surface agree about what an identifier is.
        let read = get_anycast_ip_list(&router, &row.id).await;
        assert_row(&read, &row, "GetAnycastIpList");

        // The update under a body it could apply, and under one carrying a range it could
        // not: the identifier is resolved first, so both answer the same refusal.
        let applied = update_anycast_ip_list(
            &router,
            &row.id,
            Some(&etag),
            Some(IP_ADDRESS_TYPE_IPV6),
            None,
        )
        .await;
        assert_row(&applied, &row, "UpdateAnycastIpList with a body it can apply");

        let malformed = update_anycast_ip_list(
            &router,
            &row.id,
            Some(&etag),
            None,
            Some(&UNPARSEABLE_RANGE),
        )
        .await;
        assert_row(
            &malformed,
            &row,
            "UpdateAnycastIpList with an unparseable CIDR range",
        );
    }

    // Every refusal above left the live list exactly as it was, at the version its create
    // handed back.
    let read = get_anycast_ip_list(&router, &id).await;
    assert_eq!(
        read.status, 200,
        "case the live list: expected HTTP 200, got {} ({})",
        read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        etag,
        "case the live list: a refused call must spend no version token, got {}",
        read.raw
    );
    assert_eq!(
        read.root().text_at("IpAddressType"),
        Some(IP_ADDRESS_TYPE_IPV4),
        "case the live list: it must still run under the type it was created with, got {}",
        read.raw
    );
}

#[tokio::test]
async fn the_identifier_reading_still_serves_the_list_that_exists() {
    let router = emulator();
    let (id, etag) = live_list(&router).await;

    let read = get_anycast_ip_list(&router, &id).await;
    assert_eq!(
        read.status, 200,
        "case the minted id on the read: expected HTTP 200, got {} ({})",
        read.status, read.raw
    );

    let updated =
        update_anycast_ip_list(&router, &id, Some(&etag), Some(IP_ADDRESS_TYPE_IPV6), None).await;
    assert_eq!(
        updated.status, 200,
        "case the minted id on the update: expected HTTP 200, got {} ({})",
        updated.status, updated.raw
    );
    assert_eq!(
        updated.root().text_at("IpAddressType"),
        Some(IP_ADDRESS_TYPE_IPV6),
        "case the minted id on the update: expected the submitted type, got {}",
        updated.raw
    );
}
