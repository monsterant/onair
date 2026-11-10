// onair 첫 화면: 서버 상태, 로그인 상태, 로그인·가입 양식.
// 방송 목록·상세는 #3에서 붙인다.
// 사용자 입력이나 서버 값은 textContent로만 넣는다(innerHTML을 쓰지 않아 스크립트 끼워 넣기를 막는다).
import './style.css';
import { ApiError, type User, currentUser, demoLogin, login, logout, signup } from './api';

const app = document.querySelector<HTMLElement>('#app')!;

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> = {},
  ...children: (Node | string)[]
): HTMLElementTagNameMap[K] {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children);
  return node;
}

function message(e: unknown): string {
  return e instanceof ApiError ? e.message : '서버에 연결하지 못했습니다';
}

async function render() {
  let user: User | null = null;
  let loadError = '';
  try {
    user = await currentUser();
  } catch (e) {
    loadError = message(e);
  }

  const header = el(
    'header',
    { className: 'topbar' },
    el('strong', { className: 'logo', textContent: 'onair' }),
    user
      ? el(
          'span',
          { className: 'who' },
          el('span', { textContent: `${user.username}님` }),
          el('button', {
            textContent: '로그아웃',
            onclick: async () => {
              await logout().catch(() => {});
              render();
            },
          }),
        )
      : el('span', { className: 'who', textContent: '로그인하지 않음' }),
  );

  const status = el('p', { className: 'status', textContent: '서버 확인 중...' });
  fetch('/api/health')
    .then((r) => r.json())
    .then((h: { status: string; db: string }) => {
      status.textContent = `서버 ${h.status}, DB ${h.db}`;
    })
    .catch(() => {
      status.textContent = '백엔드에 연결하지 못했습니다';
    });

  const main = el('main', { className: 'content' });
  if (loadError) main.append(el('p', { className: 'error', textContent: loadError }));
  main.append(user ? el('p', { textContent: `${user.username}님, 로그인되어 있습니다.` }) : authBox());

  app.replaceChildren(header, main, status);
}

function authBox(): HTMLElement {
  const username = el('input', { name: 'username', autocomplete: 'username', placeholder: '아이디 (영문·숫자·_ 3~20자)', required: true });
  const password = el('input', { name: 'password', type: 'password', autocomplete: 'current-password', placeholder: '비밀번호 (8자 이상)', required: true });
  const error = el('p', { className: 'error', role: 'alert' });

  const run = (fn: () => Promise<unknown>) => async () => {
    error.textContent = '';
    try {
      await fn();
      render();
    } catch (e) {
      error.textContent = message(e);
    }
  };

  const form = el(
    'form',
    { className: 'auth' },
    el('h2', { textContent: '로그인' }),
    username,
    password,
    el(
      'div',
      { className: 'row' },
      el('button', { type: 'submit', textContent: '로그인' }),
      el('button', { type: 'button', textContent: '회원가입', onclick: run(() => signup(username.value, password.value)) }),
    ),
    error,
  );
  form.onsubmit = (ev) => {
    ev.preventDefault();
    run(() => login(username.value, password.value))();
  };

  const demo = el(
    'section',
    { className: 'demo' },
    el('h3', { textContent: '테스트 계정으로 바로 로그인' }),
    el('p', { className: 'hint', textContent: '심사·시험용입니다. 두 계정으로 방송 소유자와 다른 사용자를 비교할 수 있습니다.' }),
    el(
      'div',
      { className: 'row' },
      el('button', { type: 'button', textContent: 'demo1로 로그인', onclick: run(() => demoLogin(1)) }),
      el('button', { type: 'button', textContent: 'demo2로 로그인', onclick: run(() => demoLogin(2)) }),
    ),
  );

  return el('div', {}, form, demo);
}

render();
