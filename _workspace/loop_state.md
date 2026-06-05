# Loop state — lane-loop
session_started: 2026-06-05 (UTC date; scripts can't read the clock)
loop: lane-loop
branch: fix-doctor-probes
worktree: ~/Desktop/meta/.worktrees/fix-doctor-probes/lane
cycle_budget: 3            # completed cycles per session before handoff (override via RALPH_BUDGET)
cycles_this_session: 2     # reset to 0 on RESUME
cycles_total: 2            # carried across sessions
last_item: doctor#5 false-negative probes — DONE+verified, PR #14 open (awaiting human merge)
status: BACKLOG CLEARED (2/2 items done+verified). NOT terminal-DONE — SAFE mode left both PRs
        (#13 completions, #14 doctor#5) OPEN, unmerged; main has neither feature, so there is no
        integrated green to certify. No DONE/NEEDS-HUMAN sentinel written. Next step is a HUMAN
        MERGE decision (or re-run via ralph-lane.sh with LANE_APPLY=1 for unattended apply). On a
        future RESUME after the PRs merge, run the DONE gate on the integrated main.
last_update: 2026-06-05
