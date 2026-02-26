# VHS tape for commit detail drill-down screenshot
# Run: vhs assets/screenshot-commit.tape

Set Theme "Catppuccin Mocha"
Set FontFamily "Fira Code"
Set FontSize 18
Set Width 1200
Set Height 800
Set Padding 16
Set TypingSpeed 20ms
Set PlaybackSpeed 1

# Start gitpane
Type "gitpane --root /Users/afromero/Code"
Enter

# Wait for full load
Sleep 6s

# Navigate to a repo
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms

# Tab to Graph panel
Tab
Sleep 300ms
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

Screenshot assets/screenshot-commit.png

# Quit
Escape
Sleep 100ms
Escape
Sleep 100ms
Type "q"
Sleep 100ms
