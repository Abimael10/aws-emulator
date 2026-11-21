// SPDX-License-Identifier: CC0-1.0

//! The state the Extensions and Telemetry API of ONE invocation are served from.
//!
//! An extension REGISTERS with the execution environment it runs in: what it gets back is an
//! identifier, and every later request it makes carries that identifier. So the registration
//! is state — it is minted by one request and read by the next — and this module is where the
//! invocation holds it: the registrations made in this sandbox, and the telemetry
//! subscription each of them holds. Nothing here is shared with another invocation: the
//! sandbox a run is given is its own ([`super::super::AwsAccess::in_sandbox`]), so an
//! identifier minted in one run is not a registration in another.
//!
//! The facts beside them are the function's own, read off the invocation this sandbox serves,
//! because they are what the register call ANSWERS: "Response body contains the function
//! name, version, and handler name" of the function whose environment the extension
//! registered in.

use ::std::sync::{Arc, Mutex, PoisonError};

/// What the register call reports about the function whose execution environment it is
/// serving, read off the invocation itself.
#[derive(Debug, Clone)]
pub struct FunctionFacts {
    /// The function's name, as the invocation names it.
    pub function_name: String,
    /// The version of the function this invocation executed.
    pub function_version: String,
    /// The handler the function is configured with.
    pub handler: String,
    /// The account the function belongs to.
    pub account_id: String,
}

/// ONE extension registered in this sandbox.
#[derive(Debug, Clone)]
pub struct Registration {
    /// The identifier minted for it, which every later request of that extension carries.
    pub identifier: String,
    /// The extension's own name, as it registered under.
    pub name: String,
    /// The lifecycle events it registered for.
    pub events: Vec<String>,
    /// The telemetry stream it subscribed to, for a registration that has subscribed.
    pub telemetry: Option<Subscription>,
}

/// The telemetry subscription one registration holds, as the subscribe call stated it with
/// every bound it left out resolved to its default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// The schema version the events are to be rendered in.
    pub schema_version: String,
    /// The telemetry streams subscribed to.
    pub types: Vec<String>,
    /// The protocol the destination is reached over.
    pub protocol: String,
    /// The destination the events are delivered to.
    pub uri: String,
    /// How the delivery is buffered.
    pub buffering: Buffering,
}

/// The buffering one subscription asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffering {
    /// The most events one delivery carries.
    pub max_items: u64,
    /// The most bytes one delivery carries.
    pub max_bytes: u64,
    /// How long events are held before a delivery is made, in milliseconds.
    pub timeout_ms: u64,
}

/// The Extensions API state of one invocation: the function's facts, and the registrations
/// made against it.
///
/// It is `Clone` and shares one set of registrations between its clones, because the seam a
/// run reaches this emulator through is cloned for the pieces of the run that need it
/// ([`super::super::AwsAccess`]) and they are all inside ONE sandbox.
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// The function this sandbox is running.
    facts: Arc<FunctionFacts>,
    /// Every extension registered in it, in the order they registered.
    registered: Arc<Mutex<Vec<Registration>>>,
}

impl Sandbox {
    /// The sandbox of an invocation of the function `facts` describes, with nothing registered
    /// in it yet.
    #[must_use]
    pub fn new(facts: FunctionFacts) -> Self {
        Self {
            facts: Arc::new(facts),
            registered: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The function this sandbox is running.
    #[must_use]
    pub fn facts(&self) -> &FunctionFacts {
        &self.facts
    }

    /// Register an extension called `name` for `events`, and mint the identifier its later
    /// requests carry.
    ///
    /// The identifier is a fresh UUID, as the one AWS mints is ("Generated unique agent
    /// identifier (UUID string)"), so two registrations in one sandbox are two registrations.
    ///
    /// How many registrations one sandbox can come to hold is bounded by the bound on the
    /// requests the run that owns it may make at all
    /// (`super::super::aws_api::MAX_CALLS_PER_RUN`): each register call spends one of them, and
    /// the sandbox itself lives no longer than the invocation.
    pub fn register(&self, name: &str, events: Vec<String>) -> String {
        let identifier = ::uuid::Uuid::new_v4().to_string();
        self.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Registration {
                identifier: identifier.clone(),
                name: name.to_string(),
                events,
                telemetry: None,
            });
        identifier
    }

    /// The registration `identifier` names, or `None` for an identifier this sandbox minted
    /// nothing for — which is an extension that is not registered here.
    #[must_use]
    pub fn registration(&self, identifier: &str) -> Option<Registration> {
        self.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|held| held.identifier == identifier)
            .cloned()
    }

    /// Record `subscription` as the telemetry stream the registration `identifier` names is
    /// subscribed to, replacing whatever it held before — a later subscribe of one extension
    /// is its subscription.
    ///
    /// `false` where no registration carries that identifier, in which case nothing is
    /// recorded.
    pub fn subscribe(&self, identifier: &str, subscription: Subscription) -> bool {
        let mut held = self
            .registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match held
            .iter_mut()
            .find(|held| held.identifier == identifier)
        {
            Some(registration) => {
                registration.telemetry = Some(subscription);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The facts of a sandbox under test.
    fn facts() -> FunctionFacts {
        FunctionFacts {
            function_name: "probe".to_string(),
            function_version: "$LATEST".to_string(),
            handler: "lambda_function.lambda_handler".to_string(),
            account_id: "000000000000".to_string(),
        }
    }

    /// A registration is state: the identifier one register mints resolves to that
    /// registration and to no other, an identifier nothing minted resolves to nothing, and a
    /// subscription is held against the registration it was made for.
    #[test]
    fn a_registration_is_held_against_the_identifier_it_minted() {
        let sandbox = Sandbox::new(facts());
        let first = sandbox.register("first-extension", vec!["INVOKE".to_string()]);
        let second = sandbox.register("second-extension", vec!["SHUTDOWN".to_string()]);
        assert_ne!(
            first, second,
            "case \"two registrations\": expected two identifiers, got {first:?} twice"
        );
        let held = sandbox
            .registration(&first)
            .expect("the first registration must be held");
        assert_eq!(
            held.name, "first-extension",
            "case \"the first registration\": expected its own name, got {:?}",
            held.name
        );
        assert!(
            sandbox.registration("not-an-identifier").is_none(),
            "case \"an identifier nothing minted\": expected no registration, got one"
        );
        let subscription = Subscription {
            schema_version: "2022-12-13".to_string(),
            types: vec!["platform".to_string()],
            protocol: "HTTP".to_string(),
            uri: "http://sandbox.localdomain:8080/events".to_string(),
            buffering: Buffering {
                max_items: 1000,
                max_bytes: 262_144,
                timeout_ms: 1000,
            },
        };
        assert!(
            sandbox.subscribe(&second, subscription.clone()),
            "case \"subscribing a registration\": expected it to be recorded, got no \
             registration for the identifier"
        );
        assert!(
            !sandbox.subscribe("not-an-identifier", subscription.clone()),
            "case \"subscribing an identifier nothing minted\": expected nothing recorded, \
             got a subscription"
        );
        assert_eq!(
            sandbox
                .registration(&second)
                .and_then(|held| held.telemetry),
            Some(subscription),
            "case \"the subscribed registration\": expected its own subscription, got none"
        );
        assert_eq!(
            sandbox.registration(&first).and_then(|held| held.telemetry),
            None,
            "case \"the other registration\": expected no subscription, got one"
        );
    }
}
