// onair 화면. 주소의 # 뒤를 보고 화면을 고른다(서버 설정 없이 새로고침해도 같은 화면이 나온다).
//   #/          방송 목록
//   #/login     로그인·가입·테스트 계정
//   #/new       방송 만들기(로그인 필요)
//   #/b/{id}    방송 상세·시청, 소유자는 수정·삭제·종료·송출 주소
// 사용자 입력이나 서버 값은 textContent로만 넣는다(innerHTML을 쓰지 않아 스크립트 끼워 넣기를 막는다).
import './style.css';
import {
  ApiError,
  type Broadcast,
  type Status,
  type User,
  createBroadcast,
  currentUser,
  deleteBroadcast,
  demoLogin,
  endBroadcast,
  getBroadcast,
  listBroadcasts,
  login,
  logout,
  publishInfo,
  signup,
  updateBroadcast,
} from './api';
// 플레이어(OvenPlayer+hls.js, 약 1MB)는 상세 화면에서만 불러온다. 목록 화면을 가볍게 하려는 것이다.
const player = () => import('./player');

const app = document.querySelector<HTMLElement>('#app')!;
let me: User | null = null;
/** 화면을 바꿀 때 지울 것(주기적 새로고침, 플레이어) */
let cleanup: (() => void)[] = [];

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> & Record<string, unknown> = {},
  ...children: (Node | string)[]
): HTMLElementTagNameMap[K] {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children);
  return node;
}

function message(e: unknown): string {
  return e instanceof ApiError ? e.message : '서버에 연결하지 못했습니다';
}

const STATUS_LABEL: Record<Status, string> = { ready: '대기', live: '방송 중', ended: '종료' };

function badge(s: Status) {
  return el('span', { className: `badge ${s}`, textContent: STATUS_LABEL[s] });
}

function go(hash: string) {
  if (location.hash === hash) route();
  else location.hash = hash;
}

/** 버튼을 누르는 동안 막고, 실패하면 오류 칸에 적는다 */
function action(btn: HTMLButtonElement, error: HTMLElement, fn: () => Promise<unknown>) {
  return async () => {
    error.textContent = '';
    btn.disabled = true;
    try {
      await fn();
    } catch (e) {
      error.textContent = message(e);
    } finally {
      btn.disabled = false;
    }
  };
}

// ---------- 머리말 ----------

function header(): HTMLElement {
  const right = el('span', { className: 'who' });
  if (me) {
    right.append(
      el('a', { href: '#/new', className: 'button', textContent: '방송 만들기' }),
      el('span', { textContent: `${me.username}님` }),
      el('button', {
        type: 'button',
        textContent: '로그아웃',
        onclick: async () => {
          await logout().catch(() => {});
          me = null;
          route();
        },
      }),
    );
  } else {
    right.append(el('a', { href: '#/login', className: 'button', textContent: '로그인' }));
  }
  return el('header', { className: 'topbar' }, el('a', { href: '#/', className: 'logo', textContent: 'onair' }), right);
}

// ---------- 목록 ----------

async function listPage(main: HTMLElement) {
  main.append(el('h2', { textContent: '방송 목록' }));
  const box = el('div', { className: 'list', textContent: '불러오는 중...' });
  main.append(box);
  const load = async () => {
    try {
      const items = await listBroadcasts();
      if (items.length === 0) {
        box.replaceChildren(el('p', { className: 'hint', textContent: '아직 방송이 없습니다.' }));
        return;
      }
      box.replaceChildren(
        ...items.map((b) =>
          el(
            'a',
            { className: 'card', href: `#/b/${b.id}` },
            el('div', { className: 'card-top' }, badge(b.status), el('strong', { textContent: b.title })),
            el('span', { className: 'hint', textContent: b.owner_username }),
          ),
        ),
      );
    } catch (e) {
      box.replaceChildren(el('p', { className: 'error', textContent: message(e) }));
    }
  };
  await load();
  const timer = setInterval(load, 10_000);
  cleanup.push(() => clearInterval(timer));
}

// ---------- 로그인 ----------

