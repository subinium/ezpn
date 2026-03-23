# VHS tape for README hero GIF
# Run from project root: vhs assets/hero.tape

Output assets/hero.gif

Require ezpn

Set Shell "bash"
Set FontSize 18
Set Width 960
Set Height 540
Set Padding 16
Set TypingSpeed 80ms

# Show the command
Type "ezpn 2 3"
Sleep 500ms
Enter
Sleep 2s

# Pane 1
Type "echo 'Hello ezpn!'"
Enter
Sleep 800ms

# Pane 2
Ctrl+]
Sleep 400ms
Type "ls"
Enter
Sleep 800ms

# Pane 3
Ctrl+]
Sleep 400ms
Type "date"
Enter
Sleep 800ms

# Pane 4
Ctrl+]
Sleep 400ms
Type "whoami"
Enter
Sleep 800ms

# Pane 5
Ctrl+]
Sleep 400ms
Type "pwd"
Enter
Sleep 800ms

# Pane 6 — split it
Ctrl+]
Sleep 400ms
Ctrl+d
Sleep 1.5s

# Type in new pane
Type "echo 'Split!'"
Enter
Sleep 1.5s

# Show settings panel
Ctrl+g
Sleep 2s
Escape
Sleep 1s

# Quit
Ctrl+\
Sleep 500ms
