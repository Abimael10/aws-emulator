// SPDX-License-Identifier: CC0-1.0

//! EXECUTING a stack refactor: what `ExecuteStackRefactor` carries out against the stacks the
//! refactor covers.
//!
//! "Executes the stack refactor operation", and a refactor's actions say what that does: every
//! stack the plan is to `CREATE` is created, and every resource it is to `MOVE` leaves the
//! stack that holds it for the stack that is to hold it
//! ([`super::stack_refactor_actions`]). A refactor is not a deployment: the resource that moves
//! is the SAME physical instance, so it keeps its own `PhysicalResourceId` and its attributes
//! and arrives in its new stack under the logical id the move names — which is what makes
//! renaming a resource, or moving it into another stack, something other than deleting one
//! resource and creating another.
//!
//! ## What a stack ends up with
//!
//! `StackDefinition` is "the stack and the template used by the stack", so the template each
//! definition submitted is the one its stack carries once the refactor has been executed: the
//! final state the refactor was created for. A stack the refactor only takes a resource out of
//! or puts one into, and whose definition submitted no template, keeps the template it has.
//!
//! A resource a stack takes over this way is one it did not create, so the stack REMEMBERS the
//! identity it arrived with ([`StoredStack::adopted`]) exactly as an imported resource's is
//! remembered: the deployment that follows the refactor recomputes every other resource's
//! identity from the template it deploys, and without that record it would read the moved
//! resource as a different one and replace the resource the refactor just moved.
//!
//! ## The execution rides the clock
//!
//! The call is answered while the refactor is still being carried out, which is what the
//! modelled `EXECUTE_IN_PROGRESS` is for: the record reports it until the conclusion the clock
//! owes falls due ([`super::stack_refactors::StoredStackRefactor::begin_execution`]), and each
//! stack the refactor acts on rides its own clock through the operation the refactor opens on
//! it. Nothing sleeps, and nothing advances because it was read.
//!
//! ## A plan that no longer holds
//!
//! The plan was concluded when the refactor was CREATED, against the stacks of that instant,
//! and the stacks can have moved on since: a stack the plan names can have been deleted, a
//! resource it is to move can be gone, and a stack it acts on can have an operation of its own
//! in flight. So the plan is checked against the store as it stands
//! ([`ComAmazonawsCloudformationCloudformationState::refactor_cannot_be_carried_out`]) and an
//! execution that cannot be carried out concludes `EXECUTE_FAILED` with that one reason, having
//! changed nothing — never a completion reporting moves nothing performed.

use super::*;

/// The final state ONE stack of a refactor is to end up in, prepared OFF the store lock.
///
/// Reading a submitted template body is caller-sized work — the processed stage of a document
/// and the dependency relation it describes — and no request does that while every other
/// request waits on the store lock, which is why the execution is handed these rather than the
/// bodies ([`prepare`]).
#[derive(Debug, Clone)]
pub struct PreparedFinalState {
    /// `StackDefinition$StackName`: "the name associated with the stack", as the refactor
    /// named it.
    pub stack_name: String,
    /// The body the stack is to carry once the refactor has been executed.
    pub template_body: String,
    /// That body's `Processed` stage, which is the document the stack deploys from.
    pub processed_template: template::Processed,
    /// The direct dependency relation that body describes, keyed by logical id: what each of
    /// the stack's resources waits for, and the order its own deletion walks them in.
    pub dependencies: BTreeMap<String, BTreeSet<String>>,
}

/// The final states a refactor's stack definitions submitted, each body already read.
///
/// A definition that submitted no template says nothing about what its stack is to end up
/// with, so it contributes none: that stack keeps the template it has.
pub fn prepare(
    definitions: &[super::stack_refactors::RefactorStackDefinition],
) -> Vec<PreparedFinalState> {
    definitions
        .iter()
        .filter_map(|definition| {
            let template_body = definition.template_body.clone()?;
            Some(PreparedFinalState {
                stack_name: definition.stack_name.clone(),
                processed_template: template::process(&template_body),
                // A body this service cannot read describes no relation rather than refusing
                // the execution a second time: the submitting call already held it to every
                // template rule this service enforces.
                dependencies: template::parse(&template_body)
                    .as_ref()
                    .map(template::direct_dependencies)
                    .unwrap_or_default(),
                template_body,
            })
        })
        .collect()
}

