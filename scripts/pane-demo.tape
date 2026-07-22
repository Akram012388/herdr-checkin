# Demo of the herdr-checkin status pane. Render from the repo root:
#   cargo build --release && vhs scripts/pane-demo.tape   # writes docs/pane-demo.gif
# scripts/pane-demo-setup.sh seeds a throwaway queue + a fake herdr so no real agents are needed.
# Command reference: https://github.com/charmbracelet/vhs#vhs-command-reference

# --- Output -------------------------------------------------------------
Output docs/pane-demo.gif

# --- Appearance (from the tokyonight template) --------------------------
Set Theme "TokyoNight"
Set FontSize 13
Set FontFamily "JetBrainsMono Nerd Font"
Set Width 1200
Set Height 500
Set Padding 24
Set TypingSpeed 60ms

# --- Off-camera setup ---------------------------------------------------
Hide
Type "source scripts/pane-demo-setup.sh" Enter
Type "clear" Enter
Show
Sleep 500ms

# --- Open the pane: waiters grouped into AWAITING YOU / DONE sections ----
Type "herdr-checkin pane" Enter
Sleep 2.5s

# --- Move the selection with j (display order, across the sections) ------
Type "j"
Sleep 900ms

# --- space: reply inline — route an answer into the agent, then evict ----
Type " "
Sleep 1s
Type "rebase onto main"
Sleep 1.2s
Enter
Sleep 2.2s

# --- Enter: jump to the selected agent and drop it ----------------------
Enter
Sleep 1.8s

# --- d: drop the selected entry without jumping -------------------------
Type "d"
Sleep 1.8s

# --- c: clear-all, with a confirm ---------------------------------------
Type "c"
Sleep 1.5s
Type "y"
Sleep 2s

# --- q: quit back to the shell ------------------------------------------
Type "q"
Sleep 1s
