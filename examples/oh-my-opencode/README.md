# Oh-My-OpenCode Configuration Examples

This directory contains example oh-my-opencode configurations for different AI providers.

## What is oh-my-opencode?

Oh-my-opencode is a configuration system for OpenCode that allows you to customize which models are used for different agents and task categories.

## Usage

### Method 1: API (Recommended for sandboxed.sh deployments)

Update the configuration via the API:

```bash
# For Cerebras
curl -X PUT https://YOUR-BACKEND/api/opencode/settings \
  -H "Content-Type: application/json" \
  -d @cerebras.json

# For Z.AI
curl -X PUT https://YOUR-BACKEND/api/opencode/settings \
  -H "Content-Type: application/json" \
  -d @zai.json
```

### Method 2: Direct File Placement

Copy the configuration file to your OpenCode config directory:

```bash
# Default location
cp cerebras.json ~/.config/opencode/oh-my-opencode.json

# Or use OPENCODE_CONFIG_DIR if set
cp cerebras.json $OPENCODE_CONFIG_DIR/oh-my-opencode.json
```

## Available Configurations

### cerebras.json
Configures OpenCode to use Cerebras models:
- **Quick tasks**: llama-3.1-8b (fast, efficient)
- **Deep tasks**: llama-3.3-70b (powerful reasoning)

### zai.json
Configures OpenCode to use Z.AI (GLM) models:
- **Quick tasks**: glm-4-flash (fast responses)
- **Deep tasks**: glm-5 (advanced capabilities)

## Prerequisites

Before using these configurations, ensure you have:

1. **Configured the provider** in sandboxed.sh:
   - Add via Dashboard UI, or
   - Use API: `POST /api/ai/providers`

2. **Valid API key** for the provider

3. **Enabled the provider** for the OpenCode backend

See [PROVIDERS.md](../../PROVIDERS.md) for detailed setup instructions.

## Customization

You can customize these configurations by:

1. **Adding more agents**: See the oh-my-opencode schema
2. **Mixing providers**: Use different providers for different agents
3. **Adjusting categories**: Change model assignments for task categories

Example mixed configuration:

```json
{
  "$schema": "https://raw.githubusercontent.com/code-yeongyu/oh-my-opencode/master/assets/oh-my-opencode.schema.json",
  "agents": {
    "atlas": {
      "model": "anthropic/claude-sonnet-4-5"
    },
    "explore": {
      "model": "cerebras/llama-3.1-8b"
    }
  },
  "categories": {
    "quick": {
      "model": "cerebras/llama-3.1-8b"
    },
    "deep": {
      "model": "zai/glm-5"
    }
  }
}
```

## Verification

After applying a configuration, verify it's loaded:

```bash
curl https://YOUR-BACKEND/api/opencode/settings | jq
```
