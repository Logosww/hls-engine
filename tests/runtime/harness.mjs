import init from '../../target/runtime/pkg/hls_transmux_runtime_tests.js';
import { verify as playlist } from './playlist.mjs';
import { verify as keys } from './keys.mjs';
import { verify as resources } from './resources.mjs';
import { verify as prepared } from './prepared.mjs';
import { verify as contracts } from './contracts.mjs';
import { verify as timeline } from './timeline.mjs';
import { verify as samples } from './samples.mjs';

export async function verify(wasm, progress = () => {}) {
  progress({phase:'wasm-init',status:'start'});
  const module = await init(wasm ? { module_or_path: wasm } : undefined);
  const results = {};
  for (const [name, run] of Object.entries({playlist,keys,resources,prepared,contracts,timeline,samples})) {
    progress({phase:name,status:'start'});
    results[name] = await run(module.memory);
    progress({phase:name,status:'complete'});
  }
  return {
    profile: {timeline: results.timeline.profile, samples: results.samples.profile},
    report: Object.fromEntries(Object.entries(results).map(([name, result]) => [name, result.report])),
    bridge: Object.fromEntries(Object.entries(results).map(([name, result]) => [name, result.bridge])),
  };
}
