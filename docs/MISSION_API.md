# Mission API

All endpoints require authentication via `Authorization: Bearer <token>` header.

## Create a Mission

```
POST /api/control/missions
```

**Body** (all optional):
```json
{
  "title": "My Mission",
  "workspace_id": "uuid",
  "agent": "code-reviewer",
  "model_override": "anthropic/claude-sonnet-4-20250514",
  "backend": "opencode"
}
```

`backend` can be `"opencode"`, `"claudecode"`, or `"amp"`. Defaults to `"opencode"` if omitted.

**Response**: `Mission` object (see below).

## Load/Switch to a Mission

```
POST /api/control/missions/:id/load
```

Loads the mission into the active control session. Required before sending messages.

## Send a Message

```
POST /api/control/message
```

**Body**:
```json
{
  "content": "Your message here",
  "agent": "optional-agent-override"
}
```

**Response**:
```json
{
  "id": "uuid",
  "queued": false
}
```

`queued: true` means another message is being processed.

## Cancel Current Execution

```
POST /api/control/cancel
```

Cancels the currently running agent task.

## Cancel a Specific Mission

```
POST /api/control/missions/:id/cancel
```

## Set Mission Status

```
POST /api/control/missions/:id/status
```

**Body**:
```json
{
  "status": "completed"
}
```

Statuses: `pending`, `active`, `completed`, `failed`, `interrupted`.

## Get Mission Events (History)

```
GET /api/control/missions/:id/events?types=user_message,assistant_message&limit=100&offset=0
```

**Query params** (all optional):
- `types`: comma-separated event types to filter
- `limit`: max events to return
- `offset`: pagination offset

**Response**: Array of `StoredEvent`:
```json
[
  {
    "id": 1,
    "mission_id": "uuid",
    "sequence": 1,
    "event_type": "user_message",
    "timestamp": "2025-01-13T10:00:00Z",
    "content": "...",
    "metadata": {}
  }
]
```

## Stream Events (SSE)

```
GET /api/control/stream
```

Server-Sent Events stream for real-time updates. Events have `event:` and `data:` fields.

**Event types**:
- `status` — control state changed (`idle`, `running`, `tool_waiting`)
- `user_message` — user message received
- `assistant_message` — agent response complete
- `thinking` — agent reasoning (streaming)
- `tool_call` — tool invocation
- `tool_result` — tool result
- `error` — error occurred
- `mission_status_changed` — mission status updated

**Example SSE event**:
```
event: assistant_message
data: {"id":"uuid","content":"Done!","success":true,"cost_cents":5,"model":"claude-sonnet-4-20250514"}
```

## Other Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/control/missions` | GET | List missions |
| `/api/control/missions/:id` | GET | Get mission details |
| `/api/control/missions/:id` | DELETE | Delete mission |
| `/api/control/missions/:id/tree` | GET | Get agent tree for mission |
| `/api/control/missions/current` | GET | Get current active mission |
| `/api/control/missions/:id/resume` | POST | Resume interrupted mission |
| `/api/control/tree` | GET | Get live agent tree |
| `/api/control/progress` | GET | Get execution progress |

## Automations

Automations trigger commands based on intervals, webhooks, or agent events.

### List Mission Automations

```
GET /api/control/missions/:id/automations
```

**Response**: Array of `Automation` objects.

### List All Active Automations

```
GET /api/control/automations
```

**Response**: Array of `Automation` objects across all missions.

### Create an Automation

```
POST /api/control/missions/:id/automations
```

**Body**:
```json
{
  "command_source": {"library": {"name": "my-command"}},
  "trigger": {"interval": {"seconds": 300}},
  "variables": {"key": "value"},
  "retry_config": {
    "max_retries": 3,
    "retry_delay_seconds": 60,
    "backoff_multiplier": 2.0
  },
  "start_immediately": false
}
```

**Trigger types**:
- `{"interval": {"seconds": 300}}` — Run every N seconds
- `{"webhook": {"config": {"webhook_id": "optional-uuid"}}}` — Trigger via webhook
- `"agent_finished"` — Trigger after each agent turn completes

**Command sources**:
- `{"library": {"name": "command-name"}}` — Use a library command
- `{"inline": {"command": "echo hello"}}` — Inline shell command

**Response**: `Automation` object.

### Get Automation

```
GET /api/control/automations/:id
```

**Response**: `Automation` object.

### Update Automation

```
PATCH /api/control/automations/:id
```

**Body** (all fields optional):
```json
{
  "command_source": {"library": {"name": "new-command"}},
  "trigger": {"interval": {"seconds": 600}},
  "variables": {"key": "new-value"},
  "retry_config": {"max_retries": 5},
  "active": false
}
```

**Response**: Updated `Automation` object.

### Delete Automation

```
DELETE /api/control/automations/:id
```

**Response**: `204 No Content` on success.

### Get Automation Executions

```
GET /api/control/automations/:id/executions
```

**Response**: Array of `AutomationExecution` objects.

### Get Mission Automation Executions

```
GET /api/control/missions/:id/automation-executions
```

**Response**: Array of `AutomationExecution` objects for all automations on a mission.

## Automation Object

```json
{
  "id": "uuid",
  "mission_id": "uuid",
  "command_source": {"library": {"name": "my-command"}},
  "trigger": {"interval": {"seconds": 300}},
  "variables": {"key": "value"},
  "active": true,
  "created_at": "2025-01-13T10:00:00Z",
  "last_triggered_at": "2025-01-13T10:05:00Z",
  "retry_config": {
    "max_retries": 3,
    "retry_delay_seconds": 60,
    "backoff_multiplier": 2.0
  }
}
```

## AutomationExecution Object

```json
{
  "id": "uuid",
  "automation_id": "uuid",
  "mission_id": "uuid",
  "triggered_at": "2025-01-13T10:05:00Z",
  "trigger_source": "interval",
  "status": "success",
  "webhook_payload": null,
  "started_at": "2025-01-13T10:05:00Z",
  "completed_at": "2025-01-13T10:05:05Z",
  "result_message": "Command completed successfully",
  "retry_count": 0
}
```

**Execution statuses**: `pending`, `running`, `success`, `failed`, `cancelled`, `skipped`.

## Mission Object

```json
{
  "id": "uuid",
  "status": "active",
  "title": "My Mission",
  "workspace_id": "uuid",
  "workspace_name": "my-workspace",
  "agent": "code-reviewer",
  "model_override": null,
  "backend": "opencode",
  "history": [],
  "created_at": "2025-01-13T10:00:00Z",
  "updated_at": "2025-01-13T10:05:00Z"
}
```
