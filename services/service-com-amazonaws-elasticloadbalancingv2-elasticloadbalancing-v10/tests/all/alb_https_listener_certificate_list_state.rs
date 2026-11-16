// SPDX-License-Identifier: CC0-1.0

//! WHICH certificate an HTTPS listener presents once its certificate list has CHANGED: the
//! same handshake, run again after an `AddListenerCertificates`, a
//! `RemoveListenerCertificates` or a `ModifyListener`, is presented the certificate that
//! list holds at THAT moment.
//!
//! The rule under test: **the certificate an HTTPS listener presents on the address its load
//! balancer published is selected, per handshake, from the listener's certificate list as
//! the control plane holds it then — the entry whose own names cover the name the
//! `ClientHello` asked for, and the listener's default certificate for a name no entry
//! covers or for no name at all.**
//!
//! Grounded in the model: `AddListenerCertificates` "Adds the specified SSL server
//! certificate to the certificate list for the specified HTTPS or TLS listener",
//! `RemoveListenerCertificates` "Removes the specified certificate from the certificate list
//! for the specified HTTPS or TLS listener" and `ModifyListenerInput$Certificates` is "[HTTPS
//! and TLS listeners] The default certificate for the listener" — so the list a handshake
//! selects from is whatever those three operations last persisted, and
//! `DescribeListenerCertificates` reports it.
//!
//! [`crate::alb_https_listener_tls`] pins the selection for ONE fixed list; these rows pin
//! that it tracks the list, which is what the operations that edit the list exist for. The
//! rows differ along the EDIT made between two handshakes (an entry added, that entry
//! removed, the default replaced) and, within each state of the list, along the name the
//! client asks for (an entry's own name, the default certificate's name, a name no entry
//! covers, and no name at all).
//!
//! Every certificate is minted by `rcgen` and brought in through Certificate Manager's own
//! `ImportCertificate`, and the leaf the listener presents is compared with it byte for
//! byte, so nothing but the imported material can pass. The clock-owed move to `active` is
//! placed in the store rather than slept for. The TLS exchange, the certificate-list read
//! and the control-plane calls are the crate's own helpers
//! ([`crate::alb_https_listener_tls::tls_exchange`],
//! [`crate::alb_https_listener_tls::add_listener_certificate`] and the two list edits beside
//! it, [`crate::certificate_resolution_and_redirect_protocol::certificate_rows`]), so no
//! suite drifts on how any of them is driven.

use crate::alb_https_listener_tls::{
    add_listener_certificate, create_listener, create_load_balancer, emulator, import, make_active,
    mint, modify_default_certificate, remove_listener_certificate, tls_exchange, Minted,
    SERVED_BODY,
};
use crate::certificate_resolution_and_redirect_protocol::certificate_rows;

/// The name the listener's default certificate covers.
const DEFAULT_NAME: &str = "default.list-state.example.com";

/// The name the certificate added to the listener's certificate list covers.
const LISTED_NAME: &str = "listed.list-state.example.net";

/// The name the certificate a `ModifyListener` makes the default covers.
const REPLACEMENT_NAME: &str = "replacement.list-state.example.org";

/// A name no certificate of these cases covers — what the listener's default certificate
/// answers.
const UNCOVERED_NAME: &str = "unknown.list-state.example.com";

/// One row: what the case is called, the name the client asks for (`None` for a client that
/// asks for no name at all) and the imported leaf the listener must then present.
type Row<'a> = (&'a str, Option<&'a str>, &'a Minted);

