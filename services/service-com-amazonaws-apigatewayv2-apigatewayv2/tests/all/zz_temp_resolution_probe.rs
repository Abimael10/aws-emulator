// SPDX-License-Identifier: CC0-1.0

//! Does the name an HTTP api publishes RESOLVE back to this process, and is the api served
//! under it? One probe of the published address, over a real connection to the socket that
//! answers there.
//!
//! The reachability half of the rule `published_endpoint_documented_host` states the form of:
//!
//! > The address an api publishes is one a client DIALS, so the host this emulator writes into
//! > it leads back to THIS process — the zone RFC 6761 reserves for the loopback interface the
//! > core binds its listeners on, never a zone somebody else's resolver answers for — and the
//! > api's data plane ANSWERS for that host: a request naming it, with the stage name and route
//! > path appended as `Api$ApiEndpoint` documents ("the stage name is typically appended to this
//! > URI to form a complete path to a deployed API stage"), is answered by the api its leading
//! > label names.
//!
//! The rows are `published_endpoint_documented_host`'s — the Region the api was created in and
//! the operation that minted it — plus, per row, the two forms a client builds the invoke URL
//! in (the published value on its own, which addresses the stage that serves the api's root, and
//! that value with a stage name appended) and the twin api published beside it, which shows the
//! host and not the port is what says which api answers.
//!
//! The connection is made to the loopback interface the published name resolves to, on the port
//! the published value names, and the request NAMES the published authority: the address is the
//! api's and the interface is this emulator's.

use std::net::ToSocketAddrs;

use crate::published_endpoint_documented_host::{
    emulator_served_on_a_port, host_of, minted, published_parts, rows, NAMED_STAGE, ROUTE_PATH,
    SERVED, UPSTREAM,
};
use crate::published_invoke_socket_front_door::{dialable_port, request};

/// RULE: the published host leads back to this process and the api is served under it — the
/// route answers a request that names the published authority with the stage path appended,
/// and the twin api published on the same socket answers under its own host alone.
#[tokio::test(flavor = "multi_thread")]
async fn the_published_host_leads_back_here_and_the_api_is_served_under_it() {
    for row in rows() {
        let case = row.case;
        let emulator = emulator_served_on_a_port();
        let api = minted(&emulator, &row).await;
        let twin = minted(&emulator, &row).await;
        let (authority, _path) = published_parts(&api.api_endpoint);
        let (twin_authority, _twin_path) = published_parts(&twin.api_endpoint);

        // The name a client is handed must lead back to this process, so nothing it resolves to
        // is off this machine. WHETHER a resolver answers for a name under the loopback zone is
        // the resolver's own (RFC 6761 reserves the zone and every name under it for this
        // interface); what is asserted here is that no address it answers with leaves it.
        let resolved = format!("{}:0", host_of(&authority))
            .to_socket_addrs()
            .map(|addresses| addresses.map(|address| address.ip()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(
            resolved.iter().all(|address| address.is_loopback()),
            "case {case:?}: the published host must never resolve off this machine, got \
             {resolved:?} for {:?}",
            api.api_endpoint
        );

        // One socket serves the account's apis, so two published addresses name two hosts on
        // one port: the HOST is what says which api answers.
        assert_ne!(
            authority, twin_authority,
            "case {case:?}: expected each api published under its own host, got {authority:?} \
             for both"
        );
        assert_eq!(
            dialable_port(case, &authority),
            dialable_port(case, &twin_authority),
            "case {case:?}: expected both apis published on one socket, got {authority:?} and \
             {twin_authority:?}"
        );

        // The invoke URL a client builds from the published value: the stage name appended,
        // which for the stage that serves the api's root is no segment at all.
        let interface = format!("127.0.0.1:{}", dialable_port(case, &authority));
        for (form, path) in [
            ("the published endpoint on its own", ROUTE_PATH.to_string()),
            (
                "the published endpoint with the stage name appended",
                format!("/{NAMED_STAGE}{ROUTE_PATH}"),
            ),
        ] {
            for (which, named, api_id) in [
                ("the api", &authority, &api.api_id),
                ("its twin", &twin_authority, &twin.api_id),
            ] {
                let answered = request(case, &interface, "GET", &path, named, &[]).await;
                assert_eq!(
                    answered.status, SERVED,
                    "case {case:?}: expected {which} ({api_id}) served at {form} ({path}) under \
                     {named:?}, got {answered:?}"
                );
                assert_eq!(
                    answered.field("location"),
                    Some(UPSTREAM),
                    "case {case:?}: expected the request sent to the upstream {which}'s route \
                     names at {form} ({path}), got {answered:?}"
                );
            }
        }
    }
}
