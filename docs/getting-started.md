# Getting Started with OpenAgent

This guide will walk you through setting up and using OpenAgent for the first time, from configuring your backend connection to creating your first AI-powered mission.

## What is OpenAgent?

OpenAgent is a powerful AI agent orchestration platform that manages multiple AI coding assistants (OpenCode, Claude Code, Amp) through a unified dashboard. It provides:

- **Git-backed configuration library** - Store and version control your skills, agents, commands, and tools
- **Multiple workspace support** - Isolated containers for different projects
- **Config profiles** - Manage different configurations for each AI harness
- **Mission control** - Queue and manage AI tasks with real-time monitoring

## Prerequisites

Before starting, you need:

1. **OpenAgent Backend** running (locally or remote)
   - Local: `http://localhost:3000` (default)
   - Remote: Your server URL (e.g., `https://agent-backend.example.com`)

2. **Dashboard Access**
   - Web: `http://localhost:3001` (local development)
   - iOS app: Available from the App Store

## Step 1: Initial Dashboard View

When you first access the OpenAgent dashboard, you'll see the global monitor overview:

![Initial Dashboard](images/getting-started/01-dashboard-initial.webp)

The dashboard shows:
- **Navigation sidebar** - Access different sections (Overview, Mission, Workspaces, Library, Settings)
- **Global Monitor** - Real-time view of running missions
- **Recent Missions** - History of completed tasks
- **Agent Status** - Current state (Ready, Busy, etc.)

## Step 2: Configure Backend Connection

### 2.1 Open Settings Menu

Click the **Settings** button in the left sidebar to expand the settings menu:

![Settings Menu](images/getting-started/02-settings-menu.webp)

You'll see several settings categories:
- **System** - Server connection and components
- **Backends** - AI harness configurations
- **Providers** - API keys for AI models
- **Data** - Library and backup management
- **Monitoring** - Logging and diagnostics
- **Secrets** - Encrypted credential storage

### 2.2 Access System Settings

Click on **System** to configure your backend connection:

![System Settings](images/getting-started/03-system-settings-initial.webp)

The System page shows:
- **API URL** - Backend endpoint (currently connected status)
- **System Components** - Installed harnesses and versions
  - Sandboxed.sh (v0.5.1)
  - OpenCode (with update notifications)
  - Claude Code (with update notifications)
  - Amp
  - oh-my-opencode plugin

### 2.3 Change Backend URL

If you need to connect to a different backend (e.g., a remote server):

1. Click on the **API URL** field
2. Enter your backend URL (e.g., `https://agent-backend-dev.thomas.md`)

![Backend URL Changed](images/getting-started/04-backend-url-changed.webp)

3. Click **Save Changes**

![Settings Saved](images/getting-started/05-settings-saved.webp)

The dashboard will reconnect to the new backend and display a success message.

## Step 3: Configure Your Library

The OpenAgent Library is a Git-backed repository that stores all your reusable configurations:

- **Skills** - Specialized knowledge and procedures
- **Agents** - AI assistant configurations
- **Commands** - Quick task templates
- **Tools** - Custom functions and utilities
- **Rules** - Behavioral guidelines
- **MCPs** - Model Context Protocol servers
- **Workspace Templates** - Preconfigured environments

### 3.1 Access Library Settings

1. Click **Settings** > **Data** to access library configuration:

![Data Settings](images/getting-started/08-data-settings.webp)

The Data page shows:
- **Library Remote** - Git repository URL (SSH or HTTPS)
- **Backup & Restore** - Export/import your complete configuration

### 3.2 Change Library Repository (Optional)

The default library uses the OpenAgent template repository. To use your own:

1. Create a new Git repository (GitHub, GitLab, etc.)
2. Click on the **Library Remote** field:

![Library Remote Edit](images/getting-started/09-library-remote-edit.webp)

3. Enter your repository URL:
   - SSH: `git@github.com:your-username/your-library.git`
   - HTTPS: `https://github.com/your-username/your-library.git`

4. Click **Save**

**Note**: The backend will clone your repository on first use. If it's empty, consider forking the [sandboxed.sh-library-template](https://github.com/Th0rgal/sandboxed-library-template) as a starting point.

### 3.3 Sync Library

After changing the library remote, click **Sync** in the Library > Configs section to pull the latest changes.

## Step 4: Explore Library Skills

### 4.1 View Installed Skills

Click **Library** > **Skills** to see all available skills:

![Skills List](images/getting-started/10-skills-list.webp)

The default library includes several useful skills:
- **agent-browser** - Web automation and testing
- **bitwarden-secrets** - Secure credential management
- **github-cli** - GitHub operations (PRs, issues)
- **library-management** - Library API tools
- **sandboxed.sh-dev** - OpenAgent development guide

### 4.2 Edit a Skill

Click on any skill to view and edit its content:

![Skill Editor](images/getting-started/11-skill-editor.webp)

The skill editor shows:
- **Frontmatter** - Metadata (description, license, compatibility)
- **Body Content** - Markdown documentation with instructions
- **Files** - Additional reference files within the skill folder

You can modify the content directly and click **Save** to update the library.

## Step 5: Configure AI Harness Settings

