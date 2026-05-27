# AI Provider Configuration Guide

This guide explains how to configure and use alternative AI providers with Sandboxed.sh.

## Supported Providers

### Tested & Recommended
- **Anthropic** (Claude models) - OAuth + API key
- **OpenAI** (GPT models) - OAuth + API key
- **Google** (Gemini models) - API key
- **Cerebras** (Llama models) - API key ⚡ Ultra-fast inference
- **Z.AI** (GLM models) - API key
- **DeepInfra** - API key
- **Cohere** - API key
- **Together AI** - API key
- **Perplexity** - API key

### Custom Providers
- **Custom** - Any OpenAI-compatible API

## Quick Start

### 1. Add Provider via Dashboard

1. Navigate to Settings → Providers
2. Click "Add Provider"
3. Select provider type
4. Enter API key or authenticate via OAuth
5. Enable for desired backends (OpenCode, Claude Code)

### 2. Add Provider via API

```bash
curl -X POST https://YOUR-BACKEND/api/ai/providers \
  -H "Content-Type: application/json" \
  -d '{
    "provider_type": "cerebras",
    "name": "Cerebras",
    "api_key": "YOUR_API_KEY",
    "enabled": true,
    "use_for_backends": ["opencode"]
  }'
```

## Provider-Specific Setup

### Cerebras

**Get API Key:** https://cerebras.ai

**Recommended Models:**
- `llama-3.3-70b` - Best quality, fast inference
- `llama-3.1-8b` - Quick tasks, extremely fast

**Example Configuration:**
```bash
curl -X POST https://YOUR-BACKEND/api/ai/providers \
  -H "Content-Type: application/json" \
  -d '{
    "provider_type": "cerebras",
    "name": "Cerebras",
    "api_key": "csk-...",
    "enabled": true,
    "use_for_backends": ["opencode"]
  }'
```

### Z.AI (GLM Models)

**Get API Key:** https://bigmodel.cn

**Recommended Models:**
- `glm-5` - Most capable model
- `glm-4-flash` - Fast, cost-effective
- `glm-4-plus` - Enhanced capabilities

**Example Configuration:**
```bash
curl -X POST https://YOUR-BACKEND/api/ai/providers \
  -H "Content-Type: application/json" \
  -d '{
    "provider_type": "zai",
    "name": "Z.AI",
    "api_key": "...",
    "enabled": true,
    "use_for_backends": ["opencode"]
  }'
```

### DeepInfra

**Get API Key:** https://deepinfra.com

**Recommended Models:**
- `meta-llama/Meta-Llama-3.1-70B-Instruct`
- `Qwen/QwQ-32B-Preview`

### Custom Provider

For any OpenAI-compatible API:

```bash
curl -X POST https://YOUR-BACKEND/api/ai/providers \
  -H "Content-Type: application/json" \
  -d '{
    "provider_type": "custom",
    "name": "My Custom API",
    "api_key": "YOUR_API_KEY",
    "base_url": "https://api.example.com/v1",
    "enabled": true,
    "use_for_backends": ["opencode"],
    "models": [
      {
        "id": "my-model-id",
        "name": "My Model",
        "context_limit": 128000,
        "output_limit": 8000
      }
    ]
  }'
```

## OpenCode Configuration

OpenCode model defaults are configured in `opencode.json` or through per-mission model overrides. Use native `.opencode/agents/*.md` files for agent-specific instructions and model metadata.

## Model Override in Missions

### Via API

```bash
curl -X POST https://YOUR-BACKEND/api/missions \
  -H "Content-Type: application/json" \
  -d '{
    "workspace_id": "...",
    "agent": "atlas",
    "model_override": "cerebras/llama-3.3-70b",
    "backend": "opencode"
  }'
```

### Via Dashboard

1. Create new mission
2. Select agent and backend
3. Use the model override field to specify `provider/model-name`

## Troubleshooting

### Provider Shows "Disconnected"

1. **Check API key is valid:**
   ```bash
   curl -s https://YOUR-BACKEND/api/ai/providers | jq '.[] | select(.id=="cerebras")'
   ```

