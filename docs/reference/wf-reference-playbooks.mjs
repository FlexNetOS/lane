export const meta = {
  name: 'reference-playbooks',
  description: 'Produce authoritative Rust implementation playbooks for the hard lane subsystems (proxy/TLS/daemon/tunnel/cert/cli)',
  phases: [{ title: 'Research' }, { title: 'Adversarial-review' }],
}

// args = { topics: [...], lane_repo?, slim_src? }
const parsedArgs = typeof args === 'string' ? JSON.parse(args) : (args ?? {})
const LANE_REPO = parsedArgs.lane_repo ?? '/home/drdave/Desktop/lane'
const SLIM_SRC  = parsedArgs.slim_src  ?? '/home/drdave/Downloads/slim-extract/slim-main'
const topics = parsedArgs.topics ?? []
if (topics.length === 0) throw new Error('reference-playbooks: no topics provided')
log(`Writing ${topics.length} implementation playbooks`)

const PREAMBLE = `You are writing an AUTHORITATIVE, code-heavy Rust implementation playbook that other
engineers will follow VERBATIM to implement part of \`lane\` — a faithful Rust port of the Go tool
\`slim\` (a local-HTTPS reverse-proxy + tunnel CLI). The exact crate versions are pinned in
${LANE_REPO}/Cargo.toml — your playbook MUST match those versions' real APIs.

GROUND TRUTH (offline, authoritative): read the vendored crate sources under
~/.cargo/registry/src/*/<crate>-<version>/ to confirm EXACT types, function signatures, feature
flags, and trait methods. Run e.g. \`find ~/.cargo/registry/src -maxdepth 1 -type d\` then read the
relevant crate's lib.rs / module files and their doc comments and examples. Do NOT invent APIs.
You MAY also use WebSearch/WebFetch for canonical examples/blog posts if available, but the
vendored source is the source of truth — cross-check anything from the web against it.

Read ${LANE_REPO}/ARCHITECTURE.md (the module contract) and the relevant Go source under
${SLIM_SRC}/ so your playbook maps cleanly onto the port.

Your playbook must contain: (1) the precise crate APIs to use (real signatures, copied from the
vendored source), (2) a complete, COMPILE-READY reference implementation sketch in Rust (idiomatic,
clippy-clean, async where relevant), (3) the exact slim behavior to reproduce, (4) a pitfalls section
(lifetimes, provider install, backpressure, partial reads, upgrade hijack, graceful shutdown, etc.),
(5) how to unit/integration test it with tokio. Be exhaustive and concrete — assume the reader copies
your code. Write the file with the Write tool.`

const SCHEMA = {
  type: 'object', additionalProperties: false,
  required: ['topic', 'file', 'summary'],
  properties: {
    topic: { type: 'string' },
    file: { type: 'string' },
    key_apis: { type: 'array', items: { type: 'string' }, description: 'exact crate APIs verified against vendored source' },
    pitfalls: { type: 'array', items: { type: 'string' } },
    summary: { type: 'string' },
  },
}

// pipeline: draft the playbook, then a second agent adversarially verifies it against the
// vendored crate source and rewrites any wrong/hand-waved API usage in place.
const results = await pipeline(
  topics,
  (t) => agent(`${PREAMBLE}

## PLAYBOOK TOPIC: ${t.title}
Write to: ${LANE_REPO}/${t.file}

Scope & must-cover:
${t.scope}

Relevant crates to verify against (read their vendored source): ${t.crates}
Relevant Go source: ${t.go}`,
    { label: `draft:${t.key}`, phase: 'Research', schema: SCHEMA }),
  (draft, t) => agent(`You are an adversarial reviewer for the Rust implementation playbook at
${LANE_REPO}/${t.file} (topic: "${t.title}"). Another engineer drafted it.

Your job: find every place where it uses a crate API that does NOT exist or is misused in the pinned
version, every async/lifetime/ownership bug in the reference code, and every slim behavior it got
wrong or omitted. VERIFY each crate call against the vendored source under
~/.cargo/registry/src/*/ (crates: ${t.crates}) — read the actual signatures. Cross-check slim
behavior against ${t.go}. Then EDIT the file in place to fix everything you found and fill any gaps,
so the reference code would actually compile against Cargo.toml. Be ruthless and concrete.`,
      { label: `verify:${t.key}`, phase: 'Adversarial-review', schema: {
        type: 'object', additionalProperties: false, required: ['file', 'verdict'],
        properties: {
          file: { type: 'string' },
          verdict: { type: 'string', enum: ['fixed', 'already-correct', 'major-rewrite'] },
          issues_found: { type: 'array', items: { type: 'string' } },
        } } })
)

return results.filter(Boolean)
