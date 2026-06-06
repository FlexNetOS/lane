# Loop state — lane-loop
session_started: 2026-06-05 (UTC date; scripts can't read the clock)
loop: lane-loop
branch: (between cycles — next: lane up --json, Batch 3)
worktree: (none active)
cycle_budget: 3            # completed cycles per session before handoff (override via RALPH_BUDGET)
cycles_this_session: 2     # RESUMED: c1 share #22, c2 stop #23 (both auto-merge armed)
cycles_total: 12           # carried across sessions
last_item: RESUME c2 — `lane stop --json` DONE (#23 armed). Batch 4 COMPLETE. Next: Batch 5 docs --json sync (DEEPER, non-churn).
status: HANDOFF at cycle_budget(3). Session B shipped #19/#20/#21 (up/down/start --json), all
        auto-merged hands-free. Batch 4 remainder: share --json, stop --json (then DEEPER re-DISCOVER
        or DONE gate — --json space exhausted after that). HANDOFF.md committed; ScheduleWakeup will
        auto-resume a fresh cycle. NO premature DONE, NO PRs left for a human.
last_update: 2026-06-05
