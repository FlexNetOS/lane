export const meta = {
  name: 'fanout-write',
  description: 'Fan out one agent per item to write a documentation/asset file for the lane project',
  phases: [{ title: 'Write' }],
}

const parsedArgs = typeof args === 'string' ? JSON.parse(args) : (args ?? {})
const LANE_REPO = parsedArgs.lane_repo ?? '.'
const preamble = parsedArgs.preamble ?? ''
const items = parsedArgs.items ?? []
if (items.length === 0) throw new Error('fanout-write: no items provided')
log(`Writing ${items.length} files: ${items.map(i => i.file).join(', ')}`)

const SCHEMA = {
  type: 'object', additionalProperties: false,
  required: ['file', 'summary'],
  properties: {
    file: { type: 'string' },
    summary: { type: 'string' },
    word_count: { type: 'number' },
  },
}

const results = await parallel(items.map(it => () =>
  agent(`${preamble}\n\n## YOUR DELIVERABLE\nWrite this file: ${LANE_REPO}/${it.file}\n\n${it.prompt}`,
    { label: `write:${it.key}`, phase: 'Write', schema: SCHEMA })))

return results.filter(Boolean)
