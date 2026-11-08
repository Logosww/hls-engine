import { aac_suite } from '../../target/runtime/pkg/hls_engine_runtime_tests.js';
export async function verify() {
  const report=JSON.parse(await aac_suite());
  if(report.length!==4||report[2].summary.corrected_frames!=='20'||report[2].hash!==report[3].hash)throw new Error('AAC repair contract');
  if(report[0].error!=='AacTimestampOverlap'||!report[0].overlap||report[1].overlap!==null)throw new Error('AAC overlap diagnostic');
  return {report,bridge:{strict:true,boundedRepair:true}};
}
