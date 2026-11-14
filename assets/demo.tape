# Run after creating the sample workspace with assets/demo-workspace.sh.
# Set GITPANE_DEMO_ROOT, GITPANE_DEMO_CONFIG, and GITPANE_DEMO_BINARY.
Output assets/demo.gif

Set Theme "Catppuccin Mocha"
Set FontFamily "Fira Code"
Set FontSize 14
Set Width 1200
Set Height 760
Set Padding 12
Set Framerate 10
Set TypingSpeed 10ms

Hide
Type 'sh assets/demo-session.sh'
Enter
Sleep 5s
Show
Sleep 2s

# Select the submodule to show its own changes and history.
Type "j"
Sleep 3s
Type "k"
Sleep 1s

# Expand and select the saved stash.
Type "S"
Sleep 2s
Type "jjj"
Sleep 3s
Type "kkk"
Sleep 1s

# Browse the selected repository's changed files and diff.
Tab
Sleep 1s
Enter
Sleep 3s
Escape
Sleep 1s

# Browse its history and open a commit.
Tab
Sleep 1s
Type "j"
Sleep 1s
Enter
Sleep 3s
Escape
Sleep 1s
Escape
Sleep 7s
Hide
Type "q"
Sleep 500ms