impl ComAmazonawsCloudformationCloudformationState {
    /// Carry out the refactor `stack_refactor_id` names, at `now`.
    ///
    /// `Err(message)` REFUSES the call: a refactor this Region holds no record of, and one with
    /// no plan anything can execute. Everything else is the execution being accepted — which
    /// is not the same as the plan succeeding: a plan that no longer holds against the stacks
    /// as they stand is accepted and concludes `EXECUTE_FAILED`, with nothing moved and nothing
    /// created ([`Self::refactor_cannot_be_carried_out`]).
    ///
    /// `prepared` is the final state each definition submitted, read off the lock
    /// ([`prepare`]). Settle first: the plan is carried out against the stacks this Region
    /// really holds at `now`.
    pub fn execute_stack_refactor(
        &mut self,
        region: &str,
        stack_refactor_id: &str,
        prepared: &[PreparedFinalState],
        now: SystemTime,
    ) -> Result<(), String> {
        let Some(index) = self
            .stack_refactors
            .iter()
            .position(|refactor| refactor.is_addressed_by(region, stack_refactor_id))
        else {
            return Err(stack_refactors::does_not_exist(stack_refactor_id));
        };
        let refactor = &self.stack_refactors[index];
        if !refactor.is_executable() {
            return Err(stack_refactors::not_executable_in_execution_status(
                stack_refactor_id,
                &refactor.execution_status,
            ));
        }
        // What the plan is to carry out, read off the RECORD rather than worked out again: the
        // actions are the conclusion the creation reached, and executing the refactor is
        // carrying them out ([`super::stack_refactor_actions`]).
        let enable_stack_creation = refactor.enable_stack_creation;
        let named = stack_refactors::named_stacks(
            &refactor.stack_definitions,
            &refactor.resource_mappings,
        );
        let moves: Vec<stack_refactors::RefactorResourceMapping> = refactor
            .actions
            .iter()
            .filter(|action| action.action == stack_refactor_actions::action::MOVE)
            .filter_map(|action| action.resource_mapping.clone())
            .collect();

        if let Some(reason) =
            self.refactor_cannot_be_carried_out(region, &named, enable_stack_creation, &moves)
        {
            self.stack_refactors[index].begin_execution(now, Some(reason));
            return Ok(());
        }

        // A stack has to exist before a resource can be moved into it, so the stacks the plan
        // creates come first — in the order the refactor named them.
        let created = self.create_refactored_stacks(region, &named, prepared, now);
        for mapping in &moves {
            self.move_refactored_resource(region, mapping, now);
        }
        // ...and the template each definition submitted is what its stack carries from here
        // on. A stack this execution CREATED was created with it already, and has no update to
        // report either: it has just come into existence.
        for final_state in prepared {
            self.install_refactored_final_state(region, final_state, &created, now);
        }
        // Every stack the refactor acts on goes through an operation of its own: the stacks it
        // created are creating, and the ones that were already there are being updated.
        for stack_name in &named {
            if created.contains(stack_name) {
                continue;
            }
            let Some(at) = self.position_of(region, stack_name) else {
                continue;
            };
            self.stacks[at].begin_transition(
                now,
                status::UPDATE_IN_PROGRESS,
                status::UPDATE_COMPLETE,
                Some(new_operation_id()),
                Some(new_client_request_token()),
            );
        }
        self.stack_refactors[index].begin_execution(now, None);
        // `StackIds` is "the unique ID for each stack" the refactor covers, and a stack the
        // refactor was still to CREATE had no id to report: it has one now, so the record
        // reports the stack it brought into existence beside the ones it was resolved against.
        let minted: Vec<String> = created
            .iter()
            .filter_map(|stack_name| self.resolve(region, stack_name))
            .map(|stack| stack.stack_id.clone())
            .collect();
        let refactor = &mut self.stack_refactors[index];
        for stack_id in minted {
            if !refactor.stack_ids.contains(&stack_id) {
                refactor.stack_ids.push(stack_id);
            }
        }
        Ok(())
    }

