//! Busy-input policy evaluation for gateway/adapters.
//!
//! Busy input happens when a source sends another message while missive is
//! already waiting on or subscribed to an active operation for that same source.
//! This module is deliberately a deterministic state transition layer: current
//! gateway and future adapter workers can call it before they start work, then
//! execute the returned actions (queue, cancel local waits/subscriptions,
//! request remote cancellation, or append a steering follow-up) using their own
//! transport/store code.

use missive_core::{
    AgentAlias, BusyInputConfig, BusyInputMode, ContextId, Metadata, MissiveError, Result, TaskId,
};
use missive_store::GatewayJobId;
use serde::Serialize;

/// Disposition assigned to an incoming input after busy-input evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BusyInputDisposition {
    /// No active operation existed, so the input can start immediately.
    StartNow,
    /// The input was queued behind the active operation.
    Queued,
    /// The active operation was marked for interruption and the input was queued
    /// to run after local/remote cancellation settles.
    InterruptQueued,
    /// The input should be appended to the active task/context.
    Steered,
}

/// Stable state of the active in-flight operation after evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BusyOperationState {
    /// The operation is still running normally.
    Running,
    /// An interrupt has been requested; workers should not start another active
    /// operation for this source until cancellation/cleanup finishes.
    Interrupting,
}

/// Source identity whose input stream is being serialized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BusyInputSource {
    /// Source kind such as `adapter`, `cli`, or `webhook`.
    pub source_kind: String,
    /// Stable source id, for example an adapter user/channel composite.
    pub source_id: String,
}

impl BusyInputSource {
    /// Creates a source identity after validating non-empty safe identifiers.
    pub fn new(source_kind: impl Into<String>, source_id: impl Into<String>) -> Result<Self> {
        let source_kind = source_kind.into();
        let source_id = source_id.into();
        validate_source_part("busy input source_kind", &source_kind)?;
        validate_source_part("busy input source_id", &source_id)?;
        Ok(Self {
            source_kind,
            source_id,
        })
    }
}

/// Input from a busy source that needs queue/interrupt/steer handling.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BusyInput {
    /// Caller-provided stable id for this input.
    pub input_id: String,
    /// Source that produced the input.
    pub source: BusyInputSource,
    /// Target/default agent for this input.
    pub agent_alias: AgentAlias,
    /// Optional context requested by the caller.
    pub context_id: Option<ContextId>,
    /// Optional task requested by the caller.
    pub task_id: Option<TaskId>,
    /// Non-secret metadata used by adapters/gateway workers.
    pub metadata: Metadata,
}

impl BusyInput {
    /// Creates a busy input with no explicit context/task linkage.
    pub fn new(
        input_id: impl Into<String>,
        source: BusyInputSource,
        agent_alias: AgentAlias,
    ) -> Result<Self> {
        let input_id = input_id.into();
        validate_source_part("busy input id", &input_id)?;
        Ok(Self {
            input_id,
            source,
            agent_alias,
            context_id: None,
            task_id: None,
            metadata: Metadata::new(),
        })
    }
}

/// Active operation for one source/agent session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InFlightOperation {
    /// Stable caller-provided id for the operation.
    pub operation_id: String,
    /// Target agent currently processing work.
    pub agent_alias: AgentAlias,
    /// Active A2A context id, if known.
    pub context_id: Option<ContextId>,
    /// Active A2A task id, if known.
    pub task_id: Option<TaskId>,
    /// Current local busy state.
    pub state: BusyOperationState,
    /// Whether a foreground/local wait should be cancelled on interrupt.
    pub local_wait_active: bool,
    /// Durable subscription job that should be cancelled on interrupt.
    pub subscription_job_id: Option<GatewayJobId>,
    /// Whether missive should request remote A2A `CancelTask` on interrupt.
    pub remote_task_cancellable: bool,
    /// Whether the active protocol state accepts a follow-up input for steering.
    pub steerable: bool,
}

impl InFlightOperation {
    /// Creates a running in-flight operation.
    pub fn new(operation_id: impl Into<String>, agent_alias: AgentAlias) -> Result<Self> {
        let operation_id = operation_id.into();
        validate_source_part("busy operation id", &operation_id)?;
        Ok(Self {
            operation_id,
            agent_alias,
            context_id: None,
            task_id: None,
            state: BusyOperationState::Running,
            local_wait_active: false,
            subscription_job_id: None,
            remote_task_cancellable: false,
            steerable: false,
        })
    }

