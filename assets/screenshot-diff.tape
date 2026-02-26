# VHS tape for split diff view screenshot
# Run: vhs assets/screenshot-diff.tape

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

# Navigate to a repo with dirty files
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms

# Tab to Changes panel, open diff
Tab
Sleep 300ms
Enter
Sleep 2s

Screenshot assets/screenshot-diff.png

# Quit
Escape
Sleep 100ms
Type "q"
Sleep 100ms
