# VHS tape for three-panel overview screenshot
# Run: vhs assets/screenshot-main.tape

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

# Wait for full load + status updates
Sleep 6s

# Navigate to a repo with real dirty files
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms
Type "j"
Sleep 100ms

# Hold for overview screenshot
Sleep 1s
Screenshot assets/screenshot-main.png

# Quit
Type "q"
Sleep 100ms
