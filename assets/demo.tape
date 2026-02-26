# VHS tape for gitpane demo GIF
# Run: vhs assets/demo.tape

Output assets/demo.gif

Set Theme "Catppuccin Mocha"
Set FontFamily "Fira Code"
Set FontSize 18
Set Width 1200
Set Height 800
Set Padding 16
Set Framerate 15
Set TypingSpeed 20ms
Set PlaybackSpeed 1

# Start gitpane
Type "gitpane --root /Users/afromero/Code"
Enter

# Wait for load
Sleep 4s

# Navigate repos
Type "j"
Sleep 300ms
Type "j"
Sleep 300ms

# Pause on three-panel overview
Sleep 2s

# Tab to Changes, open diff
Tab
Sleep 500ms
Enter
Sleep 3s

# Close diff with Esc
Escape
Sleep 500ms

# Tab to Graph
Tab
Sleep 500ms

# Pause on graph view
Sleep 2s

# Navigate graph, open commit detail
Type "j"
Sleep 200ms
Type "j"
Sleep 200ms
Enter
Sleep 2s

# Navigate commit files, open commit diff
Type "j"
Sleep 200ms
Enter
Sleep 3s

# Close all with Esc (layered: diff → files → panel nav)
Escape
Sleep 300ms
Escape
Sleep 300ms
Escape
Sleep 300ms

# Sort repos
Type "s"
Sleep 1s
Type "s"
Sleep 1s

# Quit
Type "q"
Sleep 300ms
