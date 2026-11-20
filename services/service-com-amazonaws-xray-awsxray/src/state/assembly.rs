// SPDX-License-Identifier: CC0-1.0

//! How the documents of one trace are ASSEMBLED into the tree a reader is
//! answered with.
//!
//! A producer does not submit one document per trace: it submits one per ENTITY,
//! and an entity whose parent is still open — or whose parent is reported by
//! another service altogether — travels on its own, as a document whose `type`
//! is `subsegment` and whose `parent_id` names the entity it belongs inside. An
//! instrumented function is the ordinary case: its runtime submits the segment
//! covering the run once the run is over, while the handler's own subsegments go
//! to the daemon as they complete, so they reach X-Ray FIRST and name a parent
//! that nothing has reported yet.
//!
//! A trace is one TREE, so reading it puts those entities back where they
//! belong: a submitted subsegment document naming an entity this trace holds is
//! answered for INSIDE that entity's own document rather than beside it, however
//! deep the entity sits and whichever order the two arrived in. What is left at
//! the top is what a trace's roots and its top-level segments really are: the
//! segments services reported, the nodes this service inferred for the calls they
//! made ([`super::inferred`]), and any entity whose parent this trace does not
//! hold at all — which is kept as it came rather than dropped, because the
//! parent may still arrive.
//!
//! Assembly is a READ: nothing here is stored, and a document that adopted
//! nothing is answered with the bytes it was submitted as. One that DID adopt is
//! read and written again through [`super::segment_document`], which is where
//! what that round trip keeps is written down — every member of every entity,
//! its value and the order its own document named it in.
//!
//! The work is bounded by what the store already bounds — a trace holds at most
//! [`MAX_SEGMENTS_PER_TRACE`] documents — and the nesting it builds by
//! [`MAX_ASSEMBLED_DEPTH`]: an entity whose chain of submitted parents is longer
//! than that stays at the top, so no chain of documents a client sends can nest
//! deeper than a reader of the answer can walk.

use super::*;

/// How many levels of ADOPTED nesting one assembled document carries at most.
///
/// The same bound a submitted document's own nesting is held to
/// ([`MAX_SUBSEGMENT_DEPTH`]), for the same reason: every reader of the answer
/// walks the tree, and the depth of that walk is the one cost a client would
/// otherwise choose. An entity deeper than this is answered for at the top of
/// the trace instead, where it is still readable.
pub const MAX_ASSEMBLED_DEPTH: usize = MAX_SUBSEGMENT_DEPTH;

/// The most entities ONE assembly visits, across every document of the trace it
/// covers.
///
/// The same bound, for the same reason, as the one this service holds its graph
/// derivation to ([`MAX_GRAPH_SUBSEGMENT_VISITS`]): a trace holds at most
/// [`MAX_SEGMENTS_PER_TRACE`] documents and a document at most
/// [`MAX_SUBSEGMENTS_PER_SEGMENT`] entities, so capping each factor still leaves
/// their product to grow, and this caps the product. Past it the trace is
/// answered with the documents still unassembled, each as it was submitted:
/// every entity is still there to read, and nothing a client submits can make
/// one read walk more than this.
pub const MAX_ASSEMBLY_VISITS: usize = MAX_GRAPH_SUBSEGMENT_VISITS;

/// One document of an assembled trace: the entity's id, and the document a
/// reader is answered with.
#[derive(Debug)]
pub struct AssembledSegment {
    /// The entity's `id`, which `Segment$Id` reports.
    pub id: String,
    /// `Segment$Document` — the submitted bytes where this entity adopted
    /// nothing, and the entity with its adopted children inside it where it did.
    pub document: String,
}

/// One stored segment under assembly.
struct Holder<'a> {
    /// The stored record.
    record: &'a SegmentRecord,
    /// Where this document attaches: the holder hosting the entity it named as
    /// its parent, and that entity's id. `None` for a document that stays at the
    /// top of the trace.
    attach: Option<(usize, String)>,
    /// The holders that attach INTO this one, in submission order.
    children: Vec<usize>,
    /// How many levels of adopted nesting this document sits under.
    depth: usize,
    /// The document once it adopted its children; `None` for one that adopted
    /// none, which is answered with the bytes it was submitted as.
    assembled: Option<String>,
}

