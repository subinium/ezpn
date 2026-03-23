# VHS tape for full ezpn demo
# Run from project root: vhs assets/demo.tape

Output assets/demo.gif

Require ezpn

Set Shell "bash"
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 20
Set TypingSpeed 60ms

# Start with 2x2 grid
Type "ezpn 2 2"
Enter
Sleep 2s

# Type in pane 1
Type "echo 'Hello from pane 1!'"
Enter
Sleep 1s

# Move to pane 2
Ctrl+]
Sleep 500ms
Type "ls -la"
Enter
Sleep 1s

# Move to pane 3
Ctrl+]
Sleep 500ms
Type "date && cal"
Enter
Sleep 1s

# Move to pane 4
Ctrl+]
Sleep 500ms
Type "echo 'Splitting this pane...'"
Enter
Sleep 1s

# Split pane 4 horizontally (left|right)
Ctrl+d
Sleep 1.5s

# Type in new pane
Type "echo 'New split!'"
Enter
Sleep 1s

# Split vertically (top/bottom)
Ctrl+e
Sleep 1.5s

# Cycle through all panes
Ctrl+]
Sleep 400ms
Ctrl+]
Sleep 400ms
Ctrl+]
Sleep 400ms
Ctrl+]
Sleep 400ms

# Open settings panel
Ctrl+g
Sleep 2s

# Close settings
Escape
Sleep 1s

# Show the final layout
Sleep 2s

# Quit
Ctrl+\
Sleep 500ms
