#!/bin/bash
# Install desktop automation dependencies for Open Agent
# Run this on the production server: bash scripts/install_desktop.sh

set -e

echo "=== Installing desktop automation packages ==="

# Update package list
apt update

# Install core X11 and window manager
echo "Installing Xvfb and i3..."
apt install -y xvfb i3 x11-utils

# Install automation tools
echo "Installing xdotool and screenshot tools..."
apt install -y xdotool scrot imagemagick

# Install Chromium browser
echo "Installing Chromium..."
apt install -y chromium chromium-sandbox || apt install -y chromium-browser

# Install accessibility tools (AT-SPI2)
echo "Installing AT-SPI2 for accessibility tree..."
apt install -y at-spi2-core libatspi2.0-0 python3-gi python3-gi-cairo gir1.2-atspi-2.0

# Install OCR
echo "Installing Tesseract OCR..."
apt install -y tesseract-ocr

# Install fonts for proper rendering
echo "Installing fonts..."
apt install -y fonts-liberation fonts-dejavu-core fonts-noto

# Create i3 config directories for both root and opencode user
# OpenCode service runs with HOME=/var/lib/opencode, so config must exist there
echo "Creating i3 configuration..."
mkdir -p /root/.config/i3
mkdir -p /var/lib/opencode/.config/i3

# Write i3 config to both locations
I3_CONFIG_FILE=/root/.config/i3/config
cat > "$I3_CONFIG_FILE" << 'EOF'
# Open Agent i3 Config - Optimized for LLM Vision & Control
# Key principle: LLM needs to SEE state (URL bar, focus indicator, all windows)

set $mod Mod4

font pango:DejaVu Sans Mono 10

# ============================================================================
# WINDOW DECORATIONS - Minimal but useful for LLM
# ============================================================================

# Thin border shows focus state (colored differently for focused vs unfocused)
default_border pixel 3
default_floating_border pixel 3

# Colors: focused window gets bright orange border, unfocused gets dim gray
# class                 border  backgr. text    indicator child_border
client.focused          #4c7899 #285577 #ffffff #2e9ef4   #ff5500
client.focused_inactive #333333 #5f676a #ffffff #484e50   #333333
client.unfocused        #333333 #222222 #888888 #292d2e   #222222

# Hide edge borders when only one window (still shows focus on multi-window)
hide_edge_borders smart

# ============================================================================
# FOCUS BEHAVIOR - Predictable but functional
# ============================================================================

focus_follows_mouse no
focus_wrapping no
force_display_urgency_hint 0 ms

# DO give focus to new windows - LLM expects to type into launched apps
# (intentionally NOT using no_focus - that prevents typing into new windows)

# Workspace back-and-forth for quick switching
workspace_auto_back_and_forth yes

# ============================================================================
# LAYOUT - Tiling with visible windows
# ============================================================================

# Use split layout (not tabbed) so LLM can see all windows
# When second window opens, split horizontally
default_orientation horizontal

# New windows open to the right of current - predictable positioning
workspace_layout default

# ============================================================================
# CHROMIUM - NOT fullscreen (need to see URL bar!)
# ============================================================================

# Just thin border, don't fullscreen - LLM needs to see URL bar and tabs
for_window [class="Chromium"] border pixel 2
for_window [class="chromium"] border pixel 2
for_window [class="Google-chrome"] border pixel 2

# ============================================================================
# FLOATING WINDOWS - Dialogs centered and predictable
# ============================================================================

# Common dialog types should float and center (file picker, alerts, etc)
for_window [window_role="pop-up"] floating enable, move position center
for_window [window_role="dialog"] floating enable, move position center
for_window [window_role="alert"] floating enable, move position center
for_window [window_type="dialog"] floating enable, move position center
for_window [class="Gcr-prompter"] floating enable, move position center

# All floating windows get centered
for_window [floating] move position center

# ============================================================================
# KEYBINDINGS - For i3-msg programmatic control
# ============================================================================

# Kill window
bindsym $mod+Shift+q kill

# Focus movement
bindsym $mod+h focus left
bindsym $mod+j focus down
bindsym $mod+k focus up
bindsym $mod+l focus right

# Move windows
bindsym $mod+Shift+h move left
bindsym $mod+Shift+j move down
bindsym $mod+Shift+k move up
bindsym $mod+Shift+l move right

# Fullscreen toggle (LLM can use when needed)
bindsym $mod+f fullscreen toggle

# Toggle floating (for dialogs)
bindsym $mod+Shift+space floating toggle

# Focus floating/tiling toggle
bindsym $mod+space focus mode_toggle

# Split direction
bindsym $mod+b split h
bindsym $mod+v split v

# Layout modes
bindsym $mod+s layout stacking
bindsym $mod+w layout tabbed
bindsym $mod+e layout toggle split

# Workspace switching
bindsym $mod+1 workspace 1
bindsym $mod+2 workspace 2
bindsym $mod+3 workspace 3

# Move to workspace
bindsym $mod+Shift+1 move container to workspace 1
bindsym $mod+Shift+2 move container to workspace 2
bindsym $mod+Shift+3 move container to workspace 3

# Exit/reload
bindsym $mod+Shift+e exit
bindsym $mod+Shift+r reload

# ============================================================================
# STARTUP
# ============================================================================

workspace 1 output primary
exec --no-startup-id i3-msg workspace 1

# Disable screensaver
exec --no-startup-id xset s off
exec --no-startup-id xset -dpms
exec --no-startup-id xset s noblank

# Set solid dark background (clean for screenshots, good contrast)
exec --no-startup-id xsetroot -solid "#1a1a2e"
EOF

# Copy to opencode user location
cp "$I3_CONFIG_FILE" /var/lib/opencode/.config/i3/config

echo "i3 configuration written to:"
echo "  - /root/.config/i3/config"
echo "  - /var/lib/opencode/.config/i3/config"

# Add DESKTOP_ENABLED to environment file
echo "Enabling desktop in environment..."
if ! grep -q "DESKTOP_ENABLED" /etc/open_agent/open_agent.env 2>/dev/null; then
    echo "" >> /etc/open_agent/open_agent.env
    echo "# Desktop automation" >> /etc/open_agent/open_agent.env
    echo "DESKTOP_ENABLED=true" >> /etc/open_agent/open_agent.env
    echo "DESKTOP_RESOLUTION=1920x1080" >> /etc/open_agent/open_agent.env
fi

# Create work and screenshots directories
echo "Creating working directories..."
mkdir -p /root/work/screenshots
mkdir -p /root/tools

# Test installation
echo ""
echo "=== Testing installation ==="

echo -n "Xvfb: "
which Xvfb && echo "OK" || echo "MISSING"

echo -n "i3: "
which i3 && echo "OK" || echo "MISSING"

echo -n "xdotool: "
which xdotool && echo "OK" || echo "MISSING"

echo -n "scrot: "
which scrot && echo "OK" || echo "MISSING"

echo -n "chromium: "
(which chromium || which chromium-browser) && echo "OK" || echo "MISSING"

echo -n "tesseract: "
which tesseract && echo "OK" || echo "MISSING"

echo -n "python3 with gi: "
python3 -c "import gi; print('OK')" 2>/dev/null || echo "MISSING"

echo ""
echo "=== Installation complete ==="
echo "Run: systemctl restart open_agent"
echo "To test manually:"
echo "  Xvfb :99 -screen 0 1920x1080x24 &"
echo "  DISPLAY=:99 i3 &"
echo "  DISPLAY=:99 chromium --no-sandbox &"
echo "  DISPLAY=:99 scrot /tmp/test.png"
