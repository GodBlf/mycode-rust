#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_iterations: usize,
    pub tool_concurrency: usize,
    pub system_prompt: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            tool_concurrency: 4,
            system_prompt: "You are MyCode, a terminal coding agent.".into(),
        }
    }
}
