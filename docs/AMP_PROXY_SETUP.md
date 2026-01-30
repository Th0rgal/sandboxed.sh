# Amp with CLIProxyAPI (Experimental)

> **Note**: This feature is experimental. CLIProxyAPI currently supports OpenAI-compatible
> endpoints (`/v1/chat/completions`), but Amp CLI uses its own protocol. For now, use the
> standard Amp service with `AMP_API_KEY`. This document is kept for future reference when
> CLIProxyAPI adds Amp protocol support.

## Standard Amp Setup

For normal usage, simply set your Amp API key:

```bash
# In /etc/sandboxed_sh/sandboxed_sh.env or your .env file
AMP_API_KEY=your-access-token-from-ampcode.com
```

Get your access token from [ampcode.com/settings](https://ampcode.com/settings).

## Future: CLIProxyAPI Integration

When CLIProxyAPI adds support for Amp's protocol, this guide explains the intended setup.

### Why Use CLIProxyAPI?

CLIProxyAPI is a Go-based proxy server that translates CLI applications to
OpenAI/Gemini/Claude-compatible API interfaces. Potential benefits include:

- **Use Your Own OAuth Credentials**: Run Amp with your Claude/OpenAI/Gemini subscriptions
- **No API Keys Required**: OAuth login flow, credentials stored locally
- **Model Fallback**: Automatically route unavailable models to alternatives
- **Multi-Provider Support**: Switch between providers without changing Amp config
- **Load Balancing**: Multiple OAuth accounts per provider with round-robin
- **Cost Control**: Track which requests use local OAuth (free) vs Amp credits

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                      Sandboxed.sh Backend                         │
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────────────┐ │
│  │ Mission     │───►│ Amp Harness │───►│ WorkspaceExec       │ │
│  │ Runner      │    │             │    │ (spawn amp CLI)     │ │
│  └─────────────┘    └─────────────┘    └──────────┬──────────┘ │
└────────────────────────────────────────────────────┼────────────┘
                                                     │
                                                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Amp CLI Process                          │
│  AMP_URL=http://localhost:8317                                  │
└────────────────────────────────────┬────────────────────────────┘
                                     │
                                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                     CLIProxyAPI Server                          │
│                     (http://localhost:8317)                     │
│                                                                 │
│  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────┐ │
│  │ Provider Routes │    │ Model Mapping   │    │ OAuth Store │ │
│  │ /api/provider/* │    │ claude-opus →   │    │ ~/.cli-     │ │
│  │                 │    │ claude-sonnet   │    │ proxy-api/  │ │
│  └────────┬────────┘    └─────────────────┘    └─────────────┘ │
└───────────┼─────────────────────────────────────────────────────┘
            │
    ┌───────┼───────┬───────────────┐
    │       │       │               │
    ▼       ▼       ▼               ▼
┌───────┐ ┌───────┐ ┌───────┐ ┌─────────────┐
│Claude │ │OpenAI │ │Gemini │ │ampcode.com  │
│ API   │ │ API   │ │ API   │ │(fallback)   │
└───────┘ └───────┘ └───────┘ └─────────────┘
```

### Configuration (When Supported)

When CLIProxyAPI adds Amp support, set the proxy URL:

```bash
# Global environment variable
export AMP_URL="http://localhost:8317"
```

Or per-workspace in the dashboard:

1. Go to **Workspaces** in the dashboard
2. Select or create a workspace
3. Add environment variable:
   - Key: `AMP_URL`
   - Value: `http://localhost:8317`

## See Also

- [CLIProxyAPI Repository](https://github.com/router-for-me/CLIProxyAPI)
- [Amp Harness Documentation](./HARNESS_SYSTEM.md#amp-harness)
- [Amp Manual](https://ampcode.com/manual)
