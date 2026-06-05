# Loop state — lane-loop
session_started: (set on first real run — UTC; scripts can't read the clock)
loop: lane-loop
branch: (set per session)
worktree: (set per session — abs path)
cycle_budget: 3            # completed cycles per session before handoff (override via RALPH_BUDGET)
cycles_this_session: 0     # reset to 0 on RESUME
cycles_total: 0            # carried across sessions
last_item: (none — template; DISCOVER will seed the backlog)
status: template — not yet started; run /lane-loop to DISCOVER and begin cycling
last_update: (set on first real run)