function loginPage(main: HTMLElement) {
  if (me) {
    main.append(el('p', { textContent: `${me.username}님, 로그인되어 있습니다.` }));
    return;
  }
  const username = el('input', { name: 'username', autocomplete: 'username', placeholder: '아이디 (영문·숫자·_ 3~20자)', required: true });
  const password = el('input', { name: 'password', type: 'password', autocomplete: 'current-password', placeholder: '비밀번호 (8자 이상)', required: true });
  const error = el('p', { className: 'error', role: 'alert' });
  const done = (u: User) => {
    me = u;
    go('#/');
  };
  const loginBtn = el('button', { type: 'submit', textContent: '로그인' });
  const signupBtn = el('button', { type: 'button', textContent: '회원가입' });
  signupBtn.onclick = action(signupBtn, error, async () => done(await signup(username.value, password.value)));
  const form = el('form', { className: 'auth panel' }, el('h2', { textContent: '로그인' }), username, password, el('div', { className: 'row' }, loginBtn, signupBtn), error);
  form.onsubmit = (ev) => {
    ev.preventDefault();
    action(loginBtn, error, async () => done(await login(username.value, password.value)))();
  };

  const demoError = el('p', { className: 'error', role: 'alert' });
  const demoBtn = (n: 1 | 2) => {
    const b = el('button', { type: 'button', textContent: `demo${n}로 로그인` });
    b.onclick = action(b, demoError, async () => done(await demoLogin(n)));
    return b;
  };
  const demo = el(
    'section',
    { className: 'demo panel' },
    el('h3', { textContent: '테스트 계정으로 바로 로그인' }),
    el('p', { className: 'hint', textContent: '심사·시험용 공용 계정입니다. 두 계정으로 방송 소유자와 다른 사용자를 비교할 수 있습니다.' }),
    el('div', { className: 'row' }, demoBtn(1), demoBtn(2)),
    demoError,
  );
  main.append(form, demo);
}

// ---------- 만들기 ----------

function newPage(main: HTMLElement) {
  if (!me) {
    main.append(el('p', {}, '방송을 만들려면 ', el('a', { href: '#/login', textContent: '로그인' }), '하세요.'));
    return;
  }
  const title = el('input', { name: 'title', placeholder: '방송 제목 (100자 이하)', maxLength: 100, required: true });
  const description = el('textarea', { name: 'description', placeholder: '설명 (1000자 이하, 선택)', maxLength: 1000, rows: 4 });
  const error = el('p', { className: 'error', role: 'alert' });
  const submit = el('button', { type: 'submit', textContent: '만들기' });
  const form = el('form', { className: 'panel' }, el('h2', { textContent: '방송 만들기' }), title, description, submit, error);
  form.onsubmit = (ev) => {
    ev.preventDefault();
    action(submit, error, async () => {
      const b = await createBroadcast(title.value, description.value);
      go(`#/b/${b.id}`);
    })();
  };
  main.append(form);
}

// ---------- 상세 ----------

async function detailPage(main: HTMLElement, id: number, gen: number) {
  let b: Broadcast;
  try {
    b = await getBroadcast(id);
  } catch (e) {
    main.append(el('p', { className: 'error', textContent: message(e) }));
    return;
  }
  // 기다리는 사이 다른 화면으로 옮겼으면 아무것도 하지 않는다(늦게 끝난 요청이 새 화면을 건드리지 않게, #27 V17)
  if (gen !== routeGen) return;
  const isOwner = me?.id === b.owner_id;

  const statusBox = el('span');
  const titleEl = el('h2');
  const descEl = el('p', { className: 'description' });
  const playerBox = el('div', { className: 'player-box' });
  const offAir = el('p', { className: 'hint' });
  let playing = false;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let gone = false;

  // 재생 오류가 나면 3초 뒤 플레이어를 다시 만든다(#27 V2)
  const onPlayerError = () => {
    clearTimeout(retry);
    retry = setTimeout(() => {
      if (gen !== routeGen || gone || b.status !== 'live') return;
      playing = false;
      paint();
    }, 3_000);
  };

  const paint = () => {
    statusBox.replaceChildren(badge(b.status));
    titleEl.textContent = b.title;
    descEl.textContent = b.description;
    const live = b.status === 'live';
    if (live && !playing) {
      playerBox.replaceChildren(el('div', { id: 'player' }));
      player().then((p) => {
        if (gen === routeGen && b.status === 'live') p.startPlayer('player', b.stream_name, onPlayerError);
      });
      playing = true;
    } else if (!live && playing) {
      clearTimeout(retry);
      player().then((p) => p.stopPlayer('player'));
      playerBox.replaceChildren();
      playing = false;
    }
    offAir.textContent = live ? '' : b.status === 'ended' ? '종료된 방송입니다.' : '아직 방송 전입니다. 방송이 시작되면 자동으로 재생됩니다.';
    owner?.update(b);
  };

  main.append(
    el('div', { className: 'detail-head' }, statusBox, titleEl),
    el('p', { className: 'hint', textContent: `방송자: ${b.owner_username}` }),
    playerBox,
    offAir,
    descEl,
  );
  const owner = isOwner ? ownerPanel(b, (nb) => { b = nb; paint(); }) : null;
  if (owner) main.append(owner.el);
  paint();

  // 5초마다 상태를 다시 본다(방송이 시작·종료되면 플레이어를 켜고 끈다)
  const stop = () => {
    clearInterval(timer);
    clearTimeout(retry);
    player().then((p) => p.stopPlayer('player'));
  };
  const timer = setInterval(async () => {
    try {
      b = await getBroadcast(id);
      if (gen === routeGen) paint();
    } catch (e) {
      // 방송이 지워졌으면 멈추고 알린다(#27 V10). 그 밖의 오류는 잠깐 끊긴 것으로 보고 다음에 다시 본다.
      if (e instanceof ApiError && e.status === 404) {
        gone = true;
        stop();
        main.replaceChildren(el('p', { className: 'hint', textContent: '삭제된 방송입니다.' }), el('a', { href: '#/', textContent: '목록으로' }));
      }
    }
  }, 5_000);
  cleanup.push(stop);
}