    /// The one reason the plan cannot be carried out against the stacks this Region holds at
    /// this instant, or `None` for a plan that can.
    ///
    /// Three things stop one, and each is read off the store rather than invented:
    ///
    /// * a stack the refactor names that this Region holds no live stack for, while the
    ///   refactor was not asked to create one — `EnableStackCreation` "determines if a new
    ///   stack is created with the refactor", so without it there is no stack to act on;
    /// * a stack the refactor acts on with an operation of its own still in flight, which is
    ///   the state no update of a stack is accepted in ([`super::stack_cannot_be_updated`]):
    ///   CloudFormation runs one operation at a time against a stack, and a refactor that
    ///   moved resources beside one would leave a stack whose resources neither operation
    ///   accounts for; and
    /// * a move whose SOURCE resource the stack that is to lose it does not hold any more,
    ///   which is a resource there is nothing to move.
    pub fn refactor_cannot_be_carried_out(
        &self,
        region: &str,
        named: &[String],
        enable_stack_creation: bool,
        moves: &[stack_refactors::RefactorResourceMapping],
    ) -> Option<String> {
        let mut creating = 0usize;
        for stack_name in named {
            match self.resolve(region, stack_name).filter(|stack| stack.is_live()) {
                Some(stack) if stack.pending.is_some() => {
                    return Some(stack_cannot_be_updated(&stack.stack_id, &stack.status));
                }
                Some(_) => {}
                None if !enable_stack_creation => {
                    return Some(stack_refactors::stack_creation_not_enabled(stack_name));
                }
                // A stack the refactor is to create, which is what that flag allows.
                None => creating += 1,
            }
        }
        // ...and a stack this refactor creates is held to the same ceiling every other stack
        // creation of this service is, so executing refactors cannot grow the store without
        // limit.
        if self.live_stacks(region).count().saturating_add(creating) > MAX_STACKS
            || self.live_stack_count().saturating_add(creating) > MAX_LIVE_STACKS
        {
            return Some(stack_refactors::stack_creation_past_quota());
        }
        for mapping in moves {
            let holds = self
                .resolve(region, &mapping.source.stack_name)
                .filter(|stack| stack.is_live())
                .is_some_and(|stack| {
                    stack
                        .resources
                        .iter()
                        .any(|deployed| holds_logical_id(deployed, &mapping.source))
                });
            if !holds {
                return Some(stack_refactors::source_resource_does_not_exist(
                    &mapping.source,
                ));
            }
        }
        None
    }

    /// Create every stack the plan names that this Region holds none of, answering the names it
    /// created.
    ///
    /// Each one is created with the final state its definition submitted, where it submitted
    /// one, and carries no resources of its own: the resources it is to hold are the ones the
    /// moves bring into it. The stack rides its own clock through the create the refactor
    /// opened on it, exactly as a stack `CreateStack` creates does.
    fn create_refactored_stacks(
        &mut self,
        region: &str,
        named: &[String],
        prepared: &[PreparedFinalState],
        now: SystemTime,
    ) -> BTreeSet<String> {
        let mut created: BTreeSet<String> = BTreeSet::new();
        for stack_name in named {
            if self
                .resolve(region, stack_name)
                .is_some_and(|stack| stack.is_live())
            {
                continue;
            }
            let final_state = prepared
                .iter()
                .find(|final_state| final_state.stack_name == *stack_name);
            let mut stack = StoredStack::new_processed(
                new_stack_id(region, stack_name),
                stack_name.clone(),
                final_state.map(|final_state| final_state.template_body.clone()),
                final_state
                    .map(|final_state| final_state.processed_template.clone())
                    .unwrap_or(template::Processed::Same),
                now,
            );
            stack.region = region.to_string();
            if let Some(final_state) = final_state {
                stack.resource_dependencies = final_state.dependencies.clone();
            }
            stack.begin_transition(
                now,
                status::CREATE_IN_PROGRESS,
                status::CREATE_COMPLETE,
                Some(new_operation_id()),
                Some(new_client_request_token()),
            );
            self.stacks.push(stack);
            created.insert(stack_name.clone());
        }
        created
    }

