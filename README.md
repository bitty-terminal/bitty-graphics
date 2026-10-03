# bitty-graphics

Bounded graphics decode extension crate (landed CTX-0003, independently verified CTX-0004). Png-only decode unit plus the raster mechanics subset; placement policy, rect helpers and renderer upload validation stay in Core. Read [AGENTS](AGENTS.md) and [TODO](TODO.md).

Prerequisite: W-133 / bitty-terminal-docs CTX-0089, Issue #170. Bounded protocol intake, placement, resource enforcement and upload validation remain Core responsibilities unless an accepted contract says otherwise.

Local phases CTX-0001 -> CTX-0002 -> CTX-0003 -> CTX-0004 map to Issues #4 -> #3 -> #2 -> #1. All four phases are complete: bootstrap, contract, landed decode/raster crate with tests, and independent verification with Core W-141 parity.
