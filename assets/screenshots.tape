# VHS tape for extracting screenshots
# Run: vhs assets/screenshots.tape

Output assets/frames.png

Set Theme "Catppuccin Mocha"
Set FontFamily "Fira Code"
Set FontSize 13
Set Width 1600
Set Height 900
Set Padding 24
Set TypingSpeed 20ms
Set PlaybackSpeed 1

# Start gitpane
Type "/Users/afromero/Code/gitpane/target/release/gitpane --root /Users/afromero/Code"
Enter

# Wait for full load + status updates
Sleep 6s

# ── SCREENSHOT 1: Three-panel overview ──
# Navigate to gitpane (index 3) which has real dirty files
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms

# Hold for overview screenshot (~frame 195)
Sleep 2s

# ── SCREENSHOT 2: Split diff view ──
# Tab to Changes panel, Enter to open diff
Tab
Sleep 300ms
Enter
Sleep 2s

# Close diff
Escape
Sleep 300ms

# ── SCREENSHOT 3: Commit detail in graph ──
# Tab to Graph panel
Tab
Sleep 300ms
# Select a commit and open detail
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms
Enter
Sleep 500ms
# Open file diff inside commit detail
Type "j"
Sleep 100ms
Enter
Sleep 2s

# Quit
Type "q"
Sleep 100ms
