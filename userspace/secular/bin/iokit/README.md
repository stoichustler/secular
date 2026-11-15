# IOKit

Console file tools installed in `/boot/bin`. To add a command, follow
[ADDING.md](ADDING.md).

| Command | Role |
| --- | --- |
| `bootstat` | Print time since boot, ticks, CPUs, and memory |
| `cat` | Write file contents to stdout |
| `clear` | Clear the console (`-x` keeps scrollback) |
| `cope` | Inline vi-style editor drawn below the prompt |
| `cpustat` | CPU identity, sampled load, and scheduler-pressure hints |
| `delay` | Delay for the given number of milliseconds |
| `elfdump` | Dump ELF headers of an executable or object |
| `fxtdump` | Summarize FXT from `ktrace save` / `ktrace stream` (`/stow/*.fxt`) |
| `hexdump` | Write a file as canonical hex and ASCII |
| `kdmips` | Dhrystone 2.1 integer throughput (KDMIPS) |
| `lktee` | Trusty SPD UUID, decoded LK build time, API, S-EL1 NOP |
| `runman` | Animate a solid-block progress bar |
| `tree` | List directory contents in a tree |
