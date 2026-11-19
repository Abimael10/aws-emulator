// SPDX-License-Identifier: CC0-1.0

//! The `address:port` pairs a WORKLOAD this emulator runs answers an HTTP request at, and what
//! the server behind each of them answers.
//!
//! A load balancer forwarding to a target reaches that target at an ADDRESS — "ip - Targets are
//! specified by IP address" — on "the port on which the target is listening", and what answers
//! is the server the workload at that address RUNS there. Some of those workloads belong to a
//! service that cannot be asked directly: the service holding the workload already reads the
//! load balancer's own store to register its targets, so the load balancer cannot read that
//! service's store back without the two depending on each other. This register is the
//! one-directional seam between them: the service that STARTED the server records it here,
//! under the address and port it is reached at, and whatever has to serve a request addressed
//! there reads it back.
//!
//! So the whole of what such a reader can reach is what a service put here. An `address:port`
//! absent from these records is one no workload of this emulator is listening at, and a request
//! addressed there is answered the way a host answers a connection nothing is listening for —
//! which is the caller's own decision, not this register's.
//!
//! Engine-scoped, like every other store two services on either side of a hand-off share: the
//! records live in the SAME shared engine as the services' own, so a server one service started
//! is the one another resolves, and two engines in one process share nothing. An engine this
//! emulator did not build vends no store, and then an address resolves to nothing — the
//! position every reader was in before a service recorded one.
//!
//! LOCK ORDER: these records are taken LAST, and never held while a server answers. A publisher
//! holds its own service's store while it records a server ([`ServedAddresses::publish`]) and
//! while the handle it was answered with is dropped ([`ServedAddressHandle`]), and nothing here
//! takes any other lock; [`ServedAddresses::serve`] clones the server out of the store and
//! releases the guard before asking it anything.

use std::collections::BTreeMap;
use std::sync::Arc;

/// What one request to a workload's own server answered: the status it answered with, the media
/// type of the document it served where it served one, and that document's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressAnswer {
    /// The status the server answered with.
    pub status: u16,
    /// The media type of the document it served, where it served one.
    pub content_type: Option<String>,
    /// The bytes of that document.
    pub body: Vec<u8>,
}

/// A server one workload of this emulator is listening with.
///
/// Implemented by the service that STARTED the server, over that workload's own state — so what
/// is answered here is computed from what the workload really holds and never invented by the
/// reader.
pub trait ServesHttp: Send + Sync + 'static {
    /// What this server answers for a request for `path` with `method`, or `None` when it
    /// answers nothing at all for that request — which the caller reports as a connection that
    /// could not be served rather than as a response.
    fn serve(&self, method: &str, path: &str) -> Option<AddressAnswer>;

    /// Whether the documents this server answers out of are ones this emulator HOLDS.
    ///
    /// `true` by default: a server answers out of the documents the workload's own program
    /// wrote, which is what the service that started it holds. A server over a directory
    /// holding none of them is the exception — what it would serve there came with the image or
    /// the package that shipped the server, which the service holding the workload models
    /// nothing of — so a reader that judges the WORKLOAD by a status (a load balancer's health
    /// check against its success criterion) learns nothing from that server's answer and must
    /// not read one into it.
    fn holds_what_it_serves(&self) -> bool {
        true
    }
}

/// The servers this emulator's workloads are listening with, by the `(address, port)` each is
/// reached at.
///
/// Held once per engine. The port is part of the key because one workload may listen on several
/// of them, and two workloads never share an address.
#[derive(Default)]
pub struct ServedHttpAddresses {
    answering: BTreeMap<(String, i32), Listening>,
    /// What the NEXT record is marked with, so a handle can tell its own record from the one a
    /// later publish put in its place ([`ServedAddressHandle`]).
    next_mark: u64,
}

/// One record of the register: which publish put it there, and the server it holds.
struct Listening {
    /// The mark the publish that made this record was answered with. A later publish for the
    /// same `(address, port)` takes a new one, which is what keeps a handle from withdrawing a
    /// successor's server.
    mark: u64,
    /// The server itself.
    server: Arc<dyn ServesHttp>,
}

/// Written by hand rather than derived: a server carries no `Debug` of its own, and what is
/// diagnosable about these records is how many addresses they hold — never a workload's
/// addresses in a log line.
impl std::fmt::Debug for ServedHttpAddresses {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServedHttpAddresses")
            .field("listening", &self.answering.len())
            .finish()
    }
}

/// A handle on one engine's [`ServedHttpAddresses`].
///
/// Clone-cheap (an `Arc` inside) and `'static`, so the service that started a server can keep
/// one beside the run performing it and a reader can hold one across the request it serves.
#[derive(Clone)]
pub struct ServedAddresses(::engine::ServiceState<ServedHttpAddresses>);

impl std::fmt::Debug for ServedAddresses {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServedAddresses")
            .field("listening", &self.0.lock().answering.len())
            .finish()
    }
}

