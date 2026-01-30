# Backend API

All endpoints require authentication via `Authorization: Bearer <token>` header.

## List Backends

```
GET /api/backends
```

**Response**:
```json
[
  {"id": "opencode", "name": "OpenCode"},
  {"id": "claudecode", "name": "Claude Code"}
]
```

## Get Backend

```
GET /api/backends/:id
```

**Response**:
```json
{"id": "opencode", "name": "OpenCode"}
```

## List Backend Agents

```
GET /api/backends/:id/agents
```

**Response**:
```json
[{"id": "Sisyphus", "name": "Sisyphus"}]
```

## Get Backend Config

```
GET /api/backends/:id/config
```

**Response**:
```json
{
  "id": "opencode",
  "name": "OpenCode",
  "enabled": true,
  "settings": {
    "base_url": "http://127.0.0.1:4096",
    "default_agent": "Sisyphus",
    "permissive": true
  }
}
```

For `claudecode`, `settings` includes `api_key_configured` and optional fields like `default_model`.

## Update Backend Config

```
PUT /api/backends/:id/config
```

**Body**:
```json
{
  "enabled": true,
  "settings": {
    "base_url": "http://127.0.0.1:4096",
    "default_agent": "Sisyphus",
    "permissive": true
  }
}
```

Claude Code accepts `api_key` in `settings` to store it securely in the secrets vault.

**Response**:
```json
{
  "ok": true,
  "message": "Backend configuration updated. Restart Sandboxed.sh to apply runtime changes."
}
```
