# Herdr Check-in centered-popup demo.
# Render from the repository root:
#   vhs demo/herdr-checkin.tape
#
# Command reference: https://github.com/charmbracelet/vhs#vhs-command-reference

# --- Output -------------------------------------------------------------
Output demo/herdr-checkin.gif

# --- Appearance ---------------------------------------------------------
Set Theme "TokyoNight"
Set FontSize 13
Set FontFamily "JetBrainsMono Nerd Font"   # matches your Ghostty config; Nerd Font = icon glyphs
Set Width 1200
Set Height 700
Set Padding 24
Set TypingSpeed 60ms              # per-keystroke delay; lower = faster

# --- Off-camera setup ---------------------------------------------------
Hide
Type "cargo build --release >/dev/null 2>&1" Enter
Type "source demo/herdr-session-setup.sh" Enter
Type "clear" Enter
Type "herdr" Enter
Sleep 1.5s
Type "git --no-pager diff --color=always -- HANDOFF.md" Enter
Sleep 1.5s
Show

# --- Demo script: summon the real session-modal plugin popup ------------
Sleep 800ms
Ctrl+G
Sleep 4s

# --- Tail padding so the last frame lingers -----------------------------
Sleep 1s

# --- Off-camera teardown of the disposable client/server ----------------
Hide
Type "q"
Sleep 500ms
Ctrl+B
Type "q"
Sleep 800ms
Type "herdr server stop >/dev/null 2>&1" Enter
Sleep 500ms
