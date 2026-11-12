// 브라우저 시험을 차례로 모두 돌린다.
//   node run-all.mjs [BASE] [--gap 초]
// 실제 서버에 돌릴 때는 같은 IP의 요청 횟수 제한(분당 20회) 때문에 --gap 60을 준다.
// 각 시험은 결과를 PASS/FAIL 줄로 찍는다. FAIL이 하나라도 있으면 끝 코드가 1이다.
import { spawnSync } from 'node:child_process';

const args = process.argv.slice(2);
const gi = args.indexOf('--gap');
const gap = gi >= 0 ? Number(args.splice(gi, 2)[1]) : 0;
const base = args[0] ?? 'http://localhost:5173/';
const tests = ['auth', 'pc', 'mobile', 'comments', 'donate', 'recording', 'reload', 'ux', 'ux2', 'offline', 'devices', 'timeouts'];

let pass = 0;
let fail = 0;
for (const [i, t] of tests.entries()) {
  if (i > 0 && gap > 0) await new Promise((r) => setTimeout(r, gap * 1000));
  const r = spawnSync(process.execPath, [`${t}.mjs`, base], { cwd: import.meta.dirname, encoding: 'utf8', timeout: 10 * 60 * 1000 });
  const lines = `${r.stdout}${r.stderr}`.split('\n');
  const p = lines.filter((l) => l.startsWith('PASS')).length;
  const f = lines.filter((l) => l.startsWith('FAIL'));
  pass += p;
  // 끝 코드가 0이 아니거나 시간을 넘겨(10분) 멈춘 경우(status가 null)도 실패로 센다
  const crashed = r.status !== 0;
  fail += f.length + (crashed ? 1 : 0);
  const why = r.error ? ' (시간 초과 또는 실행 실패)' : crashed ? ` (exit ${r.status})` : '';
  console.log(`${f.length || crashed ? 'FAIL' : 'ok  '} ${t}: ${p} pass, ${f.length} fail${why}`);
  for (const l of f) console.log(`     ${l}`);
  // 스크립트가 죽었으면 오류 마지막 몇 줄을 보여 준다
  if (crashed) {
    const err = (r.stderr ?? '').split('\n').filter((l) => /Error/.test(l)).slice(0, 3);
    for (const l of err) console.log(`     ${l.trim()}`);
  }
}
console.log(`\n합계 ${pass} pass, ${fail} fail`);
process.exit(fail ? 1 : 0);
