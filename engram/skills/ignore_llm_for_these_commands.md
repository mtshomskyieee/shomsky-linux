---
name: ignore_llm_for_these_commands
description: Interactive / TUI commands that run on the real TTY and never go through the LLM
always_load: false
---

# Bypass the LLM

Commands listed here are run directly on the terminal (stdin/stdout/stderr
inherited) in **agent** and **root** modes. The LLM is never consulted, and
output is not captured into the chat context.

Edit this file to grow or shrink the list. One command name per line
(basename only: `top`, not `/usr/bin/top`). Blank lines and `#` comments
are ignored. Markdown headings and bullets (`- top`) are also accepted.

```
# process / system monitors
top
htop
atop
iotop
btop
bpftop

# file managers / browsers
mc
ranger
nnn
lf
vifm

# editors
vi
vim
nvim
emacs
nano
micro
helix
hx

# pagers / watchers
less
more
most
watch

# multiplexers
tmux
screen
byobu

# other common TUIs
lazygit
lazydocker
tig
ftp
sftp
gdb
lldb
```