    fn steering_target(&self) -> Option<BusySteeringTarget> {
        if !self.steerable {
            return None;
        }
        if self.context_id.is_none() && self.task_id.is_none() {
            return None;
        }
        Some(BusySteeringTarget {
            context_id: self.context_id.clone(),
            task_id: self.task_id.clone(),
        })
    }
}

/// Queued input with deterministic position.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueuedBusyInput {
    /// One-based position within the source queue after evaluation.
    pub position: usize,
    /// Queued input payload/metadata summary.
    pub input: BusyInput,
}

/// Mutable busy-input state for one source.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct BusyInputState {
    /// Currently active operation for the source, if any.
    pub active: Option<InFlightOperation>,
    /// Inputs waiting for later processing.
    pub queued_inputs: Vec<QueuedBusyInput>,
}

impl BusyInputState {
    /// Returns the current queued input count.
    #[must_use]
    pub fn queue_depth(&self) -> usize {
        self.queued_inputs.len()
    }
}

/// Target selected for steer mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BusySteeringTarget {
    /// Context to continue, if available.
    pub context_id: Option<ContextId>,
    /// Task to continue, if available.
    pub task_id: Option<TaskId>,
}

/// Action that a gateway/adapter worker should execute after evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BusyInputAction {
    /// Start the input immediately because there is no active operation.
    StartInput { input_id: String },
    /// Persist/enqueue the input for later processing.
    QueueInput { input_id: String, position: usize },
    /// Mark the active operation as interrupting.
    MarkActiveInterrupting { operation_id: String },
    /// Cancel a local foreground wait associated with the active operation.
    CancelLocalWait { operation_id: String },
    /// Cancel a local gateway subscription job associated with the active task.
    CancelSubscription {
        operation_id: String,
        gateway_job_id: GatewayJobId,
    },
    /// Request remote A2A `CancelTask` for the active operation.
    RequestRemoteTaskCancellation {
        operation_id: String,
        agent_alias: AgentAlias,
        task_id: TaskId,
    },
    /// Append the input as a follow-up to the active context/task.
    AppendFollowUp {
        operation_id: String,
        input_id: String,
        context_id: Option<ContextId>,
        task_id: Option<TaskId>,
    },
    /// Record that steer mode could not be used and a fallback mode was applied.
    UnsupportedSteerFallback {
        input_id: String,
        fallback_mode: BusyInputMode,
        reason: String,
    },
}

/// Result of applying one busy-input policy to one incoming input.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BusyInputOutcome {
    /// Requested mode from the effective profile/source configuration.
    pub requested_mode: BusyInputMode,
    /// Mode that actually handled the input after any steer fallback.
    pub mode_used: BusyInputMode,
    /// Whether `unsupported_steer_fallback` was used.
    pub fallback_used: bool,
    /// Final disposition for the incoming input.
    pub disposition: BusyInputDisposition,
    /// Deterministic actions for the caller to execute.
    pub actions: Vec<BusyInputAction>,
    /// State after the action is accepted.
    pub state: BusyInputState,
}

/// Applies a busy-input policy to the current source state.
pub fn apply_busy_input(
    policy: &BusyInputConfig,
    state: BusyInputState,
    input: BusyInput,
) -> Result<BusyInputOutcome> {
    apply_mode(
        policy,
        state,
        input,
        policy.mode,
        policy.mode,
        false,
        Vec::new(),
    )
}

fn apply_mode(
    policy: &BusyInputConfig,
    state: BusyInputState,
    input: BusyInput,
    requested_mode: BusyInputMode,
    mode: BusyInputMode,
    fallback_used: bool,
    prefix_actions: Vec<BusyInputAction>,
) -> Result<BusyInputOutcome> {
    if state.active.is_none() {
        let mut actions = prefix_actions;
        actions.push(BusyInputAction::StartInput {
            input_id: input.input_id.clone(),
        });
        return Ok(BusyInputOutcome {
            requested_mode,
            mode_used: mode,
            fallback_used,
            disposition: BusyInputDisposition::StartNow,
            actions,
            state,
        });
    }

    match mode {
        BusyInputMode::Queue => apply_queue(
            policy,
            state,
            input,
            requested_mode,
            fallback_used,
            prefix_actions,
        ),
        BusyInputMode::Interrupt => apply_interrupt(
            policy,
            state,
            input,
            requested_mode,
            fallback_used,
            prefix_actions,
        ),
        BusyInputMode::Steer => apply_steer(policy, state, input, requested_mode, prefix_actions),
    }
}

