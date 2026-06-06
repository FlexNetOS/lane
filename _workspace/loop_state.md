# Loop state — lane-loop
session_started: 2026-06-05 (UTC date; scripts can't read the clock)
loop: lane-loop
branch: (between cycles — next: lane up --json, Batch 3)
worktree: (none active)
cycle_budget: 3            # completed cycles per session before handoff (override via RALPH_BUDGET)
cycles_this_session: 3     # AT BUDGET → HAND OFF. c1 up #19, c2 down #20, c3 start #21 (all auto-merge armed)
cycles_total: 10           # carried across sessions
last_item: RESUME c3 — `lane start --json` DONE (#21 auto-merge armed). AT BUDGET → HANDOFF. Next: `lane share --json` (Batch 4).
status: HANDOFF at cycle_budget(3). Session B shipped #19/#20/#21 (up/down/start --json), all
        auto-merged hands-free. Batch 4 remainder: share --json, stop --json (then DEEPER re-DISCOVER
        or DONE gate — --json space exhausted after that). HANDOFF.md committed; ScheduleWakeup will
        auto-resume a fresh cycle. NO premature DONE, NO PRs left for a human.
last_update: 2026-06-05
