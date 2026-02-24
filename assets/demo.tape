# VHS tape for gitpane demo GIF
# Run: vhs assets/demo.tape

Output assets/demo.gif

Set Theme "Catppuccin Mocha"
Set FontFamily "Fira Code"
Set FontSize 13
Set Width 1600
Set Height 900
Set Padding 24
Set TypingSpeed 30ms
Set PlaybackSpeed 2

# Start gitpane
Type "/Users/afromero/Code/gitpane/target/release/gitpane --root /Users/afromero/Code"
Enter

# Wait for load
Sleep 5s

# Navigate repos
Type "j"
Sleep 200ms
Type "j"
Sleep 200ms

# Tab to Changes, open diff
Tab
Sleep 400ms
Enter
Sleep 1.5s

# Close diff with Esc (now fixed to close diff first)
Escape
Sleep 400ms

# Tab to Graph
Tab
Sleep 400ms

# Navigate graph, open commit detail
Type "j"
Sleep 150ms
Type "j"
Sleep 150ms
Enter
Sleep 1.5s

# Navigate commit files, open commit diff
Type "j"
Sleep 200ms
Enter
Sleep 1.5s

# Close all with Esc (layered: diff → files → panel nav)
Escape
Sleep 300ms
Escape
Sleep 300ms
Escape
Sleep 300ms

# Sort repos
Type "s"
Sleep 500ms
Type "s"
Sleep 500ms

# Quit
Type "q"
Sleep 200ms