    /// Move ONE resource: out of the stack that holds it, into the stack that is to hold it,
    /// under the logical id the move names.
    ///
    /// The resource is neither deleted nor created, so its record travels as it stands — the
    /// physical instance, its attributes and the configuration it was provisioned with — and
    /// only what the move changes changes: the logical id it is known by, the stack that
    /// records it, and the resource-level status that says what happened to it. A drift result
    /// does not travel with it: a comparison belongs to the stack and the template it was
    /// carried out against.
    ///
    /// Both ends were resolved before anything was changed, so a move whose ends no longer
    /// resolve does nothing rather than taking a resource out of a stack and dropping it.
    fn move_refactored_resource(
        &mut self,
        region: &str,
        mapping: &stack_refactors::RefactorResourceMapping,
        now: SystemTime,
    ) {
        let Some(source) = self.position_of(region, &mapping.source.stack_name) else {
            return;
        };
        let Some(destination) = self.position_of(region, &mapping.destination.stack_name) else {
            return;
        };
        let Some(held) = self.stacks[source]
            .resources
            .iter()
            .position(|deployed| holds_logical_id(deployed, &mapping.source))
        else {
            return;
        };
        let mut moved = self.stacks[source].resources.remove(held);
        // The resource is no longer this stack's to deploy, so it is no longer one of the
        // identities this stack remembers either.
        self.stacks[source]
            .adopted
            .remove(&mapping.source.logical_resource_id);
        moved.resource.logical_id = mapping.destination.logical_resource_id.clone();
        // "IMPORT_COMPLETE": the stack took over a resource that already exists rather than
        // creating one, which is the resource status this service reports for every resource
        // brought under a stack's management that way ([`resource_status`]).
        moved.status = resource_status::IMPORT_COMPLETE.to_string();
        moved.status_timestamp = now;
        moved.status_reason = None;
        moved.drift_status = resource_drift_status::NOT_CHECKED.to_string();
        moved.drift_checked_at = None;
        moved.drift_detail = None;
        let identity = moved.resource.physical_resource_id.clone();
        let logical_id = moved.resource.logical_id.clone();
        let stack = &mut self.stacks[destination];
        // What nothing else can say: this stack did not create the resource, so every
        // deployment after the refactor keeps the identity it arrived with instead of
        // computing one for a resource CloudFormation was never asked to create.
        stack.adopted.insert(logical_id, identity);
        stack.resources.push(moved);
    }

    /// Install the final state one definition submitted on the stack it names.
    ///
    /// Nothing is installed for a stack this execution CREATED — it was created with that
    /// template — and nothing for a name this Region holds no stack for, which the plan check
    /// already refused.
    fn install_refactored_final_state(
        &mut self,
        region: &str,
        final_state: &PreparedFinalState,
        created: &BTreeSet<String>,
        now: SystemTime,
    ) {
        if created.contains(&final_state.stack_name) {
            return;
        }
        let Some(at) = self.position_of(region, &final_state.stack_name) else {
            return;
        };
        let stack = &mut self.stacks[at];
        stack.set_template_body(
            final_state.template_body.clone(),
            final_state.processed_template.clone(),
        );
        // The relation the template in force describes is the stack's from here on, which is
        // what its own deletion walks.
        stack.resource_dependencies = final_state.dependencies.clone();
        // `Stack$LastUpdatedTime`: "the time the stack was last updated" — the refactor
        // changed what this stack carries, which is that update.
        stack.last_updated_time = Some(now);
    }
}

/// Whether `deployed` is the resource one end of a move addresses, and a resource there is
/// something to move: a record already at its terminal delete status is a resource the stack
/// disposed of rather than one it holds.
fn holds_logical_id(
    deployed: &DeployedResource,
    location: &stack_refactors::RefactorResourceLocation,
) -> bool {
    deployed.resource.logical_id == location.logical_resource_id
        && deployed.status != resource_status::DELETE_COMPLETE
}
