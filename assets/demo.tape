# VHS tape for gitpane demo recording
# Run: vhs assets/demo.tape

Output assets/demo.gif
Output assets/screenshot.png

Set Theme "Catppuccin Mocha"
Set FontFamily "JetBrains Mono"
Set FontSize 14
Set Width 1400
Set Height 800
Set Padding 20
Set TypingSpeed 50ms
Set PlaybackSpeed 0.5

# Start gitpane pointing at ~/Code
Type "cd /Users/afromero/Code/gitpane && cargo run --release -- --root ~/Code"
Enter
Sleep 4s

# Let it load repos and show status
Sleep 2s

# Navigate through repos
Type "j"
Sleep 800ms
Type "j"
Sleep 800ms
Type "j"
Sleep 800ms

# Switch to Changes panel
Type@0ms ""
Sleep 200ms
# Press Tab
Tab
Sleep 1s

# Navigate files
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms

# Open diff
Enter
Sleep 1.5s

# Close diff
Escape
Sleep 500ms

# Switch to Graph panel
Tab
Sleep 1s

# Navigate graph
Type "j"
Sleep 400ms
Type "j"
Sleep 400ms
Type "j"
Sleep 400ms

# Open commit detail
Enter
Sleep 1.5s

# Navigate commit files
Type "j"
Sleep 400ms

# Open commit file diff
Enter
Sleep 2s

# Close everything
Escape
Sleep 500ms
Escape
Sleep 500ms

# Sort repos
Type "s"
Sleep 1s

# Back to alphabetical
Type "s"
Sleep 1s

# Final pause to show the full UI
Sleep 2s

# Quit
Type "q"
Sleep 500ms
