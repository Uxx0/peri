//! Built-in agent registry
//!
//! Embeds agent definition `.md` files at compile time and provides
//! lookup functions for agent discovery and content resolution.
//!
//! Built-in agents have the lowest priority — project-level `.claude/agents/`
//! definitions with the same `agent_id` always take precedence.

/// Built-in agent definitions, keyed by `agent_id` (filename stem).
///
/// Compile-time embedded via `include_str!`.
pub struct BuiltInAgent {
    /// Agent ID used as `subagent_type` parameter value
    pub agent_id: &'static str,
    /// Full file content (YAML frontmatter + markdown body)
    pub content: &'static str,
}

/// Return all built-in agent definitions.
pub fn list_built_in_agents() -> &'static [BuiltInAgent] {
    &BUILT_IN_AGENTS
}

/// Look up a built-in agent by `agent_id`. Returns `None` if not found.
pub fn get_built_in_agent(agent_id: &str) -> Option<&'static BuiltInAgent> {
    BUILT_IN_AGENTS.iter().find(|a| a.agent_id == agent_id)
}

static BUILT_IN_AGENTS: [BuiltInAgent; 4] = [
    BuiltInAgent {
        agent_id: "explore",
        content: include_str!("built-in/explore.md"),
    },
    BuiltInAgent {
        agent_id: "general-purpose",
        content: include_str!("built-in/general-purpose.md"),
    },
    BuiltInAgent {
        agent_id: "plan",
        content: include_str!("built-in/plan.md"),
    },
    BuiltInAgent {
        agent_id: "verification",
        content: include_str!("built-in/verification.md"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_agent_parser::parse_agent_file;

    #[test]
    fn test_all_built_in_agents_parseable() {
        for agent in list_built_in_agents() {
            let parsed = parse_agent_file(agent.content);
            assert!(
                parsed.is_some(),
                "Built-in agent '{}' failed to parse",
                agent.agent_id
            );
        }
    }

    #[test]
    fn test_built_in_agent_ids_unique() {
        let ids: Vec<&str> = list_built_in_agents().iter().map(|a| a.agent_id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "Built-in agent IDs should be sorted");
        assert_eq!(
            ids.len(),
            {
                let mut deduped = ids.clone();
                deduped.dedup();
                deduped.len()
            },
            "Built-in agent IDs should be unique"
        );
    }

    #[test]
    fn test_get_built_in_agent_found() {
        assert!(get_built_in_agent("explore").is_some());
        assert!(get_built_in_agent("plan").is_some());
        assert!(get_built_in_agent("general-purpose").is_some());
        assert!(get_built_in_agent("verification").is_some());
    }

    #[test]
    fn test_get_built_in_agent_not_found() {
        assert!(get_built_in_agent("nonexistent").is_none());
        assert!(get_built_in_agent("").is_none());
    }

    #[test]
    fn test_explore_agent_disallows_write_tools() {
        let agent = get_built_in_agent("explore").unwrap();
        let parsed = parse_agent_file(agent.content).unwrap();
        let disallowed = parsed.disallowed_tools();
        assert!(
            disallowed.iter().any(|t| t.eq_ignore_ascii_case("Write")),
            "Explore agent should disallow Write"
        );
        assert!(
            disallowed.iter().any(|t| t.eq_ignore_ascii_case("Edit")),
            "Explore agent should disallow Edit"
        );
    }

    #[test]
    fn test_general_purpose_has_all_tools() {
        let agent = get_built_in_agent("general-purpose").unwrap();
        let parsed = parse_agent_file(agent.content).unwrap();
        assert!(
            !parsed.tools().is_empty(),
            "General-purpose agent should have tools configured"
        );
    }
}