fn apply_queue(
    policy: &BusyInputConfig,
    mut state: BusyInputState,
    input: BusyInput,
    requested_mode: BusyInputMode,
    fallback_used: bool,
    mut actions: Vec<BusyInputAction>,
) -> Result<BusyInputOutcome> {
    let position = push_queue(policy, &mut state, input)?;
    let input_id = state.queued_inputs[position - 1].input.input_id.clone();
    actions.push(BusyInputAction::QueueInput { input_id, position });
    Ok(BusyInputOutcome {
        requested_mode,
        mode_used: BusyInputMode::Queue,
        fallback_used,
        disposition: BusyInputDisposition::Queued,
        actions,
        state,
    })
}

fn apply_interrupt(
    policy: &BusyInputConfig,
    mut state: BusyInputState,
    input: BusyInput,
    requested_mode: BusyInputMode,
    fallback_used: bool,
    mut actions: Vec<BusyInputAction>,
) -> Result<BusyInputOutcome> {
    let active = state
        .active
        .as_mut()
        .expect("active operation presence checked before interrupt");
    active.state = BusyOperationState::Interrupting;
    actions.push(BusyInputAction::MarkActiveInterrupting {
        operation_id: active.operation_id.clone(),
    });
    if active.local_wait_active {
        actions.push(BusyInputAction::CancelLocalWait {
            operation_id: active.operation_id.clone(),
        });
    }
    if let Some(gateway_job_id) = &active.subscription_job_id {
        actions.push(BusyInputAction::CancelSubscription {
            operation_id: active.operation_id.clone(),
            gateway_job_id: gateway_job_id.clone(),
        });
    }
    if policy.interrupt_remote_cancel && active.remote_task_cancellable {
        if let Some(task_id) = &active.task_id {
            actions.push(BusyInputAction::RequestRemoteTaskCancellation {
                operation_id: active.operation_id.clone(),
                agent_alias: active.agent_alias.clone(),
                task_id: task_id.clone(),
            });
        }
    }

    let position = push_queue(policy, &mut state, input)?;
    let input_id = state.queued_inputs[position - 1].input.input_id.clone();
    actions.push(BusyInputAction::QueueInput { input_id, position });

    Ok(BusyInputOutcome {
        requested_mode,
        mode_used: BusyInputMode::Interrupt,
        fallback_used,
        disposition: BusyInputDisposition::InterruptQueued,
        actions,
        state,
    })
}

fn apply_steer(
    policy: &BusyInputConfig,
    state: BusyInputState,
    input: BusyInput,
    requested_mode: BusyInputMode,
    mut actions: Vec<BusyInputAction>,
) -> Result<BusyInputOutcome> {
    let active = state
        .active
        .as_ref()
        .expect("active operation presence checked before steer");
    if let Some(target) = active.steering_target() {
        actions.push(BusyInputAction::AppendFollowUp {
            operation_id: active.operation_id.clone(),
            input_id: input.input_id.clone(),
            context_id: target.context_id,
            task_id: target.task_id,
        });
        return Ok(BusyInputOutcome {
            requested_mode,
            mode_used: BusyInputMode::Steer,
            fallback_used: false,
            disposition: BusyInputDisposition::Steered,
            actions,
            state,
        });
    }

    let reason = if active.steerable {
        "active operation is marked steerable but has no context_id or task_id".to_owned()
    } else {
        "active operation does not accept follow-up input in its current protocol state".to_owned()
    };
    let fallback = policy.unsupported_steer_fallback;
    actions.push(BusyInputAction::UnsupportedSteerFallback {
        input_id: input.input_id.clone(),
        fallback_mode: fallback,
        reason,
    });
    apply_mode(
        policy,
        state,
        input,
        requested_mode,
        fallback,
        true,
        actions,
    )
}