impl ServedAddresses {
    /// The records of the SAME shared engine the services' own stores live in, or `None` for an
    /// engine that vends none.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        (engine as &dyn ::std::any::Any)
            .downcast_ref::<::engine::InMemoryEngine>()
            .map(|engine| Self(engine.state::<ServedHttpAddresses>()))
    }

    /// Record that `server` is listening at `address` on `port`, and answer the handle that
    /// keeps it there: the server stops being reachable when that handle is dropped, so a
    /// workload that is gone leaves nothing listening behind it.
    ///
    /// Recording the same `(address, port)` again replaces what was there, which is what an
    /// address a later workload was given needs.
    pub fn publish(
        &self,
        address: &str,
        port: i32,
        server: Arc<dyn ServesHttp>,
    ) -> ServedAddressHandle {
        let key = key(address, port);
        let mark = {
            let mut store = self.0.lock();
            store.next_mark = store.next_mark.wrapping_add(1);
            let mark = store.next_mark;
            store.answering.insert(key.clone(), Listening { mark, server });
            mark
        };
        ServedAddressHandle {
            addresses: self.clone(),
            key,
            mark,
        }
    }

    /// What the workload listening at `address` on `port` answers for a request for `path` with
    /// `method` — `None` when nothing of this emulator is listening there, or when what is
    /// listening answers nothing for that request.
    ///
    /// A READ: serving a request moves nothing of these records and nothing of the workload.
    /// The server is cloned out of the store before it is asked anything, so no guard of this
    /// store is held while it answers (see this module's header).
    #[must_use]
    pub fn serve(
        &self,
        address: &str,
        port: i32,
        method: &str,
        path: &str,
    ) -> Option<AddressAnswer> {
        self.served(address, port, method, path)
            .map(|(answered, _)| answered)
    }

    /// The same answer, beside whether the server that produced it answers out of documents
    /// this emulator HOLDS ([`ServesHttp::holds_what_it_serves`]).
    ///
    /// Both are read off the SAME server, so a caller that judges a workload by a status cannot
    /// read the status off one server and that question off another.
    #[must_use]
    pub fn served(
        &self,
        address: &str,
        port: i32,
        method: &str,
        path: &str,
    ) -> Option<(AddressAnswer, bool)> {
        let server = {
            let store = self.0.lock();
            Arc::clone(&store.answering.get(&key(address, port))?.server)
        };
        let holds_what_it_serves = server.holds_what_it_serves();
        Some((server.serve(method, path)?, holds_what_it_serves))
    }

    /// Forget the `(address, port)` `key` names, where it still holds the record `mark` was
    /// answered for: a server a LATER publish put at the same address is that publish's to
    /// withdraw, so a handle dropped after it is replaced un-listens nothing.
    fn withdraw(&self, key: &(String, i32), mark: u64) {
        let mut store = self.0.lock();
        if store
            .answering
            .get(key)
            .is_some_and(|listening| listening.mark == mark)
        {
            store.answering.remove(key);
        }
    }
}

/// What keeps one server in the records: dropping it is the server stopping.
///
/// Held by the run performing the workload, so a run that ends — or one this emulator gives up
/// on and drops — leaves nothing listening at the address it had.
pub struct ServedAddressHandle {
    /// The records this handle's server is in.
    addresses: ServedAddresses,
    /// The key it is held under.
    key: (String, i32),
    /// The mark the publish that made it was answered with, which is what tells its own record
    /// from a successor's.
    mark: u64,
}

impl std::fmt::Debug for ServedAddressHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServedAddressHandle")
            .field("port", &self.key.1)
            .finish()
    }
}

impl Drop for ServedAddressHandle {
    fn drop(&mut self) {
        self.addresses.withdraw(&self.key, self.mark);
    }
}

