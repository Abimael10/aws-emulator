// SPDX-License-Identifier: CC0-1.0

//! Not part of the crate: no module declares this file and it holds no code.
//!
//! The VPC a function is attached to — the route a datagram of the run takes into it and the
//! DNS that VPC answers — is `super::vpc_network`, which mainline landed while this fix was in
//! review; this file held a second implementation of the same rule, which was dropped in the
//! merge so the crate keeps ONE. The tooling available here cannot delete a file once written,
//! so what is left of it is this note.
