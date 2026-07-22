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
Set Height 460
Set Padding 24
Set TypingSpeed 60ms

# --- Off-camera setup ---------------------------------------------------
Hide
Type "source scripts/pane-demo-setup.sh" Enter
Type "clear" Enter
Show
Sleep 500ms

# --- Open the pane: four agents waiting ---------------------------------
Type "herdr-checkin pane" Enter
Sleep 2.5s

# --- Move the selection with j / k --------------------------------------
Type "j"
Sleep 700ms
Type "j"
Sleep 700ms
Type "k"
Sleep 900ms

# --- Enter: jump to the selected agent and drop it ----------------------
Enter
Sleep 1.8s

# --- d: drop the selected entry without jumping -------------------------
Type "d"
Sleep 1.8s

# --- c: clear-all, with a confirm ---------------------------------------
Type "c"
Sleep 1.8s
Type "y"
Sleep 2s

# --- q: quit back to the shell ------------------------------------------
Type "q"
Sleep 1s