2. **Test API key directly:**
   ```bash
   # For Cerebras
   curl -X POST https://api.cerebras.ai/v1/chat/completions \
     -H "Authorization: Bearer YOUR_KEY" \
     -H "Content-Type: application/json" \
     -d '{"model": "llama-3.1-8b", "messages": [{"role": "user", "content": "test"}]}'
   ```

3. **Re-add the provider** with correct credentials

### Model Not Found

1. **Verify model name format:** `provider/model-name` (e.g., `cerebras/llama-3.3-70b`)
2. **Check OpenCode config:** verify `opencode.json` contains the expected `model`, `agent`, and `provider` entries.
3. **Ensure provider is enabled for the backend** you're using.

### OAuth Token Expires

OAuth tokens are automatically refreshed. If refresh fails:
1. Re-authenticate via the dashboard
2. Check provider status for error messages

### Rate Limits

Different providers have different rate limits:
- **Cerebras**: Very high limits, ultra-fast
- **Z.AI**: Check your plan limits
- **DeepInfra**: Free tier has lower limits

## Best Practices

### 1. Test with Cheap Models First

Before using expensive models, test your setup with:
- Cerebras `llama-3.1-8b` (very fast, cheap)
- Z.AI `glm-4-flash` (cost-effective)

### 2. Use OpenCode Agent Files for Consistent Configuration

Instead of specifying model overrides per mission, define native OpenCode agents in `.opencode/agents/*.md`.

### 3. Monitor Provider Status

Check provider status regularly:
```bash
curl -s https://YOUR-BACKEND/api/ai/providers | jq '.[] | {id, status, use_for_backends}'
```

### 4. Enable Multiple Providers

Configure multiple providers for redundancy:
- Primary: Cerebras for speed
- Backup: Z.AI for cost-effectiveness
- Fallback: Anthropic for quality

## API Reference

### List Providers
```bash
GET /api/ai/providers
```

### Create Provider
```bash
POST /api/ai/providers
Body: {
  "provider_type": "cerebras",
  "name": "Cerebras",
  "api_key": "...",
  "enabled": true,
  "use_for_backends": ["opencode"]
}
```

### Update Provider
```bash
PUT /api/ai/providers/:id
Body: {
  "enabled": true,
  "use_for_backends": ["opencode", "claudecode"]
}
```

### Delete Provider
```bash
DELETE /api/ai/providers/:id
```

## Examples

### Cost-Optimized Setup

Use Z.AI and Cerebras for most tasks:

```json
{
  "agents": {
    "atlas": { "model": "zai/glm-5" },
    "explore": { "model": "cerebras/llama-3.1-8b" }
  },
  "categories": {
    "quick": { "model": "cerebras/llama-3.1-8b" },
    "deep": { "model": "zai/glm-5" }
  }
}
```

### Speed-Optimized Setup

Cerebras for everything:

```json
{
  "agents": {
    "atlas": { "model": "cerebras/llama-3.3-70b" },
    "explore": { "model": "cerebras/llama-3.1-8b" }
  },
  "categories": {
    "quick": { "model": "cerebras/llama-3.1-8b" },
    "deep": { "model": "cerebras/llama-3.3-70b" }
  }
}
```

### Quality-Focused Setup

Mix of providers based on task:

```json
{
  "agents": {
    "atlas": { "model": "anthropic/claude-sonnet-4-5" },
    "explore": { "model": "cerebras/llama-3.1-8b" },
    "sisyphus": { "model": "anthropic/claude-opus-4-5" }
  },
  "categories": {
    "quick": { "model": "cerebras/llama-3.1-8b" },
    "deep": { "model": "anthropic/claude-opus-4-5" }
  }
}
```

## Getting Help

- **Provider Issues**: Check provider's documentation for API status
- **Configuration Issues**: Review this guide and your OpenCode `opencode.json` / `.opencode/agents` files.
- **Backend Issues**: Check backend logs with `journalctl -u sandboxed-sh-dev -f`

## Provider Links

- **Cerebras**: https://cerebras.ai
- **Z.AI**: https://bigmodel.cn
- **DeepInfra**: https://deepinfra.com
- **Anthropic**: https://console.anthropic.com
- **OpenAI**: https://platform.openai.com
- **Google**: https://ai.google.dev