OpenAgent supports multiple AI coding harnesses (OpenCode, Claude Code, Amp). Each harness can have different configurations stored in profiles.

### 5.1 Access Config Editor

Click **Library** > **Configs** to access the file-based config editor:

![Config Editor](images/getting-started/07-config-editor.webp)

The config editor shows:
- **Harness tabs** - Switch between OpenCode, Claude Code, Amp, OpenAgent
- **Profile selector** - Choose or create config profiles (default, custom)
- **Git status** - Branch and sync state
- **File browser** - Navigate config files (`.opencode/`, `.claudecode/`, etc.)
- **JSON editor** - Edit configuration directly

### 5.2 Edit Configuration Files

For OpenCode, you'll typically edit:

1. **settings.json** - Main configuration:
```json
{
  "agents": {
    "Sisyphus": {
      "model": "anthropic/claude-sonnet-4-20250514"
    }
  }
}
```

2. **oh-my-opencode.json** - Plugin configuration:
```json
{
  "providers": {
    "anthropic": {
      "apiKey": "<your-api-key>"
    }
  }
}
```

After editing, click **Save** to commit changes to the library.

## Step 6: Create Your First Mission

### 6.1 Open Mission Control

Click **Mission** in the sidebar to access the control interface:

![Mission Control Empty](images/getting-started/12-mission-control-empty.webp)

The Mission Control page shows:
- **Mission selector** - Switch between active missions
- **New Mission** - Create a new task
- **Status indicators** - Thinking, Idle, Queue
- **Message area** - Chat interface with the agent

### 6.2 Create a New Mission

Click **+ New Mission** to open the creation dialog:

![New Mission Dialog](images/getting-started/13-new-mission-dialog.webp)

Configure your mission:

1. **Workspace** - Select where the mission runs:
   - **Host (default)** - Direct access to your system
   - **Isolated containers** - Sandboxed environments for projects

2. **Agent** - Choose the AI model:
   - **Smart Mode** - Balanced performance (recommended)
   - **Rush Mode** - Faster responses
   - **Custom agents** - Your configured agents (Sisyphus, Prometheus, etc.)

3. **Config Profile** - Override settings (optional):
   - **Default** - Use workspace configuration
   - **Custom profiles** - Specific model/mode overrides

4. Click **Create here** to start the mission in the current tab, or **New Tab** to open in a new window

### 6.3 Interact with Your Agent

Once created, you can:

- **Send messages** - Type tasks in the message box
- **Upload files** - Drag and drop or paste files directly
- **Monitor progress** - Watch real-time output and thinking process
- **Queue messages** - Send multiple tasks while the agent is busy

Example first message:
```
Please help me understand the structure of this codebase and create a README documenting the main components.
```

## Next Steps

Now that you're set up, explore these features:

### Workspace Management
- Create isolated workspaces for different projects
- Configure workspace templates with pre-installed tools
- Use containers for safe experimentation

### Library Customization
- Create custom skills for your workflows
- Add project-specific commands
- Define reusable tools and functions
- Set up behavioral rules for agents

### Advanced Configuration
- Configure multiple config profiles for different scenarios
- Set up MCP servers for extended functionality
- Create custom agents with specific capabilities
- Configure AI provider API keys securely

### Monitoring and Debugging
- View mission logs and outputs
- Monitor resource usage
- Track agent thinking process
- Review mission history

## Troubleshooting

### Backend Connection Issues

**Problem**: "Failed to connect to backend"

**Solutions**:
1. Verify the backend is running: `curl http://localhost:3000/api/health`
2. Check the API URL in Settings > System
3. Ensure no firewall is blocking the connection
4. For remote backends, verify HTTPS/SSL certificates

### Library Sync Errors

**Problem**: "Failed to sync library"

**Solutions**:
1. Check Git credentials (SSH keys or HTTPS tokens)
2. Verify repository permissions
3. Use `Sync` button to force refresh
4. Check Settings > Data > Library Remote URL

### Mission Creation Fails

**Problem**: "Failed to create mission"

**Solutions**:
1. Verify AI provider API keys (Settings > Providers)
2. Check workspace exists and is accessible
3. Ensure backend has sufficient resources
4. Review backend logs for error details

### Configuration Not Applied

**Problem**: Changes to config files don't take effect

**Solutions**:
1. Ensure you clicked **Save** in the config editor
2. Check Git status shows "Clean" (changes committed)
3. Restart the mission or create a new one
4. Verify the correct config profile is selected

## Getting Help

- **Documentation**: [https://github.com/Th0rgal/sandboxed-sh/tree/master/docs](https://github.com/Th0rgal/sandboxed-sh/tree/master/docs)
- **Issues**: [https://github.com/Th0rgal/sandboxed-sh/issues](https://github.com/Th0rgal/sandboxed-sh/issues)
- **Discussions**: [https://github.com/Th0rgal/sandboxed-sh/discussions](https://github.com/Th0rgal/sandboxed-sh/discussions)

## Summary

You've learned how to:
- ✅ Configure backend connection
- ✅ Set up your library repository
- ✅ Browse and edit skills
- ✅ Configure AI harness settings
- ✅ Create and manage missions

Start experimenting with your first mission and explore the powerful features OpenAgent offers for AI-powered development workflows!
