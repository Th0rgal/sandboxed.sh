---
name: library-management
description: >
  Manage the Sandboxed.sh library (skills, agents, commands, tools, rules, MCPs) via Library API tools.
  Trigger terms: library, skill, agent, command, tool, rule, MCP, save skill, create skill.
---

# Sandboxed.sh Library Management

The Sandboxed.sh Library is a Git-backed configuration repo that stores reusable skills, agents,
commands, tools, rules, MCP servers, and workspace templates. Use the `library-*` tools to
read and update that repo.

## When to Use
- Creating or updating skills, agents, commands, tools, rules, or MCPs
- Syncing library git state (status/sync/commit/push)
- Updating workspace templates or plugins in the library

## When NOT to Use
- Local file operations unrelated to the library
- Running missions or managing workspace lifecycle

## Tool Map (file name + export)
Tool names follow the pattern `<filename>_<export>`.

### Skills (`library-skills.ts`)
- `library-skills_list_skills`
- `library-skills_get_skill`
- `library-skills_save_skill`
- `library-skills_delete_skill`

### Agents (`library-agents.ts`)
- `library-agents_list_agents`
- `library-agents_get_agent`
- `library-agents_save_agent`
- `library-agents_delete_agent`

### Commands / Tools / Rules (`library-commands.ts`)
- Commands: `library-commands_list_commands`, `library-commands_get_command`, `library-commands_save_command`, `library-commands_delete_command`
- Tools: `library-commands_list_tools`, `library-commands_get_tool`, `library-commands_save_tool`, `library-commands_delete_tool`
- Rules: `library-commands_list_rules`, `library-commands_get_rule`, `library-commands_save_rule`, `library-commands_delete_rule`

### MCPs + Git (`library-git.ts`)
- MCPs: `library-git_get_mcps`, `library-git_save_mcps`
- Git: `library-git_status`, `library-git_sync`, `library-git_commit`, `library-git_push`

## Procedure
1. **List** existing items
2. **Get** current content before editing
3. **Save** the full updated content (frontmatter + body)
4. **Commit** with a clear message
5. **Push** to sync the library remote

## File Formats

### Skill (`skill/<name>/SKILL.md`)
```yaml
---
name: skill-name
description: What this skill does
---
Instructions for using this skill...
```

### Agent (`agent/<name>.md`)
```yaml
---
description: Agent description
mode: primary | subagent
model: provider/model-id
hidden: true | false
color: "#44BA81"
tools:
  "*": false
  "read": true
  "write": true
permission:
  edit: ask | allow | deny
  bash:
    "*": ask
rules:
  - rule-name
---
Agent system prompt...
```

### Command (`command/<name>.md`)
```yaml
---
description: Command description
model: provider/model-id
subtask: true | false
agent: agent-name
---
Command prompt template. Use $ARGUMENTS for user input.
```

### Tool (`tool/<name>.ts`)
```typescript
import { tool } from "@opencode-ai/plugin"

export const my_tool = tool({
  description: "What it does",
  args: { param: tool.schema.string().describe("Param description") },
  async execute(args) {
    return "result"
  },
})
```

### Rule (`rule/<name>.md`)
```yaml
---
description: Rule description
---
Rule instructions applied to agents referencing this rule.
```

### MCPs (`mcp/servers.json`)
```json
{
  "server-name": {
    "type": "local",
    "command": ["npx", "package-name"],
    "env": { "KEY": "value" },
    "enabled": true
  },
  "remote-server": {
    "type": "remote",
    "url": "https://mcp.example.com",
    "headers": { "Authorization": "Bearer token" },
    "enabled": true
  }
}
```

## Guardrails
- Always read before updating to avoid overwrites
- Keep names lowercase (hyphens allowed) and within 1-64 chars
- Use descriptive commit messages
- Check `library-git_status` before pushing
