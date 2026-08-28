# Repository agent guidance

## Subagent delegation

- Use subagents when a task contains at least two concrete, bounded subtasks that can make useful progress independently.
- Prefer delegation for read-heavy codebase exploration, independent design investigations, test-gap analysis, and focused reviews.
- Keep simple tasks, tightly sequential work, and changes likely to edit the same files in the primary agent.
- Use `gpt-5.6-terra` with `medium` reasoning effort for subagents unless the user explicitly requests another model or effort.
- Give each subagent a narrow task, wait for all required results, and have the primary agent verify and synthesize the final outcome.
- Do not delegate merely to increase agent count; the expected parallel benefit must exceed coordination overhead.
