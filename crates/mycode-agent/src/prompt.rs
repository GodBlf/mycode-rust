use mycode_core::config::PermissionMode;

pub(crate) const PLAN_MODE_REMINDER: &str = "Plan mode is active. Do not create, modify, or delete any file except the selected plan file. Read-only investigation is allowed; propose changes in the plan instead of applying them.";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptSections {
    pub base: String,
    pub instructions: String,
    pub auto_memory: String,
    pub available_skills: Vec<String>,
    pub active_skill: Option<String>,
    pub permission_mode: PermissionMode,
}

impl PromptSections {
    pub fn compose(&self) -> String {
        let mut sections = Vec::new();
        if !self.base.trim().is_empty() {
            sections.push(self.base.trim().to_string());
        }
        if !self.instructions.trim().is_empty() {
            sections.push(format!(
                "## Project instructions\n\n{}",
                self.instructions.trim()
            ));
        }
        if !self.auto_memory.trim().is_empty() {
            sections.push(format!("## Memory\n\n{}", self.auto_memory.trim()));
        }
        if !self.available_skills.is_empty() {
            sections.push(format!(
                "## Available skills\n\n{}",
                self.available_skills.join("\n")
            ));
        }
        if let Some(active_skill) = self.active_skill.as_deref().filter(|skill| !skill.trim().is_empty()) {
            sections.push(format!(
                "## Active skill\n\n{}",
                active_skill.trim()
            ));
        }
        if self.permission_mode == PermissionMode::Plan {
            sections.push(format!("## Mode reminder\n\n{PLAN_MODE_REMINDER}"));
        }
        sections.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_sections_compose_in_a_stable_order() {
        let prompt = PromptSections {
            base: "base".into(),
            instructions: "instructions".into(),
            auto_memory: "memory".into(),
            available_skills: vec!["- commit: create commits".into()],
            active_skill: Some("active skill".into()),
            permission_mode: PermissionMode::Plan,
        }
        .compose();

        let mut expected: Vec<String> = [
            "base",
            "## Project instructions\n\ninstructions",
            "## Memory\n\nmemory",
            "## Available skills\n\n- commit: create commits",
            "## Active skill\n\nactive skill",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        expected.push(format!("## Mode reminder\n\n{PLAN_MODE_REMINDER}"));
        let expected = expected.join("\n\n");
        assert_eq!(prompt, expected);
    }

    #[test]
    fn plan_reminder_is_mode_specific() {
        let sections = PromptSections {
            base: "base".into(),
            permission_mode: PermissionMode::AcceptEdits,
            ..PromptSections::default()
        };
        assert_eq!(sections.compose(), "base");
    }
}
