// 시험에 쓸 Chrome 위치. CHROME_PATH 환경 변수가 있으면 그것을 쓴다.
import { existsSync } from 'node:fs';

const candidates = [
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
];

export const CHROME = process.env.CHROME_PATH ?? candidates.find((p) => existsSync(p)) ?? candidates[0];
