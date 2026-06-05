# lane backlog
Legend: [ ] todo · [x] done+verified · [!] blocked: <reason>

<!--
This file is the single source of truth for the lane-loop autonomous runner. It is SEEDED here
as a template; the loop's DISCOVER phase replaces these notes with real, deduped intents sourced
from open intents / docs roadmap / PRD parity gaps / ARCHITECTURE.md (preferred)/TODO notes / the
CLI surface / open issues, and re-deduped against origin/main + open PRs at the top of each cycle.

Recently shipped (do NOT re-propose): logs --json (#6), logs -n/--lines (#7), version --json (#8),
restart (#9). In-flight elsewhere: completions <shell> (feat-completions worktree).

Known candidate for a future run (kept here as a pointer, not yet claimed):
- [ ] Fix `lane doctor` false-negative CA-trust + port-forwarding probes (FlexNetOS/lane#5):
      trust check looks for basename rootCA.pem but installer writes lane.crt (cert/trust.rs);
      portfwd is_enabled() runs `iptables -t nat -C` without privilege (portfwd.rs) -> exit-4
      perm-denied reads as "not configured". Both report ✗ while the layer actually works.
-->
