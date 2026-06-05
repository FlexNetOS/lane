export const meta = {
  name: 'port-layer',
  description: 'Implement one dependency layer of the lane Rust port: one agent per module, faithful Go→Rust port',
  phases: [{ title: 'Implement' }],
}

// args = { layer: string, modules: [{ name, go_sources:[string], rust_files:[string], notes:string }], lane_repo?, slim_src? }
const parsedArgs = typeof args === 'string' ? JSON.parse(args) : (args ?? {})
const LANE_REPO = parsedArgs.lane_repo ?? '.'
const SLIM_SRC  = parsedArgs.slim_src  ?? '/home/drdave/Downloads/slim-extract/slim-main'
const layer = parsedArgs.layer ?? 'layer'
const modules = parsedArgs.modules ?? []
if (modules.length === 0) throw new Error('port-layer: no modules provided in args')
log(`Porting ${layer}: ${modules.map(m => m.name).join(', ')}`)

const PREAMBLE = `You are porting the Go tool **slim** to Rust as a new project named **lane**.
This is a FAITHFUL port: preserve behavior exactly — same control flow, same edge cases,
same error message text where practical, same on-disk layout, same numeric constants.

CRITICAL RULES:
- The binding API contract is ${LANE_REPO}/ARCHITECTURE.md — READ IT FIRST and match
  the public signatures for your module EXACTLY so other modules integrate. Also read PRD.md if useful.
- Apply ALL slim→lane renames from the ARCHITECTURE rename table (~/.lane, "# lane" hosts marker,
  iptables chain LANE, pf anchor com.lane, env LANE_*, CN "lane Root CA", [lane] log prefix, .lane.yaml, etc.).
- Original Go source (read-only reference) lives under ${SLIM_SRC}/.
- You OWN only the Rust files listed for your module (including its mod.rs). DO NOT edit Cargo.toml,
  src/lib.rs, src/main.rs, or any other module's files — their public APIs are fixed by ARCHITECTURE.md.
- Dependencies are ALREADY in Cargo.toml (tokio, hyper, hyper-util, http, http-body-util, bytes,
  httparse, tokio-rustls, rustls[ring], rustls-pemfile, rustls-pki-types, webpki-roots, rcgen[ring],
  rsa, x509-parser, clap[derive], serde, serde_json, serde_yaml, reqwest[rustls], tokio-tungstenite,
  owo-colors, indicatif, comfy-table, regex, fs2, libc, dirs, chrono, rand, hex, sha2, open, humantime,
  anyhow, thiserror, flate2, tar; dev: tempfile, serial_test). Do NOT add deps; use these.
- Write idiomatic, warning-clean Rust (clippy-friendly). Functions return anyhow::Result unless the
  contract says otherwise. Lower layers are already implemented (real code on disk) — read them if you
  need their exact API, but they conform to ARCHITECTURE.md.
- Where the contract is async (e.g. proxy/daemon/health/doctor/send_ipc) honor that.
- Install the rustls ring provider where you create rustls configs if needed (lib::install_crypto_provider()).

TESTS: For your module, ALSO port the corresponding Go *_test.go file(s) into a \`#[cfg(test)] mod tests\`
in the appropriate Rust file when the test is unit-level / pure-logic / file-based. For file/path/config
tests, isolate state with tempfile::TempDir and override HOME (std::env::set_var("HOME", tmp)) guarded by
#[serial_test::serial]. Where a Go test asserts EXACT lipgloss ANSI escape bytes, port it to assert the
semantic intent instead (text present, wrapped in an escape) — functional parity, not byte-identical
escapes. Skip heavy async/integration tests with a \`// TODO(test-phase): <name>\` note; those are handled
later. Do NOT write tests that fail to compile.

Do NOT run \`cargo build\`/\`cargo test\` (the crate has other not-yet-implemented modules and it will fail
spuriously) — the orchestrator builds the whole crate after the layer. You MAY run \`cargo fmt -- <yourfile>\`.`

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['module', 'files_written', 'summary'],
  properties: {
    module: { type: 'string' },
    files_written: { type: 'array', items: { type: 'string' } },
    tests_ported: { type: 'array', items: { type: 'string' } },
    summary: { type: 'string', description: 'what was implemented, in 2-4 sentences' },
    deviations: { type: 'array', items: { type: 'string' }, description: 'any departures from the contract or Go behavior, with reason' },
    uncertainties: { type: 'array', items: { type: 'string' }, description: 'integration risks the orchestrator should double-check at build time' },
  },
}

const results = await parallel(modules.map(m => () => {
  const prompt = `${PREAMBLE}

## YOUR MODULE: \`${m.name}\`  (lane ${layer})

Port these Go source file(s) (read them in full):
${m.go_sources.map(s => `  - ${SLIM_SRC}/${s}`).join('\n')}

Write/overwrite exactly these Rust file(s) (you own them, including mod.rs):
${m.rust_files.map(s => `  - ${LANE_REPO}/${s}`).join('\n')}

Module-specific notes:
${m.notes}

Steps: (1) Read ARCHITECTURE.md §\`src/${m.name}\` for the exact required signatures. (2) Read the Go
source above. (3) Read the existing placeholder/leaf file(s) you will overwrite. (4) Read any lower-layer
Rust modules whose API you call, to use their real signatures. (5) Implement faithfully + port unit tests.
(6) Self-review for clippy cleanliness and contract conformance.`
  return agent(prompt, { label: `port:${m.name}`, phase: 'Implement', schema: SCHEMA })
}))

return results.filter(Boolean)
