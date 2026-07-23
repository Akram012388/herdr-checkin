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

# --- Open on Queue: durable waiters grouped into CHECKIN / DONE -------
Type "herdr-checkin pane" Enter
Sleep 2.2s

# --- Tab to the live Agents roster -------------------------------------
Tab
Sleep 2.5s

# --- Move the Agents selection -----------------------------------------
Type "j"
Sleep 900ms

# --- Reply inline from Agents; show the themed compose surface ----------
Type " "
Sleep 1s
Type "looks good, proceed"
Sleep 1.2s
Enter
Sleep 1.8s

# --- Return to Queue ----------------------------------------------------
Tab
Sleep 1.8s

# --- Final successful jump exits the popup; send no later keys ----------
Enter
Sleep 1.5s