/// One `(address, port)` as it is keyed: the address as the workload's own service wrote it,
/// trimmed and lower-cased, so a reader that was handed the same address in another case
/// resolves the same record.
fn key(address: &str, port: i32) -> (String, i32) {
    (address.trim().to_ascii_lowercase(), port)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One server for the rows below, answering the path it was asked for.
    #[derive(Debug)]
    struct Echoing {
        /// What the server names itself in the body, so a row can tell two of them apart.
        name: &'static str,
        /// The paths it answers at all.
        answers: &'static [&'static str],
    }

    impl ServesHttp for Echoing {
        fn serve(&self, method: &str, path: &str) -> Option<AddressAnswer> {
            self.answers.contains(&path).then(|| AddressAnswer {
                status: 200,
                content_type: Some("text/plain".to_string()),
                body: format!("{} {method} {path}", self.name).into_bytes(),
            })
        }
    }

    /// RULE: an `(address, port)` is served by the server PUBLISHED for it until the handle that
    /// published it is dropped; one nothing published is served by nothing.
    ///
    /// The rows span what the reading generalises over: two ports of one address, two addresses,
    /// the case a reader writes an address in, a port nothing listens on, an address nothing
    /// listens at, and a request the published server itself answers nothing for.
    #[test]
    fn an_address_is_served_by_the_server_published_for_it() {
        let engine = ::engine::InMemoryEngine::new();
        let addresses = ServedAddresses::of(&engine).expect("the engine vends the records");
        let web = addresses.publish(
            "10.0.1.7",
            80,
            Arc::new(Echoing {
                name: "web",
                answers: &["/"],
            }),
        );
        let admin = addresses.publish(
            "10.0.1.7",
            8080,
            Arc::new(Echoing {
                name: "admin",
                answers: &["/", "/status"],
            }),
        );
        let other = addresses.publish(
            "10.0.2.9",
            80,
            Arc::new(Echoing {
                name: "other",
                answers: &["/"],
            }),
        );
        let rows: &[(&str, &str, i32, &str, Option<&str>)] = &[
            ("the server on the first port", "10.0.1.7", 80, "/", Some("web GET /")),
            (
                "the server on the second port of the same address",
                "10.0.1.7",
                8080,
                "/status",
                Some("admin GET /status"),
            ),
            ("a second address", "10.0.2.9", 80, "/", Some("other GET /")),
            (
                "the same address written in another case",
                "10.0.1.7 ",
                80,
                "/",
                Some("web GET /"),
            ),
            ("a port nothing listens on", "10.0.1.7", 443, "/", None),
            ("an address nothing listens at", "10.0.9.9", 80, "/", None),
            (
                "a path the published server answers nothing for",
                "10.0.1.7",
                80,
                "/missing",
                None,
            ),
        ];
        for (case, address, port, path, expected) in rows {
            let answered = addresses.serve(address, *port, "GET", path);
            let body = answered
                .as_ref()
                .map(|answer| String::from_utf8_lossy(&answer.body).to_string());
            assert_eq!(
                body.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {body:?}"
            );
        }
        // The handle is what keeps a server listening: dropping it leaves the address with
        // nothing at it, and the other records untouched.
        drop(web);
        assert!(
            addresses.serve("10.0.1.7", 80, "GET", "/").is_none(),
            "case \"a withdrawn server\": expected nothing listening, got a response"
        );
        assert!(
            addresses.serve("10.0.1.7", 8080, "GET", "/").is_some(),
            "case \"a sibling port of a withdrawn server\": expected it still listening"
        );
        drop(admin);
        drop(other);
        assert!(
            addresses.serve("10.0.2.9", 80, "GET", "/").is_none(),
            "case \"every server withdrawn\": expected nothing listening"
        );
        // A server a LATER publish put at an address is that publish's: dropping the handle the
        // EARLIER one was answered with leaves the successor listening, which is what an address
        // a second workload was given needs.
        let first = addresses.publish(
            "10.0.3.4",
            80,
            Arc::new(Echoing {
                name: "first",
                answers: &["/"],
            }),
        );
        let successor = addresses.publish(
            "10.0.3.4",
            80,
            Arc::new(Echoing {
                name: "successor",
                answers: &["/"],
            }),
        );
        drop(first);
        let answered = addresses
            .serve("10.0.3.4", 80, "GET", "/")
            .map(|answer| String::from_utf8_lossy(&answer.body).to_string());
        assert_eq!(
            answered.as_deref(),
            Some("successor GET /"),
            "case \"a replaced server\": expected the successor still listening, got {answered:?}"
        );
        drop(successor);
        assert!(
            addresses.serve("10.0.3.4", 80, "GET", "/").is_none(),
            "case \"the successor withdrawn\": expected nothing listening"
        );
    }

    /// A server that holds NONE of the documents it serves: what it would answer came with
    /// whatever shipped it, which no service of this emulator models.
    #[derive(Debug)]
    struct Shipped;

    impl ServesHttp for Shipped {
        fn serve(&self, _method: &str, _path: &str) -> Option<AddressAnswer> {
            Some(AddressAnswer {
                status: 404,
                content_type: None,
                body: Vec::new(),
            })
        }

        fn holds_what_it_serves(&self) -> bool {
            false
        }
    }

    /// RULE: a reader is told whether the server that answered holds the documents it serves,
    /// as that server itself says — and nothing at all about an address nothing listens at.
    #[test]
    fn an_answer_carries_whether_its_server_holds_what_it_serves() {
        let engine = ::engine::InMemoryEngine::new();
        let addresses = ServedAddresses::of(&engine).expect("the engine vends the records");
        let _own = addresses.publish(
            "10.0.4.1",
            80,
            Arc::new(Echoing {
                name: "own",
                answers: &["/"],
            }),
        );
        let _shipped = addresses.publish("10.0.4.2", 80, Arc::new(Shipped));
        let rows: &[(&str, &str, &str, Option<bool>)] = &[
            (
                "a server answering out of the documents the workload wrote",
                "10.0.4.1",
                "/",
                Some(true),
            ),
            (
                "a server holding none of what it serves",
                "10.0.4.2",
                "/anything",
                Some(false),
            ),
            ("an address nothing listens at", "10.0.4.9", "/", None),
        ];
        for (case, address, path, expected) in rows {
            let held = addresses
                .served(address, 80, "GET", path)
                .map(|(_, holds)| holds);
            assert_eq!(
                held, *expected,
                "case {case:?}: expected {expected:?}, got {held:?}"
            );
        }
    }
}