/// Run every row against the address `published` while the certificate list is in the state
/// `state`, asserting the leaf presented and that the listener then served its own answer.
async fn presents(state: &str, published: &str, rows: &[Row<'_>]) {
    for (row, server_name, expected) in rows {
        let case = format!("{state}, asking for {row}");
        let presented = tls_exchange(&case, published, *server_name).await;
        assert_eq!(
            presented.leaf,
            expected.der,
            "case {case:?}: expected the imported leaf ({} bytes), got {} bytes",
            expected.der.len(),
            presented.leaf.len()
        );
        assert!(
            presented.answer.starts_with("HTTP/1.1 200") && presented.answer.contains(SERVED_BODY),
            "case {case:?}: expected the listener's own answer, got {:?}",
            presented.answer
        );
    }
}

/// How a reported row spells the listener's default certificate, and how it spells a
/// certificate-list entry.
fn default_row(arn: &str) -> (String, Option<String>) {
    (arn.to_string(), Some("true".to_string()))
}

/// See [`default_row`].
fn listed_row(arn: &str) -> (String, Option<String>) {
    (arn.to_string(), Some("false".to_string()))
}

/// RULE: the certificate a handshake is presented comes from the listener's certificate list
/// as it stands at that handshake — an entry `AddListenerCertificates` put on the list is
/// selected by its own name from then on, and once `RemoveListenerCertificates` has taken it
/// off, that same name is presented the listener's default certificate again.
#[tokio::test]
async fn the_certificate_presented_follows_the_certificate_list_as_it_is_edited() {
    let (router, engine) = emulator();

    let default_leaf = mint(&[DEFAULT_NAME]);
    let listed_leaf = mint(&[LISTED_NAME]);
    let default_arn = import(&router, "the listener's default certificate", &default_leaf).await;
    let listed_arn = import(&router, "the certificate-list entry", &listed_leaf).await;

    let (lb_arn, published) = create_load_balancer(&router, "cert-list-edit-lb").await;
    let listener_arn =
        create_listener(&router, &lb_arn, "HTTPS", "443", Some(&default_arn), None).await;
    make_active(&engine, &lb_arn);

    // The list a create left behind: the default certificate alone. A name only the
    // not-yet-listed certificate covers is no entry's name, so it is served the default.
    let state = "with the default certificate alone on the list";
    let reported = certificate_rows(&router, &listener_arn).await;
    assert_eq!(
        reported,
        vec![default_row(&default_arn)],
        "case {state:?}: expected the default certificate alone, got {reported:?}"
    );
    presents(
        state,
        &published,
        &[
            (
                "the default certificate's own name",
                Some(DEFAULT_NAME),
                &default_leaf,
            ),
            (
                "the name of a certificate not on the list",
                Some(LISTED_NAME),
                &default_leaf,
            ),
            ("no name at all", None, &default_leaf),
        ],
    )
    .await;

    // The edit: that certificate joins the list, and its own name now selects it.
    add_listener_certificate(&router, &listener_arn, &listed_arn).await;
    let state = "after the second certificate joined the list";
    let reported = certificate_rows(&router, &listener_arn).await;
    assert_eq!(
        reported,
        vec![default_row(&default_arn), listed_row(&listed_arn)],
        "case {state:?}: expected the default certificate and the added entry, got {reported:?}"
    );
    presents(
        state,
        &published,
        &[
            ("the added entry's own name", Some(LISTED_NAME), &listed_leaf),
            (
                "the default certificate's own name",
                Some(DEFAULT_NAME),
                &default_leaf,
            ),
            (
                "a name no entry covers",
                Some(UNCOVERED_NAME),
                &default_leaf,
            ),
            ("no name at all", None, &default_leaf),
        ],
    )
    .await;

    // The edit back: the entry leaves the list, and its name falls back to the default.
    remove_listener_certificate(&router, &listener_arn, &listed_arn).await;
    let state = "after the second certificate left the list";
    let reported = certificate_rows(&router, &listener_arn).await;
    assert_eq!(
        reported,
        vec![default_row(&default_arn)],
        "case {state:?}: expected the default certificate alone again, got {reported:?}"
    );
    presents(
        state,
        &published,
        &[
            (
                "the removed entry's own name",
                Some(LISTED_NAME),
                &default_leaf,
            ),
            (
                "the default certificate's own name",
                Some(DEFAULT_NAME),
                &default_leaf,
            ),
        ],
    )
    .await;
}

/// RULE (the default half): the certificate presented for a name no entry covers is the
/// listener's default certificate as `ModifyListener` last set it — a replaced default is
/// presented for nothing, and the certificate list the modify did not name keeps selecting
/// its own names.
#[tokio::test]
async fn the_default_certificate_presented_is_the_one_a_modify_last_set() {
    let (router, engine) = emulator();

    let first_default = mint(&[DEFAULT_NAME]);
    let listed_leaf = mint(&[LISTED_NAME]);
    let replacement = mint(&[REPLACEMENT_NAME]);
    let first_arn = import(&router, "the first default certificate", &first_default).await;
    let listed_arn = import(&router, "the certificate-list entry", &listed_leaf).await;
    let replacement_arn = import(&router, "the replacing default certificate", &replacement).await;

    let (lb_arn, published) = create_load_balancer(&router, "cert-list-modify-lb").await;
    let listener_arn =
        create_listener(&router, &lb_arn, "HTTPS", "443", Some(&first_arn), None).await;
    add_listener_certificate(&router, &listener_arn, &listed_arn).await;
    make_active(&engine, &lb_arn);

    let state = "with the first default certificate";
    let reported = certificate_rows(&router, &listener_arn).await;
    assert_eq!(
        reported,
        vec![default_row(&first_arn), listed_row(&listed_arn)],
        "case {state:?}: expected the first default beside the entry, got {reported:?}"
    );
    presents(
        state,
        &published,
        &[
            (
                "a name no entry covers",
                Some(UNCOVERED_NAME),
                &first_default,
            ),
            ("the list entry's own name", Some(LISTED_NAME), &listed_leaf),
        ],
    )
    .await;

    modify_default_certificate(&router, &listener_arn, &replacement_arn).await;
    let state = "after a modify replaced the default certificate";
    let reported = certificate_rows(&router, &listener_arn).await;
    assert_eq!(
        reported,
        vec![default_row(&replacement_arn), listed_row(&listed_arn)],
        "case {state:?}: expected the replacing default beside the untouched entry, got \
         {reported:?}"
    );
    presents(
        state,
        &published,
        &[
            ("a name no entry covers", Some(UNCOVERED_NAME), &replacement),
            (
                "the replacing certificate's own name",
                Some(REPLACEMENT_NAME),
                &replacement,
            ),
            (
                "the replaced certificate's own name",
                Some(DEFAULT_NAME),
                &replacement,
            ),
            ("the list entry's own name", Some(LISTED_NAME), &listed_leaf),
            ("no name at all", None, &replacement),
        ],
    )
    .await;
}
