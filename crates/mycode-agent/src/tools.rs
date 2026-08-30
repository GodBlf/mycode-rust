use mycode_tools::context::ToolContext;
use mycode_tools::permission::{PermissionDecision, PermissionDecisionEffect};
use mycode_tools::runtime::ToolExecutor;
use mycode_tools::tool::ToolResult;
use tokio::sync::mpsc;

use crate::events::AgentEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionResponse {
    pub(crate) request_id: String,
    pub(crate) allowed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingToolCall {
    pub(crate) tool_id: String,
    pub(crate) tool_name: String,
    pub(crate) arguments: serde_json::Value,
}

pub(crate) struct PlannedToolCall {
    pub(crate) tool_call: PendingToolCall,
    pub(crate) denial_reason: Option<String>,
    pub(crate) authorization: Option<PermissionDecision>,
}

pub(crate) async fn execute_plan(
    executor: &ToolExecutor,
    context: &ToolContext,
    index: usize,
    plan: PlannedToolCall,
) -> (usize, ToolResult) {
    let result = if let Some(reason) = plan.denial_reason {
        ToolResult::error(reason)
    } else {
        let authorization = plan
            .authorization
            .expect("authorized tool calls carry a Permission Decision");
        executor
            .execute_authorized(
                context,
                &plan.tool_call.tool_name,
                plan.tool_call.arguments,
                authorization,
            )
            .await
    };
    (index, result)
}

pub(crate) async fn authorize_tool(
    executor: &ToolExecutor,
    context: &ToolContext,
    tool_call: &PendingToolCall,
    event_sender: &mpsc::Sender<AgentEvent>,
    permission_receiver: &mut mpsc::Receiver<PermissionResponse>,
) -> Option<PlannedToolCall> {
    if executor.registry().get(&tool_call.tool_name).is_none() {
        return Some(PlannedToolCall {
            tool_call: tool_call.clone(),
            denial_reason: Some(format!("unknown tool: {}", tool_call.tool_name)),
            authorization: None,
        });
    }
    let decision = executor.permission_decision(&tool_call.tool_name, &tool_call.arguments);
    let Some(decision) = decision else {
        return Some(PlannedToolCall {
            tool_call: tool_call.clone(),
            denial_reason: Some(format!("unknown tool: {}", tool_call.tool_name)),
            authorization: None,
        });
    };
    let (denial_reason, authorization) = match decision.effect {
        PermissionDecisionEffect::Allow => (None, Some(decision.clone())),
        PermissionDecisionEffect::Deny => (Some(format!("tool denied: {}", decision.reason)), None),
        PermissionDecisionEffect::Ask => {
            if event_sender
                .send(AgentEvent::PermissionRequest {
                    request_id: tool_call.tool_id.clone(),
                    tool_name: tool_call.tool_name.clone(),
                    arguments: tool_call.arguments.clone(),
                    reason: decision.reason.clone(),
                })
                .await
                .is_err()
            {
                return Some(PlannedToolCall {
                    tool_call: tool_call.clone(),
                    denial_reason: Some("agent event stream closed".into()),
                    authorization: None,
                });
            }
            let allowed = loop {
                tokio::select! {
                    _ = context.cancellation().cancelled() => return None,
                    response = permission_receiver.recv() => match response {
                        Some(response) if response.request_id == tool_call.tool_id => {
                            break response.allowed;
                        }
                        Some(_) => continue,
                        None => break false,
                    }
                }
            };
            let _ = event_sender
                .send(AgentEvent::PermissionDecision {
                    request_id: tool_call.tool_id.clone(),
                    allowed,
                })
                .await;
            if allowed {
                (
                    None,
                    Some(PermissionDecision {
                        effect: PermissionDecisionEffect::Allow,
                        reason: "user confirmation granted".into(),
                    }),
                )
            } else {
                (
                    Some(format!("tool denied by user: {}", tool_call.tool_name)),
                    None,
                )
            }
        }
    };

    Some(PlannedToolCall {
        tool_call: tool_call.clone(),
        denial_reason,
        authorization,
    })
}
