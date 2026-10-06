# bitty-graphics

Bounded graphics decode extension crate (landed CTX-0003, independently verified CTX-0004). Png-only decode unit plus the raster mechanics subset; placement policy, rect helpers and renderer upload validation stay in Core. Read [AGENTS](AGENTS.md). Task management lives in CarryCtx.

Prerequisite: W-133 accepted contract specifications/graphics-extraction-contract.md (bitty-terminal-docs specify c28681db closed #170, promotion 3479ce12 closed #184). Bounded protocol intake, placement, resource enforcement and upload validation remain Core responsibilities unless an accepted contract says otherwise.

Local phases CTX-0001 -> CTX-0002 -> CTX-0003 -> CTX-0004 map to Issues #4 -> #3 -> #2 -> #1. CarryCtx reports all four phases complete; GitHub Issues #4, #2 and #1 are closed. This change closes #3 on the accepted W-133 contract.
