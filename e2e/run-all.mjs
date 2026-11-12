// 브라우저 시험을 차례로 모두 돌린다.
//   node run-all.mjs [BASE] [--gap 초]
// 실제 서버에 돌릴 때는 같은 IP의 요청 횟수 제한(분당 20회) 때문에 --gap 60을 준다.
// 각 시험은 결과를 PASS/FAIL 줄로 찍는다. FAIL이 하나라도 있으면 끝 코드가 1이다.
import { spawnSync } from 'node:child_process';

const args = process.argv.slice(2);
const gi = args.indexOf('--gap');
const gap = gi >= 0 ? Number(args.splice(gi, 2)[1]) : 0;
const base = args[0] ?? 'http://localhost:5173/';
const tests = ['auth', 'pc', 'mobile', 'comments', 'donate', 'recording', 'reload', 'ux', 'ux2', 'offline', 'devices'];

let pass = 0;
let fail = 0;
for (const [i, t] of tests.entries()) {
  if (i > 0 && gap > 0) await new Promise((r) => setTimeout(r, gap * 1000));
  const r = spawnSync(process.execPath, [`${t}.mjs`, base], { cwd: import.meta.dirname, encoding: 'utf8' });
  const lines = `${r.stdout}${r.stderr}`.split('\n');
  const p = lines.filter((l) => l.startsWith('PASS')).length;
  const f = lines.filter((l) => l.startsWith('FAIL'));
  pass += p;
  fail += f.length + (r.status === 0 ? 0 : 1);
  console.log(`${f.length || r.status ? 'FAIL' : 'ok  '} ${t}: ${p} pass, ${f.length} fail${r.status ? ` (exit ${r.status})` : ''}`);
  for (const l of f) console.log(`     ${l}`);
}
console.log(`\n합계 ${pass} pass, ${fail} fail`);
process.exit(fail ? 1 : 0);
