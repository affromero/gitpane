# VHS tape for gitpane demo GIF
# Run: vhs assets/demo.tape

Output assets/demo.gif

Set Theme "Catppuccin Mocha"
Set FontFamily "Fira Code"
Set FontSize 13
Set Width 1600
Set Height 900
Set Padding 24
Set TypingSpeed 20ms
Set PlaybackSpeed 2

# Start gitpane
Type "/Users/afromero/Code/gitpane/target/release/gitpane --root /Users/afromero/Code"
Enter

# Wait for load
Sleep 3s

# Navigate repos
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms

# Tab to Changes, open diff
Tab
Sleep 200ms
Enter
Sleep 800ms

# Close diff with Esc
Escape
Sleep 200ms

# Tab to Graph
Tab
Sleep 200ms

# Navigate graph, open commit detail
Type "j"
Sleep 80ms
Type "j"
Sleep 80ms
Enter
Sleep 800ms

# Navigate commit files, open commit diff
Type "j"
Sleep 100ms
Enter
Sleep 800ms

# Close all with Esc (layered: diff → files → panel nav)
Escape
Sleep 150ms
Escape
Sleep 150ms
Escape
Sleep 150ms

# Sort repos
Type "s"
Sleep 300ms
Type "s"
Sleep 300ms

# Quit
Type "q"
Sleep 100ms