/// The documents of `trace`, assembled — see the module docs for the rule.
pub fn assembled_documents(trace: &TraceRecord) -> Vec<AssembledSegment> {
    let mut holders: Vec<Holder<'_>> = trace
        .segments
        .iter()
        .map(|record| Holder {
            record,
            attach: None,
            children: Vec::new(),
            depth: 0,
            assembled: None,
        })
        .collect();

    // Which stored document holds each entity of the trace: its own id, and the
    // ids of the subsegments carried inside it. Walked once, under the budget
    // this whole assembly is held to — a document reached past it hosts nothing,
    // so its own entities are answered for where they already are.
    let mut budget = MAX_ASSEMBLY_VISITS;
    let mut hosts: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, holder) in holders.iter().enumerate() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        hosts.insert(holder.record.id.as_str(), index);
        let mut pending: Vec<&SubsegmentRecord> = holder.record.subsegments.iter().collect();
        while let Some(subsegment) = pending.pop() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            if let Some(id) = subsegment.id.as_deref() {
                hosts.insert(id, index);
            }
            pending.extend(subsegment.subsegments.iter());
        }
    }

    // A SUBMITTED subsegment document belongs inside the entity it named, where
    // this trace holds that entity. A segment is never nested: it reports a
    // service's own work, and its `parent_id` is an EDGE of the tree rather than
    // containment — which is what keeps an inferred node, and a service's own
    // segment, at the top.
    for (index, holder) in holders.iter_mut().enumerate() {
        if !holder.record.is_subsegment() {
            continue;
        }
        let Some(parent_id) = holder.record.parent_id.clone() else {
            continue;
        };
        let Some(&host) = hosts.get(parent_id.as_str()) else {
            continue;
        };
        if host == index {
            continue;
        }
        holder.attach = Some((host, parent_id));
    }

    // How deep each document sits, following the chain of documents it attaches
    // through. A chain longer than the bound, or one that closes on itself — two
    // documents each naming an entity of the other — is cut: those documents stay
    // at the top rather than nesting without end.
    let depths: Vec<usize> = (0..holders.len())
        .map(|index| {
            let mut depth = 0;
            let mut at = index;
            while let Some((host, _)) = holders[at].attach.clone() {
                depth += 1;
                at = host;
                if depth > MAX_ASSEMBLED_DEPTH {
                    break;
                }
            }
            depth
        })
        .collect();
    for (holder, depth) in holders.iter_mut().zip(depths) {
        holder.depth = depth;
    }
    for holder in &mut holders {
        if holder.depth > MAX_ASSEMBLED_DEPTH {
            holder.attach = None;
            holder.depth = 0;
        }
    }
    let attached: Vec<(usize, usize)> = holders
        .iter()
        .enumerate()
        .filter_map(|(index, holder)| holder.attach.as_ref().map(|(host, _)| (*host, index)))
        .collect();
    for (host, child) in attached {
        holders[host].children.push(child);
    }

    // Built deepest FIRST, so a document is adopted into its host only once it
    // carries its own adopted children: one pass over the trace, no recursion
    // through the chain.
    let mut order: Vec<usize> = (0..holders.len()).collect();
    order.sort_by(|left, right| holders[*right].depth.cmp(&holders[*left].depth));
    for index in order {
        if holders[index].children.is_empty() {
            continue;
        }
        let Some(mut value) = read_document(&holders[index].record.document) else {
            // A stored document always parsed at ingest, so this is unreachable;
            // one that somehow did not adopts nothing, and its children stay at
            // the top of the trace.
            for child in holders[index].children.clone() {
                holders[child].attach = None;
            }
            continue;
        };
        for child in holders[index].children.clone() {
            let text = holders[child]
                .assembled
                .clone()
                .unwrap_or_else(|| holders[child].record.document.clone());
            let mut nested = read_document(&text);
            let at = holders[child]
                .attach
                .clone()
                .map(|(_, entity)| entity)
                .unwrap_or_default();
            // A child this document could not take in — because the entity it
            // named is not in this document after all, or because the budget this
            // assembly is held to is spent — is answered for at the top of the
            // trace, so no entity is lost by being adopted.
            if !nest(&mut value, &at, &mut nested, &mut budget) {
                holders[child].attach = None;
            }
        }
        match written_document(&value) {
            Some(document) => holders[index].assembled = Some(document),
            // A document read from JSON is always written again, so this is
            // unreachable; one that somehow was not keeps the bytes it was
            // submitted as, and the children it took in are answered for at the
            // top of the trace rather than lost inside it.
            None => {
                for child in holders[index].children.clone() {
                    holders[child].attach = None;
                }
            }
        }
    }

    holders
        .iter()
        .filter(|holder| holder.attach.is_none())
        .map(|holder| AssembledSegment {
            id: holder.record.id.clone(),
            document: holder
                .assembled
                .clone()
                .unwrap_or_else(|| holder.record.document.clone()),
        })
        .collect()
}

/// Put `child` inside the entity of `host` whose id is `parent_id`, answering
/// whether that entity was found and took it in.
///
/// `child` is MOVED into the entity that matched, and left where it is for a
/// caller to answer for itself when none did: the entity is walked for the id
/// and the document is taken in once, rather than copied at each entity on the
/// way. `budget` is the whole assembly's, so the walk cannot cost more than the
/// trace is allowed in total.
///
/// A child the entity already carries under the same id is REPLACED rather than
/// added beside, which is how a re-submitted document overwrites the one it
/// supersedes.
fn nest(
    host: &mut serde_json::Value,
    parent_id: &str,
    child: &mut Option<serde_json::Value>,
    budget: &mut usize,
) -> bool {
    if *budget == 0 || child.is_none() {
        return false;
    }
    *budget -= 1;
    if host.get("id").and_then(serde_json::Value::as_str) == Some(parent_id) {
        let Some(taken) = child.take() else {
            return false;
        };
        let named = taken
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let Some(object) = host.as_object_mut() else {
            return false;
        };
        let carried = object
            .entry("subsegments".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        let Some(entities) = carried.as_array_mut() else {
            return false;
        };
        match entities.iter().position(|entity| {
            entity.get("id").and_then(serde_json::Value::as_str) == named.as_deref()
        }) {
            Some(at) if named.is_some() => entities[at] = taken,
            _ => entities.push(taken),
        }
        return true;
    }
    let Some(entities) = host
        .get_mut("subsegments")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    for entity in entities {
        if nest(entity, parent_id, child, budget) {
            return true;
        }
    }
    false
}
