# VHS tape for gitpane demo GIF
# Run: vhs assets/demo.tape

Output assets/demo.gif

Set Theme "Catppuccin Mocha"
Set FontFamily "Fira Code"
Set FontSize 18
Set Width 1200
Set Height 900
Set Padding 16
Set Framerate 15
Set TypingSpeed 20ms
Set PlaybackSpeed 1

# Start gitpane
Type "gitpane --root /Users/afromero/Code"
Enter

# Wait for load
# Repo order (yazi-ssh pinned first, then alpha):
#   0. yazi-ssh (pinned)
#   1. 3d-llm-client
#   2. afromero
#   3. capacitor-app
#   4. gitpane        ← ⎇3 * dirty
#   5. Hax-CV
#   6. Sotto          ← ⎇3 * ↑13
Sleep 4s

# Navigate slowly to show indicators on each repo
# j → 3d-llm-client
Type "j"
Sleep 500ms

# j → afromero
Type "j"
Sleep 500ms

# j → capacitor-app
Type "j"
Sleep 500ms

# j → gitpane (⎇3 worktrees, * dirty) — our repo, explore it
Type "j"
Sleep 2s

# Tab to Changes panel — show gitpane's dirty files
Tab
Sleep 1s

# Navigate files
Type "j"
Sleep 300ms

# Open diff for a changed file
Enter
Sleep 3s

# Close diff
Escape
Sleep 500ms

# Tab to Graph panel — show gitpane's commit graph
Tab
Sleep 1s

# Pause on commit graph
Sleep 2s

# Navigate commits
Type "j"
Sleep 200ms
Type "j"
Sleep 200ms
Type "j"
Sleep 200ms

# Open commit detail
Enter
Sleep 2s

# Navigate commit files, open commit diff
Type "j"
Sleep 200ms
Enter
Sleep 3s

# Close all with Esc (layered: diff → files → graph)
Escape
Sleep 300ms
Escape
Sleep 300ms
Escape
Sleep 300ms

# Back to repo list — continue down to Sotto
# j → Hax-CV
Type "j"
Sleep 500ms

# j → Sotto (⎇3 worktrees, * dirty, ↑13 ahead) — all indicators visible
Type "j"
Sleep 3s

# Sort to dirty-first — gitpane and Sotto jump to top
Type "s"
Sleep 2s

# Sort back to alphabetical
Type "s"
Sleep 1s

# Quit
Type "q"
Sleep 500ms
