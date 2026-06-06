# Loop state — lane-loop
session_started: 2026-06-05 (UTC date; scripts can't read the clock)
loop: lane-loop
branch: (between cycles — next: domain-add-json, after #16 merges)
worktree: (none active)
cycle_budget: 3            # completed cycles per session before handoff (override via RALPH_BUDGET)
cycles_this_session: 3     # cycle1=domain list --json (#15 MERGED), cycle2=doctor async (dedup-drop), cycle3=domain verify --json (#16 auto-merge armed)
cycles_total: 5            # carried across sessions
last_item: Batch-1 CLEARED — #15 MERGED, doctor-async already-shipped (dedup), #16 open auto-merge armed. Batch-2 re-DISCOVERED (domain add/remove --json).
status: ACTIVE, self-pacing. Batch 1 done (3/3). Re-DISCOVERED Batch 2: domain add/remove --json
        (completes domain-subcommand JSON coverage). Batch 2 touches domain.rs (same file as #16),
        so the next cycle MUST branch from a main that has #16 merged — self-paced a ScheduleWakeup
        to continue once #16 auto-merges (poll `gh pr view 16 --json state`). At cycle_budget (3) for
        this session: next session continues Batch 2. NO premature DONE; loop stays alive via the
        durable backlog. NO-HUMAN-IN-LOOP: every cycle opens a PR + arms auto-merge.
last_update: 2026-06-05