fn push_queue(
    policy: &BusyInputConfig,
    state: &mut BusyInputState,
    input: BusyInput,
) -> Result<usize> {
    let max_depth = usize::from(policy.max_queue_depth);
    if state.queued_inputs.len() >= max_depth {
        return Err(MissiveError::orchestration(format!(
            "busy input queue for source {}:{} is full at {} item(s)",
            input.source.source_kind, input.source.source_id, max_depth
        ))
        .with_help(
            "Increase gateway.busy_input.max_queue_depth or configure the source for interrupt/steer when safe.",
        ));
    }
    let position = state.queued_inputs.len() + 1;
    state
        .queued_inputs
        .push(QueuedBusyInput { position, input });
    Ok(position)
}

fn validate_source_part(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MissiveError::validation(format!("{label} cannot be empty")));
    }
    if value.len() > 256 {
        return Err(MissiveError::validation(format!(
            "{label} must be at most 256 bytes"
        )));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(MissiveError::validation(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use missive_core::{AgentAlias, BusyInputConfig, BusyInputMode, ContextId, TaskId};
    use missive_store::GatewayJobId;

    use super::*;

    #[test]
    fn queue_mode_preserves_active_operation_and_enqueues_input() {
        let policy = BusyInputConfig {
            mode: BusyInputMode::Queue,
            max_queue_depth: 2,
            ..BusyInputConfig::default()
        };
        let active = active_operation();
        let state = BusyInputState {
            active: Some(active.clone()),
            queued_inputs: Vec::new(),
        };

        let outcome = apply_busy_input(&policy, state, input("input-1")).expect("queue outcome");

        assert_eq!(outcome.disposition, BusyInputDisposition::Queued);
        assert_eq!(outcome.mode_used, BusyInputMode::Queue);
        assert_eq!(outcome.state.active.as_ref(), Some(&active));
        assert_eq!(outcome.state.queue_depth(), 1);
        assert_eq!(
            outcome.actions,
            vec![BusyInputAction::QueueInput {
                input_id: "input-1".to_owned(),
                position: 1,
            }]
        );
    }

    #[test]
    fn interrupt_mode_cancels_local_state_and_requests_remote_cancel_when_possible() {
        let policy = BusyInputConfig {
            mode: BusyInputMode::Interrupt,
            interrupt_remote_cancel: true,
            max_queue_depth: 4,
            ..BusyInputConfig::default()
        };
        let mut active = active_operation();
        active.local_wait_active = true;
        active.subscription_job_id = Some(GatewayJobId::new("subscription-echo-task-1").unwrap());
        active.remote_task_cancellable = true;
        active.task_id = Some(TaskId::new("task-1").unwrap());
        let state = BusyInputState {
            active: Some(active),
            queued_inputs: Vec::new(),
        };

        let outcome = apply_busy_input(&policy, state, input("input-2")).expect("interrupt");

        assert_eq!(outcome.disposition, BusyInputDisposition::InterruptQueued);
        let active_after = outcome.state.active.as_ref().expect("active after");
        assert_eq!(active_after.state, BusyOperationState::Interrupting);
        assert_eq!(outcome.state.queue_depth(), 1);
        assert!(outcome.actions.contains(&BusyInputAction::CancelLocalWait {
            operation_id: "op-1".to_owned(),
        }));
        assert!(outcome.actions.iter().any(|action| matches!(
            action,
            BusyInputAction::CancelSubscription { gateway_job_id, .. }
                if gateway_job_id.as_str() == "subscription-echo-task-1"
        )));
        assert!(outcome.actions.iter().any(|action| matches!(
            action,
            BusyInputAction::RequestRemoteTaskCancellation { task_id, .. }
                if task_id.as_str() == "task-1"
        )));
    }

    #[test]
    fn steer_mode_appends_follow_up_without_queuing_or_interrupting() {
        let policy = BusyInputConfig {
            mode: BusyInputMode::Steer,
            unsupported_steer_fallback: BusyInputMode::Queue,
            ..BusyInputConfig::default()
        };
        let mut active = active_operation();
        active.steerable = true;
        active.context_id = Some(ContextId::new("ctx-1").unwrap());
        active.task_id = Some(TaskId::new("task-1").unwrap());
        let state = BusyInputState {
            active: Some(active),
            queued_inputs: Vec::new(),
        };

        let outcome = apply_busy_input(&policy, state, input("input-3")).expect("steer");

        assert_eq!(outcome.disposition, BusyInputDisposition::Steered);
        assert_eq!(outcome.mode_used, BusyInputMode::Steer);
        assert_eq!(outcome.state.queue_depth(), 0);
        assert!(outcome.actions.iter().any(|action| matches!(
            action,
            BusyInputAction::AppendFollowUp { input_id, context_id, task_id, .. }
                if input_id == "input-3"
                    && context_id.as_ref().is_some_and(|value| value.as_str() == "ctx-1")
                    && task_id.as_ref().is_some_and(|value| value.as_str() == "task-1")
        )));
    }

    #[test]
    fn unsupported_steer_uses_configured_queue_fallback() {
        let policy = BusyInputConfig {
            mode: BusyInputMode::Steer,
            unsupported_steer_fallback: BusyInputMode::Queue,
            max_queue_depth: 4,
            ..BusyInputConfig::default()
        };
        let state = BusyInputState {
            active: Some(active_operation()),
            queued_inputs: Vec::new(),
        };

        let outcome = apply_busy_input(&policy, state, input("input-4")).expect("fallback");

        assert_eq!(outcome.requested_mode, BusyInputMode::Steer);
        assert_eq!(outcome.mode_used, BusyInputMode::Queue);
        assert!(outcome.fallback_used);
        assert_eq!(outcome.disposition, BusyInputDisposition::Queued);
        assert_eq!(outcome.state.queue_depth(), 1);
        assert!(matches!(
            outcome.actions.first(),
            Some(BusyInputAction::UnsupportedSteerFallback {
                fallback_mode: BusyInputMode::Queue,
                ..
            })
        ));
    }

    #[test]
    fn unsupported_steer_can_fallback_to_interrupt() {
        let policy = BusyInputConfig {
            mode: BusyInputMode::Steer,
            unsupported_steer_fallback: BusyInputMode::Interrupt,
            interrupt_remote_cancel: false,
            max_queue_depth: 4,
        };
        let state = BusyInputState {
            active: Some(active_operation()),
            queued_inputs: Vec::new(),
        };

        let outcome = apply_busy_input(&policy, state, input("input-5")).expect("fallback");

        assert_eq!(outcome.mode_used, BusyInputMode::Interrupt);
        assert!(outcome.fallback_used);
        assert_eq!(
            outcome.state.active.as_ref().map(|active| active.state),
            Some(BusyOperationState::Interrupting)
        );
    }

    #[test]
    fn queue_depth_limit_is_enforced() {
        let policy = BusyInputConfig {
            mode: BusyInputMode::Queue,
            max_queue_depth: 1,
            ..BusyInputConfig::default()
        };
        let state = BusyInputState {
            active: Some(active_operation()),
            queued_inputs: vec![QueuedBusyInput {
                position: 1,
                input: input("already-queued"),
            }],
        };

        let error = apply_busy_input(&policy, state, input("input-6")).expect_err("full queue");

        assert_eq!(error.category(), missive_core::ErrorCategory::Orchestration);
        assert!(error.to_string().contains("queue"));
    }

    #[test]
    fn input_starts_immediately_when_no_operation_is_active() {
        let policy = BusyInputConfig::default();
        let state = BusyInputState::default();

        let outcome = apply_busy_input(&policy, state, input("input-7")).expect("start now");

        assert_eq!(outcome.disposition, BusyInputDisposition::StartNow);
        assert_eq!(
            outcome.actions,
            vec![BusyInputAction::StartInput {
                input_id: "input-7".to_owned()
            }]
        );
        assert_eq!(outcome.state.queue_depth(), 0);
    }

    fn active_operation() -> InFlightOperation {
        InFlightOperation::new("op-1", agent()).expect("active operation")
    }

    fn input(input_id: &str) -> BusyInput {
        BusyInput::new(input_id, source(), agent()).expect("busy input")
    }

    fn source() -> BusyInputSource {
        BusyInputSource::new("adapter", "stdio/user-1").expect("source")
    }

    fn agent() -> AgentAlias {
        AgentAlias::new("echo").expect("agent")
    }
}
