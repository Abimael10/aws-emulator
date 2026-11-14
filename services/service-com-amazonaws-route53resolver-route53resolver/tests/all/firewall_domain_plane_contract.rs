// SPDX-License-Identifier: CC0-1.0

//! One end-to-end test for WHICH list the DNS Firewall in-flight conflict
//! names.
//!
//! The rule under test: the `ConflictException` a domain list answers while its
//! own change is still running names the list the REQUEST addressed — the id is
//! interpolated from that request and not carried as one list's id for every
//! caller — and the change already running still lands, so the refusal writes
//! nothing. The sentence itself, and the step it states for each in-flight
//! `FirewallDomainListStatus`, are pinned by
//! [`super::firewall_domain_import_refusals`]; what this case adds is the
//! second list, busy at the same time, which is what makes an interpolated id
//! and a memorised one tell apart.
//!
//! The case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! shared [`super::harness`] and the import harness of
//! [`super::firewall_domain_imports`], whose helpers it reuses rather than
//! growing copies of them.

use serde_json::json;

use super::firewall_domain_import_refusals::in_progress_message;
use super::firewall_domain_imports::{
    emulator_with_object_storage, hold_object, list_domains, seed_list, settle, BUCKET,
};
use super::harness::{call, error_type, ok, string_member};

/// Two lists importing at the same time are each refused in their own name, and
/// each one's running import still lands with the file IT named.
///
/// The rows are the two lists: each is refused while the OTHER is in the same
/// step, so the id the sentence quotes can only have come from the request that
/// drew it.
#[tokio::test]
async fn two_lists_importing_at_once_are_each_refused_in_their_own_name() {
    let (router, engine) = emulator_with_object_storage();
    hold_object(
        &engine,
        "concurrent/first.txt",
        b"first.example.com\n".to_vec(),
    );
    hold_object(
        &engine,
        "concurrent/second.txt",
        b"second.example.org\n".to_vec(),
    );
    hold_object(
        &engine,
        "concurrent/other.txt",
        b"other.example.net\n".to_vec(),
    );

    // (the case, the file this list's own import named, the domain that file
    // lists, in the canonical spelling a list holds a name in)
    let rows = [
        (
            "the first list",
            "concurrent/first.txt",
            "first.example.com.",
        ),
        (
            "the second list",
            "concurrent/second.txt",
            "second.example.org.",
        ),
    ];

    let mut busy = Vec::with_capacity(rows.len());
    for (index, (case, key, landed)) in rows.into_iter().enumerate() {
        let id = seed_list(&router, &format!("concurrent-{index}")).await;
        let accepted = ok(
            &router,
            "ImportFirewallDomains",
            json!({
                "FirewallDomainListId": id,
                "Operation": "REPLACE",
                "DomainFileUrl": format!("s3://{BUCKET}/{key}"),
            }),
        )
        .await;
        assert_eq!(
            string_member(&accepted, "Status"),
            "IMPORTING",
            "case {case:?}: an accepted import is under way when it answers, got {accepted}"
        );
        busy.push((case, id, landed));
    }

    // Both lists are importing now, so each refusal below is drawn while the
    // other list stands in the very same step.
    for (case, id, _) in &busy {
        let (status, refusal) = call(
            &router,
            "ImportFirewallDomains",
            json!({
                "FirewallDomainListId": id,
                "Operation": "REPLACE",
                "DomainFileUrl": format!("s3://{BUCKET}/concurrent/other.txt"),
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the second import must be refused, got {status}: {refusal}"
        );
        assert!(
            error_type(&refusal).contains("ConflictException"),
            "case {case:?}: the refusal must name ConflictException, got {refusal}"
        );
        let message = string_member(&refusal, "Message");
        assert_eq!(
            message,
            in_progress_message(id, "IMPORTING", "import"),
            "case {case:?}: the refusal must name the list the request addressed, got {message:?}"
        );
        for (other_case, other, _) in &busy {
            assert!(
                other == id || !message.contains(other.as_str()),
                "case {case:?}: the refusal must name no other list, {other_case:?} appears in \
                 {message:?}"
            );
        }
    }

    // Each list's OWN import is the one that ran: the refusal wrote nothing, so
    // the file each list named is the file it holds.
    for (case, id, landed) in &busy {
        settle(&router, id, case).await;
        let domains = list_domains(&router, id).await;
        assert_eq!(
            domains,
            vec![(*landed).to_string()],
            "case {case:?}: the refused import must write nothing, got {domains:?}"
        );
    }
}