function ownerPanel(b: Broadcast, onChange: (b: Broadcast) => void): { el: HTMLElement; update: (b: Broadcast) => void } {
  const error = el('p', { className: 'error', role: 'alert' });

  // 수정
  const title = el('input', { value: b.title, maxLength: 100 });
  const description = el('textarea', { value: b.description, maxLength: 1000, rows: 3 });
  const save = el('button', { type: 'button', textContent: '저장' });
  save.onclick = action(save, error, async () => onChange(await updateBroadcast(b.id, title.value, description.value)));

  // 송출 주소(소유자만)
  const publishBox = el('div', { className: 'publish' });
  const showPublish = el('button', { type: 'button', textContent: '송출 주소 보기' });
  showPublish.onclick = action(showPublish, error, async () => {
    const p = await publishInfo(b.id);
    publishBox.replaceChildren(
      el('p', { className: 'hint', textContent: 'WHIP 송출 주소입니다. 남에게 보여 주지 마세요(이 주소를 가진 사람은 이 방송으로 송출할 수 있습니다). OBS 30 이상의 WHIP 출력에도 쓸 수 있습니다.' }),
      el('code', { textContent: `${location.origin}${p.whip_path}` }),
    );
  });

  const end = el('button', { type: 'button', className: 'danger', textContent: '방송 종료' });
  end.onclick = action(end, error, async () => {
    if (!confirm('방송을 종료할까요? 종료한 방송은 다시 송출할 수 없습니다.')) return;
    onChange(await endBroadcast(b.id));
  });
  const del = el('button', { type: 'button', className: 'danger', textContent: '삭제' });
  del.onclick = action(del, error, async () => {
    if (!confirm('방송을 삭제할까요? 되돌릴 수 없습니다.')) return;
    await deleteBroadcast(b.id);
    go('#/');
  });

  // 종료된 방송은 다시 송출할 수 없으니 송출 주소·종료 버튼을 숨기고 안내한다(#27 V15)
  const endedNote = el('p', { className: 'hint', textContent: '종료된 방송은 다시 송출할 수 없습니다. 새로 방송하려면 방송을 새로 만드세요.' });
  const update = (nb: Broadcast) => {
    b = nb;
    const ended = nb.status === 'ended';
    showPublish.hidden = ended;
    end.hidden = ended;
    endedNote.hidden = !ended;
    if (ended) publishBox.replaceChildren();
  };
  const panel = el(
    'section',
    { className: 'panel owner' },
    el('h3', { textContent: '방송 관리 (소유자)' }),
    title,
    description,
    el('div', { className: 'row' }, save, showPublish, end, del),
    endedNote,
    publishBox,
    error,
  );
  update(b);
  return { el: panel, update };
}

// ---------- 길 찾기 ----------

/** 화면을 바꿀 때마다 1씩 는다. 늦게 끝난 이전 화면의 작업이 지금 화면을 건드리지 않게 비교한다. */
let routeGen = 0;

async function route() {
  const gen = ++routeGen;
  cleanup.forEach((f) => f());
  cleanup = [];
  try {
    me = await currentUser();
  } catch {
    me = null;
  }
  if (gen !== routeGen) return;
  const main = el('main', { className: 'content' });
  const status = el('p', { className: 'status' });
  app.replaceChildren(header(), main, status);
  fetch('/api/health')
    .then((r) => r.json())
    .then((h: { status: string; db: string }) => (status.textContent = `서버 ${h.status}, DB ${h.db}`))
    .catch(() => (status.textContent = '백엔드에 연결하지 못했습니다'));

  const hash = location.hash || '#/';
  const detail = /^#\/b\/(\d+)$/.exec(hash);
  if (hash === '#/login') loginPage(main);
  else if (hash === '#/new') newPage(main);
  else if (detail) await detailPage(main, Number(detail[1]), gen);
  else await listPage(main);
}

window.addEventListener('hashchange', route);
route();
