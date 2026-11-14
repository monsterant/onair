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
  getSummary,
  type Clip,
  listClips,
  createClip,
  getClip,
  deleteClip,
  type Comment,
  type Recording,
  getRecording,
  listBroadcastRecordings,
  listRecordings,
  type Status,
  confirmDonation,
  createDonation,
  listDonations,
  chatStreamUrl,
  createComment,
  sendChat,
  type ChatMessage,
  banUser,
  listBans,
  setSlowMode,
  setNotice,
  getBannedWords,
  setBannedWords,
  unbanUser,
  deleteComment,
  listComments,
  updateComment,
  type User,
  createBroadcast,
  currentUser,
  deleteBroadcast,
  guestLogin,
  getConfig,
  endBroadcast,
  getBroadcast,
  listBroadcasts,
  CATEGORIES,
  categoryLabel,
  type Category,
  listFollows,
  followUser,
  unfollowUser,
  login,
  logout,
  publishInfo,
  rotateStreamKey,
  signup,
  updateBroadcast,
  setSchedule,
} from './api';
// 플레이어(OvenPlayer+hls.js, 약 1MB)는 상세 화면에서만 불러온다. 목록 화면을 가볍게 하려는 것이다.
const player = () => import('./player');
import { mountWidget } from './toss';
import {
  CancelledError,
  type Facing,
  Publisher,
  type PublishState,
  cameraAndMic,
  cameraCount,
  keepScreenOn,
  message as deviceMessage,
  otherCamera,
  sameCamera,
  screenAndMic,
} from './publisher';

const app = document.querySelector<HTMLElement>('#app')!;
let me: User | null = null;
/** 화면을 바꿀 때 지울 것(주기적 새로고침, 플레이어) */
let cleanup: (() => void)[] = [];
/** 로그인 화면에 오기 직전에 보던 화면. 로그인하면 그리로 돌아간다(심사 시뮬레이션 2차) */
let backAfterLogin = '#/';

/**
 * 칸 안 영상의 실제 비율에 맞춰 칸 크기를 바꾼다(#67, #34 M10: 휴대폰 세로 영상이 16:9 칸 가운데에 작게 보이던 문제).
 * 영상 크기를 알게 될 때(loadedmetadata)와 바뀔 때(resize: 송출 중 휴대폰을 돌린 경우)마다 맞춘다.
 * 두 이벤트는 위로 퍼지지 않아서(bubble 안 함) 캡처 단계로 듣는다. 그래야 플레이어가 안에 새로 만든 video도 잡힌다.
 */
function fitToVideo(box: HTMLElement): HTMLElement {
  const fit = (ev: Event) => {
    const v = ev.target;
    if (!(v instanceof HTMLVideoElement) || !v.videoWidth || !v.videoHeight) return;
    box.style.setProperty('--vw', String(v.videoWidth));
    box.style.setProperty('--vh', String(v.videoHeight));
    box.classList.add('fitted');
  };
  box.addEventListener('loadedmetadata', fit, true);
  box.addEventListener('resize', fit, true);
  return box;
}

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

/** "N명 시청 중"(#85). 방송 중이고 서버가 셀 수 있을 때만, 아니면 빈 글자 */
function viewersText(b: Broadcast): string {
  return b.status === 'live' && typeof b.viewers === 'number' ? `${b.viewers}명 시청 중` : '';
}

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
  right.append(el('a', { href: '#/recordings', className: 'button ghost', textContent: '다시보기' }));
  if (me) {
    right.append(
      el('a', { href: '#/new', className: 'button', textContent: '방송 만들기' }),
      el('span', { className: 'username', textContent: `${me.username}님` }),
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
  if (!me) {
    // 처음 온 사람(심사자)이 무엇을 할지 바로 알 수 있게 한 줄 소개와 시작 버튼
    const err = el('p', { className: 'error', role: 'alert' });
    const start = el('button', { type: 'button', className: 'primary', textContent: '테스트 계정으로 바로 시작' });
    start.onclick = action(start, err, async () => {
      me = await guestLogin();
      go('#/new');
    });
    main.append(
      el(
        'section',
        { className: 'panel intro' },
        el('h2', { textContent: '브라우저만으로 방송하고, 누구나 바로 시청하세요' }),
        el('p', { className: 'hint', textContent: 'PC 화면 공유·휴대폰 카메라로 방송하고, 시청은 로그인 없이 됩니다. 댓글·후원(테스트 결제)·다시보기도 있습니다.' }),
        el('div', { className: 'row' }, start, el('a', { href: '#/login', className: 'button ghost', textContent: '로그인 · 회원가입' })),
        err,
      ),
    );
  }
  // 즐겨찾는 방송자(#102): 로그인했고 1명 이상일 때만. 방송 중이면 그 방송으로 가는 링크.
  const favs = el('section', { className: 'panel follows' });
  favs.hidden = true;
  const loadFavs = async () => {
    if (!me) return;
    try {
      const list = await listFollows();
      favs.replaceChildren(
        el('h3', { textContent: '즐겨찾는 방송자' }),
        el(
          'ul',
          {},
          ...list.map((f) =>
            el(
              'li',
              {},
              el('strong', { textContent: f.username }),
              ' ',
              f.live
                ? el('a', { href: `#/b/${f.live.id}`, textContent: `방송 중: ${f.live.title}` })
                : el('span', { className: 'hint', textContent: '쉬는 중' }),
            ),
          ),
        ),
      );
      favs.hidden = list.length === 0;
    } catch {
      // 즐겨찾기를 못 읽으면(다른 탭에서 로그아웃 등) 칸을 숨긴다. 방송 목록은 그대로 둔다(#103)
      favs.hidden = true;
    }
  };
  main.append(favs);
  // 곧 시작(#114): 목록 중 대기 중이고 예정 시각이 있는 방송을 시각 순으로. 지났으면 "늦어지는 중"
  // 예정이 1시간 넘게 지났는데 시작하지 않은 방송은 칸에서 뺀다(#136). 방송은 아래 목록에 그대로 있다.
  const upcoming = el('section', { className: 'panel upcoming' });
  upcoming.hidden = true;
  const STALE_MS = 60 * 60 * 1000;
  const showUpcoming = (items: Broadcast[]) => {
    const list = items
      .filter((b) => b.status === 'ready' && b.scheduled_at && Date.parse(b.scheduled_at) > Date.now() - STALE_MS)
      .sort((a, b) => Date.parse(a.scheduled_at!) - Date.parse(b.scheduled_at!));
    upcoming.replaceChildren(
      el('h3', { textContent: '곧 시작' }),
      el(
        'ul',
        {},
        ...list.map((b) => {
          const late = Date.parse(b.scheduled_at!) < Date.now();
          return el(
            'li',
            {},
            el('a', { href: `#/b/${b.id}`, textContent: b.title }),
            el('span', { className: 'hint', textContent: ` ${b.owner_username} · ${shortTime(b.scheduled_at!)}${late ? ' · 늦어지는 중' : ''}` }),
          );
        }),
      ),
    );
    upcoming.hidden = list.length === 0;
  };
  main.append(upcoming);
  main.append(el('h2', { textContent: '방송 목록' }));
  // 제목 검색(#96). 치고 잠깐 멈추면 찾는다. 10초마다 새로 읽을 때도 검색어를 지킨다.
  const search = el('input', { type: 'search', className: 'search', placeholder: '방송 제목 검색', maxLength: 50, ariaLabel: '방송 제목 검색' });
  main.append(search);
  // 분류로 걸러 보기(#113). 고른 버튼은 aria-pressed로 표시
  let cat = '';
  const catButtons = [['', '전체'] as const, ...CATEGORIES].map(([v, l]) => {
    const btn = el('button', { type: 'button', className: 'small ghost category-filter', textContent: l });
    btn.dataset.category = v;
    btn.setAttribute('aria-pressed', String(v === cat));
    btn.onclick = () => {
      cat = v;
      for (const x of catButtons) x.setAttribute('aria-pressed', String(x.dataset.category === cat));
      void load();
    };
    return btn;
  });
  main.append(el('div', { className: 'row category-filters' }, ...catButtons));
  const box = el('div', { className: 'list', textContent: '불러오는 중...' });
  main.append(box);
  // 늦게 온 예전 응답이 새 검색 결과를 덮지 않게 순서 번호를 붙인다
  let seq = 0;
  const load = async () => {
    const my = ++seq;
    const q = search.value.trim();
    try {
      const items = await listBroadcasts(q, cat);
      if (my !== seq) return;
      showUpcoming(items);
      if (items.length === 0) {
        box.replaceChildren(el('p', { className: 'hint', textContent: q || cat ? '찾는 방송이 없습니다.' : '아직 방송이 없습니다.' }));
        return;
      }
      box.replaceChildren(
        ...items.map((b) =>
          el(
            'a',
            { className: 'card', href: `#/b/${b.id}` },
            el('div', { className: 'card-top' }, badge(b.status), el('strong', { textContent: b.title })),
            el('span', { className: 'hint', textContent: [categoryLabel(b.category), b.owner_username, viewersText(b)].filter(Boolean).join(' · ') }),
          ),
        ),
      );
    } catch (e) {
      if (my !== seq) return;
      box.replaceChildren(el('p', { className: 'error', textContent: message(e) }));
    }
  };
  let typing: ReturnType<typeof setTimeout> | undefined;
  search.oninput = () => {
    clearTimeout(typing);
    typing = setTimeout(() => void load(), 300);
  };
  await Promise.all([load(), loadFavs()]);
  const timer = setInterval(() => {
    void load();
    void loadFavs();
  }, 10_000);
  cleanup.push(() => {
    clearInterval(timer);
    clearTimeout(typing);
  });
}

// ---------- 로그인 ----------

function loginPage(main: HTMLElement) {
  if (me) {
    main.append(el('p', { textContent: `${me.username}님, 로그인되어 있습니다.` }));
    return;
  }
  const username = el('input', { name: 'username', autocomplete: 'username', placeholder: '아이디 (영문·숫자·_ 3~20자)', ariaLabel: '아이디', required: true });
  const password = el('input', { name: 'password', type: 'password', autocomplete: 'current-password', placeholder: '비밀번호 (8자 이상)', ariaLabel: '비밀번호', required: true });
  const error = el('p', { className: 'error', role: 'alert' });
  const done = (u: User) => {
    me = u;
    go(backAfterLogin);
  };
  const loginBtn = el('button', { type: 'submit', textContent: '로그인' });
  const signupBtn = el('button', { type: 'button', textContent: '회원가입' });
  signupBtn.onclick = action(signupBtn, error, async () => done(await signup(username.value, password.value)));
  const form = el('form', { className: 'auth panel' }, el('h2', { textContent: '로그인' }), username, password, el('div', { className: 'row' }, loginBtn, signupBtn), error);
  form.onsubmit = (ev) => {
    ev.preventDefault();
    action(loginBtn, error, async () => done(await login(username.value, password.value)))();
  };

  // 심사자마다 따로 쓰는 테스트 계정(#35). 공용 계정이면 방송 수 한도·송출 키·삭제가 심사자끼리 섞인다.
  const demoError = el('p', { className: 'error', role: 'alert' });
  const guestBtn = el('button', { type: 'button', className: 'primary', textContent: '테스트 계정으로 바로 시작' });
  guestBtn.onclick = action(guestBtn, demoError, async () => done(await guestLogin()));
  const demo = el(
    'section',
    { className: 'demo panel' },
    el('h3', { textContent: '테스트 계정으로 바로 시작' }),
    el('p', { className: 'hint', textContent: '가입 없이 누를 때마다 새 테스트 계정이 만들어집니다. 테스트 계정은 24시간 뒤 지워집니다.' }),
    el('p', { className: 'hint', textContent: '다른 사용자 권한(남의 방송 수정·삭제 거절)을 확인하려면 시크릿 창이나 다른 브라우저에서 한 번 더 누르세요.' }),
    el('div', { className: 'row' }, guestBtn),
    demoError,
  );
  // 서버에서 테스트 계정 기능이 꺼져 있으면 버튼 칸을 숨긴다(#25 A7). 물어보지 못하면 그대로 둔다.
  getConfig()
    .then((c) => (demo.hidden = !c.demo_login))
    .catch(() => {});
  main.append(form, demo);
}

// ---------- 만들기 ----------

/** 분류 고르기(#113) */
function categorySelect(value: string): HTMLSelectElement {
  const s = el('select', { name: 'category', className: 'category-select', ariaLabel: '분류' }, ...CATEGORIES.map(([v, l]) => el('option', { value: v, textContent: `분류: ${l}` })));
  s.value = value;
  return s;
}

function newPage(main: HTMLElement) {
  if (!me) {
    main.append(el('p', {}, '방송을 만들려면 ', el('a', { href: '#/login', textContent: '로그인' }), '하세요.'));
    return;
  }
  const title = el('input', { name: 'title', placeholder: '방송 제목 (100자 이하)', maxLength: 100, ariaLabel: '방송 제목', required: true });
  const description = el('textarea', { name: 'description', placeholder: '설명 (1000자 이하, 선택)', maxLength: 1000, rows: 4, ariaLabel: '방송 설명' });
  const category = categorySelect('etc');
  const error = el('p', { className: 'error', role: 'alert' });
  const submit = el('button', { type: 'submit', textContent: '만들기' });
  const form = el('form', { className: 'panel', noValidate: true }, el('h2', { textContent: '방송 만들기' }), title, description, category, submit, error);
  form.onsubmit = (ev) => {
    ev.preventDefault();
    action(submit, error, async () => {
      const b = await createBroadcast(title.value, description.value, category.value as Category);
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
    // 없는 방송이면 서버 문구 대신 무슨 일인지 알려 준다(#136)
    const text = e instanceof ApiError && e.status === 404 ? '없는 방송입니다. 지워졌거나 주소가 틀렸습니다.' : message(e);
    main.append(el('p', { className: 'error', textContent: text }), el('a', { href: '#/', textContent: '방송 목록으로' }));
    // 인터넷이 끊긴 채로 열었으면, 다시 연결될 때 이 화면을 다시 연다(#63 공격 사냥)
    if (!(e instanceof ApiError)) {
      const again = () => {
        if (gen === routeGen) void route();
      };
      window.addEventListener('online', again, { once: true });
      // online 알림 직후에는 아직 인터넷이 안 될 수 있다. 5초마다도 다시 연다(#63 2바퀴)
      const later = setTimeout(again, 5_000);
      cleanup.push(() => {
        window.removeEventListener('online', again);
        clearTimeout(later);
      });
    }
    return;
  }
  // 기다리는 사이 다른 화면으로 옮겼으면 아무것도 하지 않는다(늦게 끝난 요청이 새 화면을 건드리지 않게, #27 V17)
  if (gen !== routeGen) return;
  const isOwner = me?.id === b.owner_id;

  const statusBox = el('span');
  const titleEl = el('h2');
  // 시작 예정 시각 안내(#114). 대기 중이고 예정이 있을 때만
  const scheduleNote = el('p', { className: 'hint schedule-note' });
  const descEl = el('p', { className: 'description' });
  const moreBtn = el('button', { type: 'button', className: 'small more', textContent: '더보기', hidden: true });
  moreBtn.onclick = () => {
    const folded = descEl.classList.toggle('folded');
    moreBtn.textContent = folded ? '더보기' : '접기';
  };
  const playerBox = fitToVideo(el('div', { className: 'player-box' }));
  const offAir = el('p', { className: 'hint' });
  // 브라우저는 소리 있는 자동 재생을 막아 음소거로 시작한다. 켜는 방법이 눈에 띄게 버튼을 둔다(심사 시뮬레이션: "소리가 안 난다"로 보임)
  const soundBtn = el('button', { type: 'button', className: 'sound-toggle', textContent: '🔇 소리 켜기', hidden: true });
  soundBtn.onclick = () => {
    void player().then((p) => {
      const on = p.toggleMute('player');
      if (on !== null) soundBtn.textContent = on ? '🔊 소리 끄기' : '🔇 소리 켜기';
    });
  };
  // 소유자가 이 화면에서 송출하는 동안에는 자기 방송을 플레이어로 다시 보지 않는다(미리보기가 따로 있다)
  const selfPublishing = () => !!publisher && publisher.state !== 'idle';
  let playing = false;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let gone = false;

  // 시청자 인터넷 끊김 알림(#63). 끊기면 알리고, 다시 연결되면 멈춘 플레이어를 새로 만든다.
  const netNote = el('p', { className: 'net-note', role: 'status', hidden: true });
  let down = false;
  // 다시 연결된 뒤 플레이어를 새로 만들어야 하는지. 지우고 만드는 일은 paint() 한 곳에서 한다(순서가 꼬이지 않게, #63 리뷰)
  let restart = false;
  // 상태 확인이 서버에 닿지 못한 연속 횟수. 한 번은 잠깐 흔들린 것일 수 있어 2번(약 10초)부터 알린다.
  let failures = 0;
  const setDown = (d: boolean) => {
    if (down === d) return;
    down = d;
    netNote.hidden = !d;
    // 송출하는 소유자에게는 시청 문구가 맞지 않는다(#63 공격 사냥)
    netNote.textContent = selfPublishing()
      ? '인터넷 연결이 끊겼습니다. 연결이 돌아오면 송출 상태를 확인해 주세요.'
      : '인터넷 연결이 끊겼습니다. 다시 연결되면 자동으로 이어서 봅니다.';
    // 끊긴 동안 멈춘 플레이어는 스스로 돌아오지 않을 수 있다. 이어지는 paint()가 방송 중이면 새로 만든다.
    if (!d && playing) restart = true;
  };

  // 재생 오류가 나면 3초 뒤 플레이어를 다시 만든다(#27 V2)
  const onPlayerError = () => {
    clearTimeout(retry);
    retry = setTimeout(() => {
      // 끊긴 동안에는 다시 만들지 않는다. 다시 연결되면 setDown(false)가 새로 만든다.
      if (gen !== routeGen || gone || down || b.status !== 'live') return;
      playing = false;
      paint();
    }, 3_000);
  };

  const paint = () => {
    statusBox.replaceChildren(badge(b.status));
    // 지금 보는 사람 수(#85). 상태 확인(5초마다)과 함께 바뀐다
    const vt = viewersText(b);
    if (vt) statusBox.append(el('span', { className: 'viewers', textContent: vt }));
    titleEl.textContent = b.title;
    scheduleNote.textContent =
      b.status === 'ready' && b.scheduled_at
        ? `시작 예정: ${shortTime(b.scheduled_at)}${Date.parse(b.scheduled_at) < Date.now() ? ' · 늦어지는 중' : ''}`
        : '';
    scheduleNote.hidden = !scheduleNote.textContent;
    if (descEl.textContent !== b.description) {
      descEl.textContent = b.description;
      // 긴 설명은 접어 둔다(휴대폰에서 방송하기·댓글까지 한참 내려가야 하던 문제)
      const long = b.description.length > 200 || b.description.split('\n').length > 4;
      descEl.classList.toggle('folded', long);
      // 글자가 많아도 넓은 화면에서 접힌 높이 안에 다 들어가면 접지 않는다(리뷰 지적 3)
      const fits = long && descEl.isConnected && descEl.scrollHeight <= descEl.clientHeight + 1;
      if (fits) descEl.classList.remove('folded');
      moreBtn.hidden = !long || fits;
      moreBtn.textContent = '더보기';
    }
    const live = b.status === 'live' && !selfPublishing();
    if (live && (!playing || restart)) {
      // startPlayer는 같은 칸의 옛 플레이어를 먼저 지운다
      clearTimeout(retry);
      restart = false;
      playerBox.replaceChildren(el('div', { id: 'player' }));
      // 옛 영상의 비율이 새 플레이어로 이어지지 않게 지운다. 새 영상 크기를 알면 다시 맞춘다(#68 R5).
      playerBox.classList.remove('fitted');
      soundBtn.hidden = false;
      soundBtn.textContent = '🔇 소리 켜기';
      player().then((p) => {
        if (gen === routeGen && b.status === 'live') p.startPlayer('player', b.stream_name, onPlayerError);
      });
      playing = true;
    } else if (!live && playing) {
      restart = false;
      clearTimeout(retry);
      player().then((p) => p.stopPlayer('player'));
      playerBox.replaceChildren();
      soundBtn.hidden = true;
      playing = false;
    }
    offAir.textContent =
      b.status === 'live'
        ? selfPublishing()
          ? '지금 이 화면에서 송출하고 있습니다. 시청자 화면은 다른 창(시크릿 창)에서 확인하세요.'
          : ''
        : b.status === 'ended'
          ? '종료된 방송입니다.'
          : b.started_at
            ? '방송이 잠시 멈췄습니다. 다시 시작되면 자동으로 재생됩니다.'
            : '아직 방송 전입니다. 방송이 시작되면 자동으로 재생됩니다.';
    owner?.update(b);
    studio?.update(b);
  };

  // 이 화면에서 쓰는 송출기(소유자만). 화면을 떠나면 송출을 멈춘다.
  const publisher = isOwner ? new Publisher() : null;
  // 송출 연결이 끊기면 서버가 이 방송을 아직 "방송 중"으로 보는지 묻는다(#136). 못 물어보면 예전처럼 기다린다.
  // 종료된 방송은 다시 시작할 수 없으니 문구를 나눈다(#136 리뷰).
  if (publisher)
    publisher.serverStopped = async () => {
      const s = (await getBroadcast(id)).status;
      if (s === 'live') return null;
      return s === 'ended' ? '방송이 종료돼 송출을 멈췄습니다' : '서버에서 송출이 끊겼습니다. 다시 시작할 수 있습니다.';
    };
  const owner = publisher ? ownerPanel(b, (nb) => { b = nb; paint(); }, publisher) : null;
  const studio = publisher ? studioPanel(() => b, publisher) : null;
  main.append(
    el('div', { className: 'detail-head' }, statusBox, titleEl),
    el('p', { className: 'hint owner-line' }, `방송자: ${b.owner_username} `, followButton(b.owner_id, b.owner_username)),
    scheduleNote,
    playerBox,
    soundBtn,
    netNote,
    offAir,
  );
  // 소유자는 방송하기 칸(시작 버튼·미리보기)을 채팅보다 먼저 본다(#124: 채팅 칸에 밀려 첫 화면 밖에 있었다)
  if (studio) main.append(studio.el);
  main.append(chatPanel(id, () => b));
  // 송출이 시작·멈추면 방송 상태를 바로 다시 읽어 위쪽 표시(대기/방송 중)를 맞춘다. 5초 새로고침을 기다리지 않는다.
  if (publisher) {
    const showStudio = publisher.onState;
    publisher.onState = (s, d) => {
      showStudio(s, d);
      if (s === 'live' || s === 'idle') {
        setTimeout(() => {
          if (gen !== routeGen || gone) return;
          void getBroadcast(id)
            .then((nb) => {
              b = nb;
              if (gen === routeGen) paint();
            })
            .catch(() => {});
        }, 800);
      }
    };
  }
  main.append(descEl, moreBtn);
  if (owner) main.append(owner.el);
  main.append(recordingsPanel(id), donationsPanel(b, gen), commentsPanel(id, gen, b.owner_id));
  paint();

  // 5초마다 상태를 다시 본다(방송이 시작·종료되면 플레이어를 켜고 끈다)
  const onOffline = () => setDown(true);
  // 다시 연결되면 5초를 기다리지 않고 바로 확인한다. 성공하면 알림을 지우고 플레이어를 새로 만든다.
  const onOnline = () => void refresh();
  window.addEventListener('offline', onOffline);
  window.addEventListener('online', onOnline);
  const stop = () => {
    clearInterval(timer);
    clearTimeout(retry);
    window.removeEventListener('offline', onOffline);
    window.removeEventListener('online', onOnline);
    player().then((p) => p.stopPlayer('player'));
  };
  // 상태 확인이 겹쳐 돌 수 있다(5초 차례 + 다시 연결). 늦게 도착한 옛 결과는 버린다(#63 리뷰)
  let asked = 0;
  let applied = 0;
  const refresh = async () => {
    const mine = ++asked;
    try {
      const nb = await getBroadcast(id);
      if (gen !== routeGen || gone || mine < applied) return;
      applied = mine;
      b = nb;
      failures = 0;
      // offline 알림 직후에 도착한 응답이면 아직 끊긴 것으로 둔다
      if (navigator.onLine) setDown(false);
      paint();
    } catch (e) {
      if (gen !== routeGen || gone || mine < applied) return;
      applied = mine;
      // 서버에 닿지 못함(fetch 자체 실패). 서버가 답한 오류(ApiError)는 인터넷이 살아 있다는 뜻이다.
      if (!(e instanceof ApiError)) {
        failures += 1;
        if (failures >= 2 || !navigator.onLine) setDown(true);
        return;
      }
      failures = 0;
      if (navigator.onLine) setDown(false);
      // 끊긴 동안 멈춘 플레이어를 바로 새로 만든다(성공 응답을 기다리지 않게, #63 2바퀴)
      if (restart && e.status !== 404) paint();
      // 방송이 지워졌으면 멈추고 알린다(#27 V10). 그 밖의 오류는 잠깐 끊긴 것으로 보고 다음에 다시 본다.
      if (e instanceof ApiError && e.status === 404) {
        gone = true;
        stop();
        // 이 화면에서 송출 중이었으면 화면 공유·마이크도 바로 끈다(#30 P3)
        void publisher?.stop('방송이 삭제됐습니다');
        main.replaceChildren(el('p', { className: 'hint', textContent: '삭제된 방송입니다.' }), el('a', { href: '#/', textContent: '목록으로' }));
      }
    }
  };
  const timer = setInterval(() => void refresh(), 5_000);
  if (!navigator.onLine) setDown(true);
  cleanup.push(stop);
  if (publisher) cleanup.push(() => void publisher.stop());
}

// ---------- 녹화·다시보기(#47) ----------

/** 초를 '1시간 2분 3초' 꼴로(#110) */
function clock(total: number): string {
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return [h ? `${h}시간` : '', h || m ? `${m}분` : '', `${s}초`].filter(Boolean).join(' ');
}

/** 걸린 시간 "12분 3초" */
function duration(r: Recording): string {
  if (!r.ended_at) return '';
  const s = Math.max(0, Math.round((new Date(r.ended_at).getTime() - new Date(r.started_at).getTime()) / 1000));
  const m = Math.floor(s / 60);
  return m > 0 ? `${m}분 ${s % 60}초` : `${s}초`;
}

function replayCard(r: Recording): HTMLElement {
  return el(
    'a',
    { className: 'card', href: `#/r/${r.id}` },
    el('div', { className: 'card-top' }, el('span', { className: 'badge ended', textContent: '다시보기' }), el('strong', { textContent: r.title })),
    el('span', { className: 'hint', textContent: `${r.owner_username} · ${shortTime(r.started_at)} · ${duration(r)}` }),
  );
}

/** 다시보기 목록(#/recordings). 누구나 본다. */
async function recordingsPage(main: HTMLElement, gen: number) {
  main.append(
    el('h2', { textContent: '다시보기' }),
    el('p', { className: 'hint', textContent: '방송이 끝나면 녹화가 여기에 올라옵니다(끝난 뒤 30초쯤 걸립니다).' }),
  );
  const box = el('div', { className: 'list', textContent: '불러오는 중...' });
  main.append(box);
  try {
    const items = await listRecordings();
    if (gen !== routeGen) return;
    box.replaceChildren(...(items.length ? items.map(replayCard) : [el('p', { className: 'hint', textContent: '아직 다시보기가 없습니다.' })]));
  } catch (e) {
    box.replaceChildren(el('p', { className: 'error', textContent: message(e) }));
  }
}

/** 다시보기 상세(#/r/{id}): 녹화 영상 재생 */
async function replayPage(main: HTMLElement, id: number, gen: number) {
  let r: Recording;
  try {
    r = await getRecording(id);
  } catch (e) {
    if (gen !== routeGen) return;
    main.append(el('p', { className: 'error', textContent: message(e) }), el('a', { href: '#/recordings', textContent: '다시보기 목록으로' }));
    return;
  }
  // 기다리는 사이 다른 화면으로 옮겼으면 영상을 만들지 않는다(#47 리뷰)
  if (gen !== routeGen) return;
  const video = el('video', { controls: true, preload: 'metadata', playsInline: true, className: 'replay' });
  video.src = `/api${r.url}`;
  main.append(
    el('div', { className: 'detail-head' }, el('span', { className: 'badge ended', textContent: '다시보기' }), el('h2', { textContent: r.title })),
    el('p', { className: 'hint', textContent: `방송자: ${r.owner_username} · ${shortTime(r.started_at)} · ${duration(r)} · ${(r.bytes / 1024 / 1024).toFixed(1)}MB` }),
    fitToVideo(el('div', { className: 'player-box' }, video)),
    el('p', {}, el('a', { href: `#/b/${r.broadcast_id}`, textContent: '방송 화면으로' }), ' · ', el('a', { href: '#/recordings', textContent: '다시보기 목록' })),
    clipsPanel(r, video),
  );
  // 화면을 떠나면 영상 내려받기를 멈춘다
  cleanup.push(() => {
    video.pause();
    video.removeAttribute('src');
    video.load();
  });
}

/** 초를 "1:05" 꼴로(#111) */
function mmss(sec: number): string {
  const s = Math.max(0, Math.floor(sec));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** 다시보기 화면의 장면 칸(#111): 장면 목록, 로그인했으면 지금 위치로 장면 저장 */
function clipsPanel(r: Recording, video: HTMLVideoElement): HTMLElement {
  const list = el('ul', { className: 'clip-list' });
  const error = el('p', { className: 'error', role: 'alert' });
  const load = async () => {
    const clips = await listClips(r.id).catch(() => [] as Clip[]);
    list.replaceChildren(
      ...clips.map((c) => {
        const li = el('li', {}, el('a', { href: `#/c/${c.id}`, textContent: c.title }), el('span', { className: 'hint', textContent: ` ${mmss(c.start)}~${mmss(c.end)} · ${c.username}` }));
        if (me && me.id === c.user_id) {
          const del = el('button', { type: 'button', className: 'small ghost', textContent: '지우기' });
          del.onclick = action(del, error, async () => {
            await deleteClip(c.id);
            await load();
          });
          li.append(' ', del);
        }
        return li;
      }),
    );
    if (clips.length === 0) list.append(el('li', { className: 'hint', textContent: '아직 저장한 장면이 없습니다.' }));
  };
  void load();
  const panel = el('section', { className: 'panel clips' }, el('h3', { textContent: '장면' }), list);
  if (!me) {
    panel.append(el('p', { className: 'hint' }, el('a', { href: '#/login', textContent: '로그인' }), '하면 장면을 저장할 수 있습니다.'));
    return panel;
  }
  // 지금 재생 위치를 시작으로, 기본 30초(다시보기 끝을 넘지 않게). 장면은 2분까지.
  const title = el('input', { type: 'text', placeholder: '장면 제목 (50자 이하)', maxLength: 50, ariaLabel: '장면 제목' });
  const start = el('input', { type: 'number', min: '0', step: '0.1', ariaLabel: '장면 시작(초)', className: 'clip-start' });
  const end = el('input', { type: 'number', min: '0', step: '0.1', ariaLabel: '장면 끝(초)', className: 'clip-end' });
  const fromNow = el('button', { type: 'button', className: 'small', textContent: '지금 위치부터 30초' });
  fromNow.onclick = () => {
    const s = video.currentTime || 0;
    const max = Number.isFinite(video.duration) ? video.duration : s + 30;
    start.value = s.toFixed(1);
    end.value = Math.min(s + 30, max).toFixed(1);
  };
  const save = el('button', { type: 'button', textContent: '장면 저장' });
  save.onclick = action(save, error, async () => {
    await createClip(r.id, title.value, Number(start.value), Number(end.value));
    title.value = '';
    await load();
  });
  panel.append(
    el('div', { className: 'row clip-form' }, title, el('span', { className: 'hint', textContent: '시작(초)' }), start, el('span', { className: 'hint', textContent: '끝(초)' }), end, fromNow, save),
    el('p', { className: 'hint', textContent: '장면은 2분까지입니다. 영상을 자르지 않고 재생 위치만 저장합니다.' }),
    error,
  );
  return panel;
}

/** 장면 화면(#/c/{id}, #111): 그 구간만 재생하고 끝에서 멈춘다 */
async function clipPage(main: HTMLElement, id: number, gen: number) {
  let c: Awaited<ReturnType<typeof getClip>>;
  try {
    c = await getClip(id);
  } catch (e) {
    if (gen !== routeGen) return;
    const text = e instanceof ApiError && e.status === 404 ? '없는 장면입니다. 지워졌거나 주소가 틀렸습니다.' : message(e);
    main.append(el('p', { className: 'error', textContent: text }), el('a', { href: '#/recordings', textContent: '다시보기 목록으로' }));
    return;
  }
  if (gen !== routeGen) return;
  const video = el('video', { controls: true, preload: 'metadata', playsInline: true, className: 'replay clip-video' });
  // 브라우저의 구간 재생 문법(#t=시작,끝)도 함께 써서 처음부터 그 위치를 받게 한다
  video.src = `/api${c.recording.url}#t=${c.start},${c.end}`;
  const toStart = () => {
    video.currentTime = c.start;
  };
  video.addEventListener('loadedmetadata', toStart, { once: true });
  // 끝에 닿으면 멈춘다(사용자가 끝 뒤로 넘긴 경우도)
  video.addEventListener('timeupdate', () => {
    if (video.currentTime >= c.end && !video.paused) {
      video.pause();
      video.currentTime = c.end;
    }
  });
  const again = el('button', { type: 'button', className: 'small', textContent: '장면 다시 보기' });
  again.onclick = () => {
    toStart();
    void video.play().catch(() => {});
  };
  main.append(
    el('div', { className: 'detail-head' }, el('span', { className: 'badge ended', textContent: '장면' }), el('h2', { textContent: c.title })),
    el('p', { className: 'hint', textContent: `${c.recording.title} · ${mmss(c.start)}~${mmss(c.end)} · 저장한 사람 ${c.username}` }),
    fitToVideo(el('div', { className: 'player-box' }, video)),
    el('p', {}, again, ' ', el('a', { href: `#/r/${c.recording_id}`, textContent: '전체 다시보기' })),
  );
  cleanup.push(() => {
    video.pause();
    video.removeAttribute('src');
    video.load();
  });
}

/** 방송 상세 화면의 "이 방송의 다시보기" 칸. 없으면 숨긴다. */
function recordingsPanel(broadcastId: number): HTMLElement {
  const list = el('div', { className: 'list' });
  const panel = el('section', { className: 'recordings-panel panel', hidden: true }, el('h3', { textContent: '이 방송의 다시보기' }), list);
  listBroadcastRecordings(broadcastId)
    .then((items) => {
      if (items.length === 0) return;
      list.replaceChildren(...items.map(replayCard));
      panel.hidden = false;
    })
    .catch(() => {});
  return panel;
}

// ---------- 도네이션(#41) ----------

const won = (n: number) => `${n.toLocaleString('ko-KR')}원`;
const AMOUNTS = [1_000, 5_000, 10_000, 50_000];

/**
 * 방송자 즐겨찾기 버튼(#102). 로그인했고 내 방송이 아닐 때만 보인다. 지금 상태는 내 즐겨찾기 목록에서 읽는다.
 */
function followButton(ownerId: number, ownerName: string): HTMLElement | string {
  if (!me || me.id === ownerId) return '';
  const btn = el('button', { type: 'button', className: 'small ghost follow', textContent: '즐겨찾기', disabled: true });
  let on = false;
  const show = () => {
    btn.textContent = on ? '즐겨찾기 해제' : '즐겨찾기';
    btn.title = on ? `${ownerName} 님 즐겨찾기 해제` : `${ownerName} 님 즐겨찾기`;
  };
  void listFollows()
    .then((list) => {
      on = list.some((f) => f.user_id === ownerId);
      show();
    })
    .catch(() => {})
    .finally(() => {
      btn.disabled = false;
    });
  btn.onclick = async () => {
    btn.disabled = true;
    try {
      if (on) await unfollowUser(ownerId);
      else await followUser(ownerId);
      on = !on;
      show();
    } catch (e) {
      alert(message(e));
    } finally {
      btn.disabled = false;
    }
  };
  return btn;
}

/**
 * 방송 상세 화면의 후원 칸. 누구나 합계와 내역을 본다. 로그인한 다른 사용자는 후원한다(자기 방송은 안 됨).
 * 후원하기 → 서버가 주문을 만든다 → 토스 결제위젯 → 결제창 → /donate/success로 돌아와 서버가 승인을 확인한다.
 */
function donationsPanel(b: Broadcast, gen: number): HTMLElement {
  const total = el('strong', { className: 'donation-total' });
  const list = el('ul', { className: 'donations' });
  // 후원 순위(#99): 이 방송에 가장 많이 후원한 사람 5명
  const top = el('ol', { className: 'donor-top', ariaLabel: '후원 순위' });
  top.hidden = true;
  const panel = el('section', { className: 'donations-panel panel' }, el('h3', {}, '후원 ', total), top, list);
  let stopped = false;

  const load = async () => {
    try {
      const s = await listDonations(b.id);
      if (stopped || gen !== routeGen) return;
      total.textContent = `${won(s.total)} · ${s.count}건`;
      top.replaceChildren(
        ...(s.top ?? []).map((t) =>
          el('li', {}, el('strong', { textContent: t.donor_username }), ` ${won(t.total)} (${t.count}번)`),
        ),
      );
      top.hidden = top.childElementCount === 0;
      list.replaceChildren(
        ...s.items.map((d) =>
          el(
            'li',
            { className: 'donation' },
            el('strong', { textContent: d.donor_username }),
            el('span', { className: 'donation-amount', textContent: ` ${won(d.amount)}` }),
            d.message ? el('p', { className: 'comment-body', textContent: d.message }) : '',
          ),
        ),
      );
      if (s.items.length === 0) list.append(el('li', { className: 'hint', textContent: '아직 후원이 없습니다.' }));
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) stopped = true;
    }
  };

  if (!me) {
    panel.append(el('p', { className: 'hint' }, el('a', { href: '#/login', textContent: '로그인' }), '하면 후원할 수 있습니다.'));
  } else if (me.id === b.owner_id) {
    panel.append(el('p', { className: 'hint', textContent: '내 방송에는 후원할 수 없습니다.' }));
  } else {
    const error = el('p', { className: 'error', role: 'alert' });
    const amount = el('select', { ariaLabel: '후원 금액' });
    for (const a of AMOUNTS) amount.append(el('option', { value: String(a), textContent: won(a) }));
    const message = el('input', { type: 'text', placeholder: '응원 메시지 (선택, 100자까지)', ariaLabel: '응원 메시지' });
    const start = el('button', { type: 'button', textContent: '후원하기' });
    const widgetBox = el('div', { className: 'toss-widget', hidden: true }, el('div', { id: 'toss-methods' }), el('div', { id: 'toss-agreement' }));
    const pay = el('button', { type: 'button', textContent: '결제하기', hidden: true });
    const note = el('p', { className: 'hint', textContent: '토스페이먼츠 테스트 결제입니다. 실제로 돈이 나가지 않습니다.' });
    // 토스 SDK가 던지는 오류(결제수단 미선택, 사용자 취소 등)는 그 문구를 그대로 보여 준다
    const tossError = (e: unknown) => new ApiError(0, e instanceof Error && e.message ? e.message : '결제를 진행하지 못했습니다');
    start.onclick = action(start, error, async () => {
      const checkout = await createDonation(b.id, Number(amount.value), message.value);
      widgetBox.hidden = false;
      // 결제위젯이 그려질 때까지 빈 상자로 보이지 않게 안내한다(#136)
      const loading = el('p', { className: 'hint', textContent: '결제 화면을 불러오는 중...' });
      widgetBox.before(loading);
      const w = await mountWidget(checkout, b.id, '#toss-methods', '#toss-agreement')
        .catch((e) => {
          widgetBox.hidden = true;
          throw tossError(e);
        })
        .finally(() => loading.remove());
      // 주문(금액·메시지)이 정해졌으므로 다시 고를 수 없게 하고, 결제하기만 남긴다
      amount.disabled = message.disabled = true;
      start.hidden = true;
      pay.hidden = false;
      pay.onclick = action(pay, error, async () => {
        await w.pay().catch((e) => {
          throw tossError(e);
        });
      });
    });
    panel.append(el('div', { className: 'row' }, amount, start), message, widgetBox, pay, note, error);
  }

  void load();
  const timer = setInterval(() => {
    if (!stopped) void load();
  }, 10_000);
  cleanup.push(() => {
    stopped = true;
    clearInterval(timer);
  });
  return panel;
}

/** 토스 결제창에서 돌아온 화면(/donate/success, /donate/fail). 서버에 승인 확인을 맡기고 방송 화면으로 돌아간다. */
async function donateReturnPage(main: HTMLElement) {
  const q = new URLSearchParams(location.search);
  const bid = Number(q.get('b'));
  const failed = location.pathname === '/donate/fail';
  // 값을 읽자마자 주소를 방송 화면으로 바꾼다(#41 리뷰).
  // - 결제 키가 주소창·방문 기록에 남지 않는다.
  // - 머리말 링크(#/)를 눌러도 이 화면에 다시 갇혀 승인 확인을 또 보내지 않는다(경로가 /donate/...로 남지 않으므로).
  history.replaceState(null, '', Number.isFinite(bid) && bid > 0 ? `/#/b/${bid}` : '/#/');
  const back = () => void route();
  const msg = el('p');
  const backBtn = el('button', { type: 'button', textContent: '방송으로 돌아가기' });
  backBtn.onclick = back;
  main.append(el('section', { className: 'panel' }, el('h2', { textContent: '후원' }), msg, backBtn));
  if (failed) {
    msg.textContent = `결제하지 않았습니다: ${q.get('message') ?? '취소되었습니다'}`;
    return;
  }
  const paymentKey = q.get('paymentKey') ?? '';
  const orderId = q.get('orderId') ?? '';
  const amount = Number(q.get('amount'));
  msg.textContent = '결제를 확인하는 중입니다...';
  try {
    const d = await confirmDonation(paymentKey, orderId, amount);
    msg.textContent = d.status === 'paid' ? `${won(d.amount)} 후원이 완료되었습니다. 고맙습니다!` : '후원을 확인하지 못했습니다.';
  } catch (e) {
    msg.textContent = `후원을 확인하지 못했습니다: ${message(e)}`;
  }
}

// ---------- 댓글(#37) ----------

/** 날짜를 "11. 11. 오후 2:05"처럼 짧게 */
function shortTime(iso: string): string {
  return new Date(iso).toLocaleString('ko-KR', { month: 'numeric', day: 'numeric', hour: 'numeric', minute: '2-digit' });
}

/**
 * 방송 상세 화면 아래 댓글 칸. 누구나 읽고, 로그인한 사용자만 쓴다. 내 댓글에만 수정·삭제 버튼이 있다.
 * 4초마다 목록을 다시 불러온다(폴링).
 * - 바뀌지 않은 댓글과 수정 중인 댓글은 같은 화면 요소를 그대로 둔다. 다시 그리면 수정 칸의 커서(포커스)와
 *   댓글별 오류 문구가 사라지기 때문이다(#37 리뷰).
 * - 늦게 도착한 옛 응답이 새 목록을 덮지 않게 요청마다 번호를 매긴다.
 */
/**
 * 실시간 채팅(#86). 받기는 SSE(EventSource, 끊기면 브라우저가 알아서 다시 붙음), 보내기는 POST.
 * 다시 붙으면 서버가 최근 글을 또 보내므로 id로 겹친 것을 버린다. 글은 textContent로만 넣는다.
 * 댓글(아래 칸)과는 따로 동작한다. 채팅은 서버 메모리에만 있어 서버를 다시 켜면 사라진다.
 */
function chatPanel(broadcastId: number, current: () => Broadcast): HTMLElement {
  const list = el('ul', { className: 'chat-list', ariaLive: 'polite' });
  const state = el('span', { className: 'hint' });
  const error = el('p', { className: 'error', role: 'alert' });
  const seen = new Set<number>();
  const empty = el('li', { className: 'chat-empty hint', textContent: '아직 채팅이 없습니다.' });
  list.append(empty);

  const isOwner = () => !!me && me.id === current().owner_id;
  const add = (m: ChatMessage) => {
    if (seen.has(m.id)) return;
    seen.add(m.id);
    empty.remove();
    // 맨 아래를 보고 있었으면 새 글이 와도 맨 아래에 머문다(위로 올려 읽는 중이면 그대로 둔다)
    const nearBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 40;
    const li =
      m.kind === 'donation'
        ? // 진짜 후원 안내에는 화면이 '후원' 표시를 붙인다. 일반 채팅 글로는 이 표시를 만들 수 없다(#90 R4: 안내 흉내)
          el('li', { className: 'chat-line donation' }, el('span', { className: 'chat-tag', textContent: '후원' }), ' ', el('strong', { textContent: m.username }), ` 님이 ${m.text}`)
        : el('li', { className: 'chat-line' }, el('strong', { textContent: m.username }), ` ${m.text}`);
    li.dataset.id = String(m.id);
    // 방송 주인에게는 남의 채팅 옆에 제한 버튼(#87)
    if (m.kind === 'chat' && m.user_id !== null && isOwner() && m.user_id !== me?.id) {
      li.append(' ', banButtons(broadcastId, m.user_id, m.username, error));
    }
    list.append(li);
    // 너무 많이 쌓이지 않게 앞에서 지운다
    while (list.children.length > 200) {
      const first = list.firstElementChild as HTMLElement | null;
      if (!first) break;
      seen.delete(Number(first.dataset.id));
      first.remove();
    }
    if (nearBottom) list.scrollTop = list.scrollHeight;
  };

  // 연결이 잠깐 끊기면 EventSource가 알아서 다시 붙는다. 하지만 서버가 다시 켜지는 중이라 502 같은 오류로 답하면
  // EventSource는 완전히 닫힌다(CLOSED). 그때는 몇 초 뒤 직접 다시 연다(3초부터 두 배씩, 최대 30초, #86 리뷰).
  // 방송이 지워졌으면(404) 더 열지 않는다.
  let source: EventSource | null = null;
  let closed = false;
  let wait = 3_000;
  let again: ReturnType<typeof setTimeout> | undefined;
  const open = () => {
    if (closed) return;
    const s = new EventSource(chatStreamUrl(broadcastId));
    source = s;
    s.addEventListener('chat', (ev) => {
      try {
        add(JSON.parse((ev as MessageEvent<string>).data) as ChatMessage);
      } catch {
        // 모양이 다른 메시지는 버린다
      }
    });
    s.onopen = () => {
      state.textContent = '';
      wait = 3_000;
    };
    s.onerror = () => {
      if (s.readyState !== EventSource.CLOSED) {
        state.textContent = ' (다시 연결 중…)';
        return;
      }
      s.close();
      if (closed) return;
      void getBroadcast(broadcastId)
        .then(() => {
          state.textContent = ' (다시 연결 중…)';
          again = setTimeout(open, wait);
          wait = Math.min(wait * 2, 30_000);
        })
        .catch((e) => {
          if (e instanceof ApiError && e.status === 404) {
            state.textContent = ' (연결 끊김)';
            return;
          }
          // 서버가 아직 안 떴으면(연결 실패) 조금 뒤 다시 본다
          state.textContent = ' (다시 연결 중…)';
          again = setTimeout(open, wait);
          wait = Math.min(wait * 2, 30_000);
        });
    };
  };
  open();
  cleanup.push(() => {
    closed = true;
    clearTimeout(again);
    source?.close();
  });

  const panel = el('section', { className: 'chat-panel panel' }, el('h3', {}, '실시간 채팅', state));
  // 고정 공지(#109): 누구나 본다. 방송 정보(5초마다 새로 읽음)를 따라간다.
  const noticeBox = el('p', { className: 'chat-notice', role: 'note' });
  const showNotice = () => {
    const n = current().notice ?? '';
    noticeBox.textContent = n ? `공지: ${n}` : '';
    noticeBox.hidden = !n;
  };
  showNotice();
  const noticeTimer = setInterval(showNotice, 2_000);
  cleanup.push(() => clearInterval(noticeTimer));
  panel.append(noticeBox);
  // 천천히 모드(#87): 주인은 고르고, 시청자는 안내를 본다. 방송 정보(5초마다 새로 읽음)를 따라간다.
  const slowHint = el('p', { className: 'hint slow-hint' });
  const showSlow = () => {
    const s = current().slow_mode_secs ?? 0;
    slowHint.textContent = s > 0 && !isOwner() ? `천천히 모드: ${s}초에 한 번 쓸 수 있습니다.` : '';
    slowHint.hidden = !slowHint.textContent;
  };
  showSlow();
  const slowTimer = setInterval(showSlow, 2_000);
  cleanup.push(() => clearInterval(slowTimer));
  panel.append(slowHint);
  // 제한 목록(주인만)
  const bans = el('ul', { className: 'ban-list' });
  const loadBans = async () => {
    if (!isOwner()) return;
    try {
      const list = await listBans(broadcastId);
      bans.replaceChildren(
        ...list.map((x) => {
          const off = el('button', { type: 'button', className: 'small ghost', textContent: '해제' });
          off.onclick = async () => {
            off.disabled = true;
            try {
              await unbanUser(broadcastId, x.user_id);
              await loadBans();
            } catch (e) {
              error.textContent = message(e);
              off.disabled = false;
            }
          };
          return el('li', {}, el('strong', { textContent: x.username }), ` ${x.until ? `${shortTime(x.until)}까지` : '해제할 때까지'} `, off);
        }),
      );
      bans.hidden = list.length === 0;
    } catch {
      // 목록을 못 읽어도 채팅은 그대로 둔다
    }
  };
  if (isOwner()) {
    const slow = el(
      'select',
      { className: 'slow-select', ariaLabel: '천천히 모드' },
      ...[0, 5, 10, 30].map((s) => el('option', { value: String(s), textContent: s === 0 ? '천천히 모드 끔' : `천천히 모드 ${s}초` })),
    );
    slow.value = String(current().slow_mode_secs ?? 0);
    slow.onchange = async () => {
      slow.disabled = true;
      try {
        await setSlowMode(broadcastId, Number(slow.value));
        error.textContent = Number(slow.value) > 0 ? `천천히 모드를 ${slow.value}초로 켰습니다.` : '천천히 모드를 껐습니다.';
      } catch (e) {
        error.textContent = message(e);
      } finally {
        slow.disabled = false;
      }
    };
    // 공지 고정(#109): 입력하고 올리기, 비우고 올리면 내림
    const noticeInput = el('input', { type: 'text', className: 'notice-input', placeholder: '채팅 위에 고정할 공지 (100자까지)', ariaLabel: '공지', value: current().notice ?? '' });
    const noticeSave = el('button', { type: 'button', className: 'small', textContent: '공지 올리기' });
    noticeSave.onclick = async () => {
      noticeSave.disabled = true;
      try {
        await setNotice(broadcastId, noticeInput.value);
        // 서버가 다듬은 값을 바로 다시 읽어 보여 준다(#115). 못 읽으면 다음 새로 읽기(5초)에 맞춰진다.
        const saved = (await getBroadcast(broadcastId).catch(() => null))?.notice ?? current().notice;
        current().notice = saved;
        noticeInput.value = saved;
        showNotice();
        error.textContent = saved ? '공지를 올렸습니다.' : '공지를 내렸습니다.';
      } catch (e) {
        error.textContent = message(e);
      } finally {
        noticeSave.disabled = false;
      }
    };
    // 채팅 금칙어(#112): 쉼표로 나눠 적고 저장. 서버가 다듬은 목록으로 칸을 다시 채운다
    const wordsInput = el('input', { type: 'text', className: 'words-input', placeholder: '금칙어 (쉼표로 구분, 30개·한 단어 20자까지)', ariaLabel: '금칙어' });
    const wordsSave = el('button', { type: 'button', className: 'small', textContent: '금칙어 저장' });
    // 목록을 읽기 전에는 저장을 막는다. 못 읽은 채 저장하면 기존 목록이 지워진다(#117)
    wordsSave.disabled = true;
    void getBannedWords(broadcastId)
      .then((w) => {
        wordsInput.value = w.join(', ');
        wordsSave.disabled = false;
      })
      .catch(() => {
        wordsInput.placeholder = '금칙어 목록을 읽지 못했습니다. 새로고침해 주세요';
      });
    wordsSave.onclick = async () => {
      wordsSave.disabled = true;
      try {
        const saved = await setBannedWords(broadcastId, wordsInput.value.split(','));
        wordsInput.value = saved.join(', ');
        error.textContent = saved.length ? `금칙어 ${saved.length}개를 저장했습니다.` : '금칙어를 모두 지웠습니다.';
      } catch (e) {
        error.textContent = message(e);
      } finally {
        wordsSave.disabled = false;
      }
    };
    bans.hidden = true;
    panel.append(
      el('div', { className: 'row owner-tools' }, slow),
      el('div', { className: 'row owner-tools notice-tools' }, noticeInput, noticeSave),
      el('div', { className: 'row owner-tools words-tools' }, wordsInput, wordsSave),
      bans,
    );
    void loadBans();
    // 댓글 옆 버튼으로 막은 것, 기한이 지난 것도 목록에 맞춰지게 10초마다 다시 읽는다(#94)
    const bansTimer = setInterval(() => void loadBans(), 10_000);
    const onBansChanged = () => void loadBans();
    window.addEventListener(BANS_CHANGED, onBansChanged);
    cleanup.push(() => {
      clearInterval(bansTimer);
      window.removeEventListener(BANS_CHANGED, onBansChanged);
    });
  }
  panel.append(list);
  if (me && current().status === 'ended') {
    // 종료된 방송에는 쓸 수 없으니 입력 칸 대신 안내를 보여 준다(#90 R5)
    panel.append(el('p', { className: 'hint', textContent: '종료된 방송이라 채팅을 쓸 수 없습니다.' }));
  } else if (me) {
    // 글자 수는 서버가 센다(200자). 화면의 maxLength는 이모지를 두 글자로 세어 더 엄격해서 두지 않는다(#90 R7)
    const input = el('input', { type: 'text', placeholder: '채팅 (200자까지, Enter로 보내기)', ariaLabel: '채팅 입력', autocomplete: 'off' });
    const send = el('button', { type: 'submit', textContent: '보내기' });
    const form = el('form', { className: 'chat-form row' }, input, send);
    form.onsubmit = (ev) => {
      ev.preventDefault();
      if (current().status === 'ended') {
        error.textContent = '종료된 방송에는 채팅할 수 없습니다';
        return;
      }
      const text = input.value;
      action(send, error, async () => {
        await sendChat(broadcastId, text);
        // 보낸 글은 스트림으로 돌아와 목록에 붙는다. 입력 칸만 비운다.
        if (input.value === text) input.value = '';
        input.focus();
      })();
    };
    panel.append(form, error);
    // 보고 있는 사이에 방송이 끝나면 입력 칸을 안내로 바꾼다(#136). 방송 정보는 상세 화면이 5초마다 새로 읽는다.
    const endWatch = setInterval(() => {
      if (current().status === 'ended' && form.isConnected) {
        form.replaceWith(el('p', { className: 'hint', textContent: '종료된 방송이라 채팅을 쓸 수 없습니다.' }));
        clearInterval(endWatch);
      }
    }, 2_000);
    cleanup.push(() => clearInterval(endWatch));
  } else {
    panel.append(el('p', { className: 'hint' }, el('a', { href: '#/login', textContent: '로그인' }), '하면 채팅할 수 있습니다.'));
  }
  return panel;
}

const BANS_CHANGED = 'onair-bans-changed';

/**
 * 방송 주인이 글 옆에서 쓰는 제한 버튼(#87). "10분" 또는 "계속"(해제할 때까지). 결과는 note에 적는다.
 * 서버가 주인인지 다시 확인한다(화면에서 버튼을 보여 주는 것은 편의일 뿐).
 */
function banButtons(broadcastId: number, userId: number, username: string, note: HTMLElement): HTMLElement {
  const make = (label: string, ten: boolean) => {
    const b = el('button', { type: 'button', className: 'small ghost ban', textContent: label, title: `${username} 님의 글쓰기 막기` });
    b.onclick = async () => {
      if (!confirm(`${username} 님이 이 방송에 채팅·댓글을 쓰지 못하게 ${ten ? '10분 동안' : '해제할 때까지'} 막을까요?`)) return;
      b.disabled = true;
      try {
        await banUser(broadcastId, userId, ten);
        note.textContent = `${username} 님을 ${ten ? '10분 동안' : '해제할 때까지'} 막았습니다.`;
        // 채팅 칸의 제한 목록에 알린다. 댓글 옆 버튼으로 막아도 목록이 바로 바뀐다(#94 R3).
        window.dispatchEvent(new Event(BANS_CHANGED));
      } catch (e) {
        note.textContent = message(e);
      } finally {
        b.disabled = false;
      }
    };
    return b;
  };
  return el('span', { className: 'ban-buttons' }, make('10분 제한', true), make('계속 제한', false));
}

function commentsPanel(broadcastId: number, gen: number, ownerId: number): HTMLElement {
  const list = el('ul', { className: 'comments' });
  const error = el('p', { className: 'error', role: 'alert' });
  const count = el('span', { className: 'hint' });
  /** 수정 중인 댓글 id들 */
  const editing = new Set<number>();
  /** 댓글 id → 화면 요소와 그 내용 표시(내용이나 수정 시각이 바뀌면 다시 그린다) */
  const nodes = new Map<number, { li: HTMLElement; sig: string }>();
  const sigOf = (c: Comment) => `${c.body}\u0000${c.updated_at ?? ''}`;
  let stopped = false;
  let seq = 0;
  let applied = 0;
  let firstPaint = true;
  let wantBottom = false;

  const row = (c: Comment): HTMLElement => {
    const head = el(
      'div',
      { className: 'comment-head' },
      el('strong', { textContent: c.author_username }),
      el('span', { className: 'hint', textContent: ` ${shortTime(c.created_at)}${c.updated_at ? ' (수정됨)' : ''}` }),
    );
    const text = el('p', { className: 'comment-body', textContent: c.body });
    const li = el('li', { className: 'comment' }, head, text);
    li.dataset.id = String(c.id);
    // 방송 주인에게는 남의 댓글 옆에 제한 버튼(#87)
    if (me && me.id === ownerId && me.id !== c.author_id) {
      head.append(banButtons(broadcastId, c.author_id, c.author_username, error));
    }
    if (me?.id !== c.author_id) return li;

    const rowError = el('p', { className: 'error', role: 'alert' });
    const editBtn = el('button', { type: 'button', className: 'small', textContent: '수정' });
    const delBtn = el('button', { type: 'button', className: 'small danger', textContent: '삭제' });
    // 수정을 끝내면 이 댓글을 다음 목록에서 새로 그리게 한다
    const finishEdit = () => {
      editing.delete(c.id);
      nodes.delete(c.id);
    };
    editBtn.onclick = () => {
      editing.add(c.id);
      // 글자 수는 서버가 센다(브라우저 maxLength는 이모지를 2자로 세서 서버와 다르다)
      const box = el('textarea', { rows: 2, value: c.body, ariaLabel: '댓글 수정' });
      const save = el('button', { type: 'button', className: 'small', textContent: '저장' });
      const cancel = el('button', { type: 'button', className: 'small', textContent: '취소' });
      save.onclick = action(save, rowError, async () => {
        await updateComment(c.id, box.value);
        finishEdit();
        await load();
      });
      cancel.onclick = () => {
        finishEdit();
        void load();
      };
      li.replaceChildren(head, box, el('div', { className: 'row' }, save, cancel), rowError);
      box.focus();
    };
    delBtn.onclick = action(delBtn, rowError, async () => {
      if (!confirm('이 댓글을 지울까요?')) return;
      await deleteComment(c.id);
      await load();
    });
    li.append(el('div', { className: 'row' }, editBtn, delBtn), rowError);
    return li;
  };

  /** 목록을 다시 불러와 그린다. toBottom이면 맨 아래(최신 댓글)로 내린다. */
  const load = async (toBottom = false) => {
    const my = ++seq;
    // 내가 쓴 직후 요청이 옛 응답으로 버려져도, 다음에 그려질 때 맨 아래로 내린다
    if (toBottom) wantBottom = true;
    try {
      const items = await listComments(broadcastId);
      if (stopped || gen !== routeGen || my < applied) return;
      applied = my;
      count.textContent = ` ${items.length}개`;
      // 읽는 사람이 이미 맨 아래 근처에 있으면 새 댓글이 와도 맨 아래를 계속 보여 준다
      const nearBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 40;
      const wanted = items.map((c) => {
        const old = nodes.get(c.id);
        if (old && (editing.has(c.id) || old.sig === sigOf(c))) return old.li;
        const li = row(c);
        nodes.set(c.id, { li, sig: sigOf(c) });
        return li;
      });
      const ids = new Set(items.map((c) => c.id));
      for (const id of [...nodes.keys()]) {
        if (!ids.has(id)) {
          nodes.delete(id);
          editing.delete(id);
        }
      }
      // 1) 필요 없는 요소(지워졌거나 내용이 바뀐 댓글의 옛 요소)를 먼저 뺀다.
      // 2) 새 요소만 제자리에 끼워 넣는다. 남은 요소는 순서가 그대로라(댓글은 id 순) 옮기지 않는다.
      //    수정 중인 칸을 옮기면 커서가 빠지므로, 기존 요소는 절대 옮기지 않는 것이 핵심이다(#37 2바퀴).
      const keep = new Set<Element>(wanted);
      for (const child of [...list.children]) if (!keep.has(child)) child.remove();
      wanted.forEach((li, i) => {
        const cur = list.children[i];
        if (cur !== li) list.insertBefore(li, cur ?? null);
      });
      if (wantBottom || firstPaint || nearBottom) list.scrollTop = list.scrollHeight;
      wantBottom = false;
      firstPaint = false;
    } catch (e) {
      // 방송이 지워졌으면(404) 멈춘다. 그 밖에는 잠깐 끊긴 것으로 보고 다음에 다시 부른다.
      if (e instanceof ApiError && e.status === 404) stopped = true;
    }
  };

  const panel = el('section', { className: 'comments-panel panel' }, el('h3', {}, '댓글', count), list);
  if (me) {
    const input = el('textarea', { rows: 2, placeholder: '댓글을 남겨 보세요 (300자까지)', ariaLabel: '댓글 입력' });
    const send = el('button', { type: 'submit', textContent: '댓글 쓰기' });
    const form = el('form', { className: 'comment-form' }, input, el('div', { className: 'row' }, send), error);
    form.onsubmit = (ev) => {
      ev.preventDefault();
      action(send, error, async () => {
        await createComment(broadcastId, input.value);
        input.value = '';
        await load(true);
      })();
    };
    panel.append(form);
  } else {
    panel.append(el('p', { className: 'hint' }, el('a', { href: '#/login', textContent: '로그인' }), '하면 댓글을 쓸 수 있습니다.'));
  }

  void load();
  const timer = setInterval(() => {
    if (!stopped) void load();
  }, 4_000);
  cleanup.push(() => {
    stopped = true;
    clearInterval(timer);
  });
  return panel;
}

// ---------- 방송하기(소유자) ----------

const PUBLISH_LABEL: Record<PublishState, string> = {
  idle: '송출하지 않음',
  starting: '송출 준비 중...',
  live: '송출 중',
  stopping: '멈추는 중...',
};

/**
 * 방송하기 칸: PC 화면 공유 + 마이크(#1), 카메라 + 마이크(#2, 휴대폰·PC). 둘 다 같은 Publisher를 쓴다.
 * 화면 공유가 없는 브라우저(휴대폰)에서는 화면 공유 줄을 숨기고 카메라만 보인다.
 */
function studioPanel(current: () => Broadcast, publisher: Publisher): { el: HTMLElement; update: (b: Broadcast) => void } {
  const state = el('strong', { className: 'pub-state', textContent: PUBLISH_LABEL.idle });
  const note = el('p', { className: 'hint', role: 'status' });
  const warn = el('p', { className: 'warn', role: 'alert' });
  const preview = el('video', { className: 'preview', muted: true, autoplay: true, playsInline: true });
  preview.hidden = true;
  // 보내는 소리 크기(#80)
  const levelLabel = el('span', { className: 'level-label', textContent: '보내는 소리' });
  const levelBar = el('i');
  const level = el('div', { className: 'level', role: 'meter', ariaLabel: '보내는 소리 크기' }, levelLabel, el('span', { className: 'level-bar' }, levelBar));
  level.setAttribute('aria-valuemin', '0');
  level.setAttribute('aria-valuemax', '100');
  level.hidden = true;
  const withMic = el('input', { type: 'checkbox', checked: true, id: 'with-mic' });
  const startScreen = el('button', { type: 'button', className: 'start-screen', textContent: '화면 공유로 방송 시작' });
  const facing = el(
    'select',
    { className: 'facing', title: '카메라', ariaLabel: '카메라 고르기' },
    el('option', { value: 'user', textContent: '앞 카메라' }),
    el('option', { value: 'environment', textContent: '뒤 카메라' }),
  );
  const startCamera = el('button', { type: 'button', className: 'start-camera', textContent: '카메라로 방송 시작' });
  const stop = el('button', { type: 'button', className: 'danger stop-publish', textContent: '송출 멈춤' });
  stop.hidden = true;
  // 송출 중 앞·뒤 카메라 전환(#67). 카메라 방송 중에만 보인다.
  const flip = el('button', { type: 'button', className: 'flip-camera', textContent: '카메라 전환' });
  flip.hidden = true;
  const screenSupported = !!navigator.mediaDevices?.getDisplayMedia;
  const cameraSupported = !!navigator.mediaDevices?.getUserMedia;
  /** 송출하는 동안만 거는 것들(화면 꺼짐 막기, 카메라 방송의 화면 숨김 감시 등). 멈추면 모두 푼다. */
  let releases: (() => void)[] = [];

  const paint = (s: PublishState, detail?: string) => {
    state.textContent = PUBLISH_LABEL[s];
    state.className = `pub-state ${s}`;
    const idle = s === 'idle';
    startScreen.disabled = !idle || !screenSupported;
    startCamera.disabled = !idle || !cameraSupported;
    withMic.disabled = !idle;
    facing.disabled = !idle || !cameraSupported;
    stop.hidden = idle;
    // 멈추는 중에는 멈춤을 다시 누를 수 없다(#30 P2)
    stop.disabled = s === 'stopping';
    // 연결이 다 된 뒤에만 전환할 수 있다
    flip.disabled = s !== 'live';
    if (idle) {
      preview.hidden = true;
      preview.srcObject = null;
      preview.classList.remove('mirror');
      warn.textContent = '';
      releases.forEach((f) => f());
      releases = [];
      if (detail) note.textContent = detail;
    }
  };
  publisher.onState = paint;

  /**
   * 보내는 소리 크기 막대(#80). 송출하는 사람이 소리가 실제로 나가는지 볼 수 있게 한다.
   * 소리 트랙이 없으면 "소리 없이 송출 중"을 보여 준다. 돌려준 함수를 부르면 분석을 멈추고 막대를 숨긴다.
   */
  const setLevel = (pct: number) => {
    levelBar.style.width = `${pct}%`;
    level.dataset.level = String(pct);
    level.setAttribute('aria-valuenow', String(pct));
  };
  const startMeter = (track: MediaStreamTrack | undefined): (() => void) => {
    level.hidden = false;
    setLevel(0);
    const hide = () => {
      level.hidden = true;
      setLevel(0);
    };
    if (!track) {
      levelLabel.textContent = '소리 없이 송출 중';
      level.setAttribute('aria-valuetext', '소리 없음');
      return hide;
    }
    levelLabel.textContent = '보내는 소리';
    level.removeAttribute('aria-valuetext');
    // 막대는 송출을 돕는 표시일 뿐이다. 무엇이 실패해도 송출과 뒷처리(화면 숨김 감시 등)는 그대로 가게 여기서 오류를 삼킨다(#80 리뷰)
    let ctx: AudioContext | null = null;
    try {
      ctx = new AudioContext();
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 512;
      ctx.createMediaStreamSource(new MediaStream([track])).connect(analyser);
      const buf = new Float32Array(analyser.fftSize);
      const c = ctx;
      // 클릭 직후가 아니라서 브라우저(특히 사파리)가 소리 분석을 막을 수 있다. 그러면 막대가 0에 멈춰
      // 소리가 안 나가는 것처럼 보이므로, 잴 수 없다고 따로 알린다(소리는 그대로 보낸다).
      void c
        .resume()
        .catch(() => {})
        .finally(() => {
          if (c.state !== 'running' && !level.hidden) levelLabel.textContent = '소리 크기를 잴 수 없습니다(소리는 보내는 중)';
        });
      // 화면 그리기 신호(requestAnimationFrame)는 탭을 가리면 멈추므로 0.1초 간격으로 잰다
      const timer = setInterval(() => {
        analyser.getFloatTimeDomainData(buf);
        let sum = 0;
        for (const v of buf) sum += v * v;
        setLevel(Math.min(100, Math.round(Math.sqrt(sum / buf.length) * 300)));
      }, 100);
      return () => {
        clearInterval(timer);
        void c.close().catch(() => {});
        hide();
      };
    } catch {
      void ctx?.close().catch(() => {});
      hide();
      return () => {};
    }
  };

  /** 두 시작 버튼이 함께 쓰는 흐름. 누르자마자 막고(#30 P8), 장치 켜기를 서버 요청보다 먼저 한다(#30 P4). */
  const begin = async (
    getTracks: Parameters<Publisher['start']>[0],
    endedReason: string,
    afterStart: (stream: MediaStream) => void,
  ) => {
    startScreen.disabled = true;
    startCamera.disabled = true;
    warn.textContent = '';
    note.textContent = '';
    try {
      const stream = await publisher.start(getTracks, async () => (await publishInfo(current().id)).whip_path, endedReason);
      // 미리보기에는 영상만 넣는다(소리 트랙을 붙이지 않아 되울림 걱정이 없게, #80 리뷰)
      preview.srcObject = new MediaStream(stream.getVideoTracks());
      preview.hidden = false;
      releases.push(keepScreenOn());
      // 송출 뒷처리를 먼저 하고 막대는 그다음에 켠다(막대에서 무엇이 실패해도 뒷처리는 빠지지 않게)
      afterStart(stream);
      releases.push(startMeter(stream.getAudioTracks()[0]));
      // 휴대폰(터치 화면)에서는 송출 멈춤 버튼과 미리보기가 첫 화면 아래로 밀려 있다. 시작하면 그 자리로 내려간다(심사자 시뮬레이션 3차)
      if (matchMedia('(pointer: coarse)').matches) stop.scrollIntoView({ block: 'start', behavior: 'smooth' });
    } catch (e) {
      // 멈춰서 취소된 시도는 조용히 넘긴다(새 시도나 멈춘 이유를 덮지 않게, #30 P7)
      if (e instanceof CancelledError) return;
      note.textContent = e instanceof Error ? e.message : String(e);
    } finally {
      if (publisher.state === 'idle') {
        startScreen.disabled = !screenSupported;
        startCamera.disabled = !cameraSupported;
      }
    }
  };

  startScreen.onclick = () => {
    let screenAudio = 0;
    let micError = '';
    let mic: MediaStreamTrack | undefined;
    void begin(
      async (register) => {
        const t = await screenAndMic(register, withMic.checked);
        screenAudio = t.screenAudio;
        micError = t.micError;
        mic = t.micTrack;
        return t;
      },
      '화면 공유가 끝났습니다',
      () => {
        const warns = [micError];
        if (screenAudio === 0) {
          warns.push('화면 소리가 함께 가지 않습니다. 공유 창에서 "탭 오디오 공유" 또는 "시스템 오디오 공유"를 켰는지 확인하세요(Chrome·Edge만 되고, "창" 공유에는 소리 칸이 없습니다).');
        }
        const show = () => {
          if (publisher.state !== 'idle') warn.textContent = warns.filter(Boolean).join(' ');
        };
        // 송출 중 마이크만 끊기면(마이크를 뺌 등) 화면과 화면 소리는 계속 가므로 알린다(#31 P11)
        if (mic) {
          const lostMic = '마이크가 꺼졌습니다. 지금은 마이크 소리 없이 송출 중입니다.';
          mic.onended = () => {
            warns.push(lostMic);
            show();
          };
          if (mic.readyState === 'ended') warns.push(lostMic);
        }
        show();
      },
    );
  };
  startCamera.onclick = () => {
    const f = facing.value as Facing;
    let mic: MediaStreamTrack | undefined;
    let micError = '';
    void begin(
      async (register) => {
        const t = await cameraAndMic(register, f);
        mic = t.audio[0];
        micError = t.micError;
        return t;
      },
      '카메라가 꺼져 송출을 멈췄습니다',
      (stream) => {
        // 경고는 종류별로 따로 들고 이어 붙여 보여 준다. 하나가 풀려도 다른 경고가 지워지지 않게(#33 2바퀴).
        const warns = { facing: '', mute: '', mic: micError };
        const showWarns = () => {
          if (publisher.state !== 'idle') warn.textContent = Object.values(warns).filter(Boolean).join(' ');
        };
        // 지금 보내는 카메라 방향. 전환 버튼이 바꾼다(#67).
        let want = f;
        /** 새 영상 트랙을 미리보기에 붙이고 방향·멈춤을 살핀다. 처음 시작과 카메라 전환이 함께 쓴다. */
        const watch = (video: MediaStreamTrack | undefined, flipped = false) => {
          // 실제로 켜진 카메라 방향(브라우저가 알려 줄 때). 고른 카메라가 없으면 브라우저가 다른 카메라를 켠다(#33 M4).
          const actual = video?.getSettings().facingMode as Facing | undefined;
          warns.facing =
            actual && actual !== want ? `고른 ${want === 'user' ? '앞' : '뒤'} 카메라가 없어 다른 카메라로 보냅니다.` : '';
          // 앞 카메라는 거울처럼 보여야 자연스럽다. 미리보기만 뒤집는다(시청자에게 가는 영상은 그대로).
          // 방향을 알려 주지 않는 카메라(PC 웹캠)는 전환해도 거울 여부를 바꾸지 않는다(#68 R3: 좌우만 뒤집혀 보이던 문제)
          if (actual || !flipped) preview.classList.toggle('mirror', (actual ?? want) === 'user');
          warns.mute = '';
          if (video) {
            video.onmute = () => {
              warns.mute = '카메라 영상이 잠시 멈췄습니다. 이 화면을 켜 두세요.';
              showWarns();
            };
            video.onunmute = () => {
              warns.mute = '';
              showWarns();
            };
            // 준비하는 사이에 이미 멈춰 있었으면 지금 알린다
            if (video.muted) warns.mute = '카메라 영상이 잠시 멈췄습니다. 이 화면을 켜 두세요.';
          }
        };
        watch(stream.getVideoTracks()[0]);

        // 송출 중 앞·뒤 카메라 전환(#67). 새 방향을 못 켜면 원래 방향으로 되돌린다. 그것도 안 되면 Publisher가 송출을 멈춘다.
        // 카메라가 2개 이상일 때만 버튼을 보인다(#68 R3). 개수는 권한을 받은 지금 세야 정확하다.
        let alive = true;
        flip.hidden = true;
        flip.disabled = publisher.state !== 'live';
        void cameraCount().then((n) => {
          if (alive) flip.hidden = n < 2;
        });
        flip.onclick = async () => {
          flip.disabled = true;
          const from = want;
          const to: Facing = from === 'user' ? 'environment' : 'user';
          const fromSettings = (preview.srcObject as MediaStream | null)?.getVideoTracks()[0]?.getSettings();
          const fromId = fromSettings?.deviceId;
          // 방향을 알려 주는 카메라(휴대폰)만 앞·뒤로 바꾼다. 모르면(PC) 다른 카메라로 돌아가며 바꾼다.
          const knows = !!fromSettings?.facingMode;
          const label = knows ? `${to === 'user' ? '앞' : '뒤'} 카메라` : '다른 카메라';
          let note = '';
          try {
            const t = await publisher.switchVideo(async () => {
              try {
                return await otherCamera(knows ? to : undefined, fromId, () => publisher.state === 'live');
              } catch (e) {
                // 그사이 송출을 멈췄으면 원래 카메라를 다시 켜지 않는다(#68 R4)
                if (publisher.state !== 'live') throw e;
                note = `${label}를 켜지 못해 원래 카메라로 돌아왔습니다(${deviceMessage(e)}).`;
                return sameCamera(from, fromId);
              }
            }, '카메라가 꺼져 송출을 멈췄습니다');
            // 실제 방향을 알 때만 고른 방향을 바꾼다. 모르면 그대로 둬서 다음 송출의 거울 여부가 바뀌지 않게(2바퀴 리뷰 2)
            const now = t.getSettings().facingMode as Facing | undefined;
            if (now === 'user' || now === 'environment') {
              want = now;
              facing.value = now;
            }
            preview.srcObject = new MediaStream([t]);
            watch(t, true);
            if (note) warns.facing = note;
            showWarns();
          } catch {
            // 실패하면 Publisher가 이유와 함께 송출을 멈춘다(멈춘 이유는 아래 안내 칸에 나온다)
          } finally {
            flip.disabled = publisher.state !== 'live';
          }
        };
        releases.push(() => {
          alive = false;
          flip.hidden = true;
          flip.onclick = null;
        });

        // 휴대폰은 다른 앱으로 가거나 화면이 꺼질 때 카메라를 끝내지 않고 잠시 멈추기(mute)만 하는 경우가 많다.
        // 그러면 검은 화면이 나가고 마이크가 계속 갈 수도 있어서, 카메라 방송은 화면이 숨겨지면 송출을 멈춘다(#33 M1).
        // (PC 화면 공유는 다른 탭으로 가는 게 보통이라 이 처리를 하지 않는다.)
        const onHidden = () => {
          if (document.visibilityState === 'hidden' && publisher.state !== 'idle') {
            void publisher.stop('화면을 떠나 송출을 멈췄습니다. 카메라 방송은 이 화면을 켜 둔 동안만 됩니다');
          }
        };
        document.addEventListener('visibilitychange', onHidden);
        releases.push(() => document.removeEventListener('visibilitychange', onHidden));
        // 마이크만 끊기면(전화 등) 소리 없이 계속 가므로 알린다(#33 M6)
        if (mic) {
          mic.onended = () => {
            warns.mic = '마이크가 꺼졌습니다. 지금은 소리 없이 송출 중입니다.';
            showWarns();
          };
          if (mic.readyState === 'ended') warns.mic = '마이크가 꺼졌습니다. 지금은 소리 없이 송출 중입니다.';
        }
        showWarns();
        // 송출을 준비하는 사이에(장치 켠 뒤 서버 응답을 기다리는 1~2초) 이미 화면을 떠났으면 바로 멈춘다(#33 2바퀴)
        onHidden();
      },
    );
  };
  stop.onclick = () => void publisher.stop('송출을 멈췄습니다. 다시 시작할 수 있습니다.');

  // 송출 중에 창을 닫으려 하면 한 번 묻는다
  const beforeUnload = (ev: BeforeUnloadEvent) => {
    if (publisher.state !== 'idle') ev.preventDefault();
  };
  window.addEventListener('beforeunload', beforeUnload);
  cleanup.push(() => window.removeEventListener('beforeunload', beforeUnload));
  // 그래도 닫거나 새로고침하면 송출 자원을 바로 정리한다(#31 P14: OME가 끊김을 알 때까지 "방송 중"이 남아 다시 시작하면 409)
  const onPageHide = () => publisher.abandon();
  window.addEventListener('pagehide', onPageHide);
  cleanup.push(() => window.removeEventListener('pagehide', onPageHide));

  // 화면 공유(PC): 화면 공유가 없는 브라우저(휴대폰)에서는 줄째 숨긴다
  const screenRow = el(
    'div',
    { className: 'studio-mode screen-mode' },
    el('h4', { textContent: 'PC 화면 공유' }),
    el('p', { className: 'hint', textContent: '공유 창에서 탭을 고르고 "탭 오디오 공유"를 켜면 탭 소리가 함께 갑니다. Windows에서 전체 화면을 고르면 "시스템 오디오 공유"도 고를 수 있습니다.' }),
    el('label', { className: 'check' }, withMic, ' 마이크 소리도 함께 보내기'),
    el('div', { className: 'row' }, startScreen),
  );
  screenRow.hidden = !screenSupported;
  const cameraRow = el(
    'div',
    { className: 'studio-mode camera-mode' },
    el('h4', { textContent: '카메라 (휴대폰·PC)' }),
    el('p', { className: 'hint', textContent: cameraSupported
      ? '카메라 영상과 마이크 소리를 보냅니다. 휴대폰에서는 송출하는 동안 이 화면을 켜 두세요. 다른 앱으로 가거나 화면을 끄면 브라우저가 카메라를 멈춥니다.'
      : '이 브라우저에서는 카메라를 쓸 수 없습니다. https 주소에서 최신 Chrome이나 Safari로 여세요.' }),
    el('div', { className: 'row' }, facing, startCamera),
  );
  const panel = el(
    'section',
    { className: 'panel studio' },
    el('h3', { textContent: '방송하기' }),
    // 휴대폰(터치 화면)에서는 카메라 방송을 먼저 보여 준다
    ...(matchMedia('(pointer: coarse)').matches ? [cameraRow, screenRow] : [screenRow, cameraRow]),
    el('div', { className: 'row' }, stop, flip, state),
    preview,
    level,
    warn,
    note,
  );
  const update = (b: Broadcast) => {
    // 종료된 방송은 송출할 수 없으니 칸을 숨긴다
    if (b.status !== 'ended') {
      panel.hidden = false;
      return;
    }
    // 종료된 방송은 송출할 수 없다. 이 화면에서 송출 중이었으면(다른 탭에서 종료 등) 이유를 남기고 칸을 숨긴다(#30 P17).
    if (publisher.state !== 'idle') {
      void publisher.stop('방송이 종료돼 송출을 멈췄습니다');
      // 멈춘 이유는 아래 소유자 칸의 "종료된 방송" 안내와 함께 잠깐 보이게, 칸은 정리가 끝난 다음 숨긴다
      setTimeout(() => (panel.hidden = true), 3_000);
    } else {
      panel.hidden = true;
    }
  };
  paint('idle');
  return { el: panel, update };
}

/** datetime-local 칸에 넣을 값(이 기기 시각, 분까지) */
function localInputValue(iso: string): string {
  const d = new Date(iso);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

function ownerPanel(b: Broadcast, onChange: (b: Broadcast) => void, publisher: Publisher): { el: HTMLElement; update: (b: Broadcast) => void } {
  const error = el('p', { className: 'error', role: 'alert' });
  // 시작 예정 시각(#114): 이 기기 시각으로 고르면 서버에는 UTC로 보낸다. 지우기는 null.
  const when = el('input', { type: 'datetime-local', className: 'schedule-input', ariaLabel: '시작 예정 시각', value: b.scheduled_at ? localInputValue(b.scheduled_at) : '' });
  const setWhen = el('button', { type: 'button', className: 'small', textContent: '예정 저장' });
  const clearWhen = el('button', { type: 'button', className: 'small ghost', textContent: '예정 지우기' });
  setWhen.onclick = action(setWhen, error, async () => {
    if (!when.value) throw new Error('예정 시각을 고르세요');
    onChange(await setSchedule(b.id, new Date(when.value).toISOString()));
  });
  clearWhen.onclick = action(clearWhen, error, async () => {
    onChange(await setSchedule(b.id, null));
    when.value = '';
  });
  const scheduleRow = el('div', { className: 'row schedule-tools' }, el('span', { className: 'hint', textContent: '시작 예정' }), when, setWhen, clearWhen);

  // 수정
  const title = el('input', { value: b.title, maxLength: 100, ariaLabel: '방송 제목', placeholder: '방송 제목 (100자 이하)' });
  const description = el('textarea', { value: b.description, maxLength: 1000, rows: 3, ariaLabel: '방송 설명', placeholder: '설명 (1000자 이하, 선택)' });
  const category = categorySelect(b.category ?? 'etc');
  const save = el('button', { type: 'button', textContent: '저장' });
  const saved = el('span', { className: 'hint', role: 'status' });
  // 저장 뒤에 다시 고치기 시작하면 "저장했습니다"를 지운다(리뷰 지적 2)
  title.oninput = description.oninput = category.onchange = () => (saved.textContent = '');
  save.onclick = action(save, error, async () => {
    saved.textContent = '';
    onChange(await updateBroadcast(b.id, title.value, description.value, category.value as Category));
    saved.textContent = '저장했습니다.';
  });

  // 송출 주소(소유자만)
  const publishBox = el('div', { className: 'publish' });
  const showPublish = el('button', { type: 'button', textContent: '송출 주소 보기' });
  const showKey = (whipPath: string, rotated = false) => {
    const rotate = el('button', { type: 'button', className: 'small', textContent: '송출 키 다시 만들기' });
    rotate.onclick = action(rotate, error, async () => {
      if (!confirm('송출 키를 새로 만들까요? 옛 주소는 더 쓸 수 없고, 옛 키로 송출 중이면 끊깁니다.')) return;
      // 이 브라우저에서 송출 중이면 먼저 멈춘다(서버도 옛 키 송출을 끊는다)
      await publisher.stop('송출 키를 새로 만들어 송출을 멈췄습니다. 다시 시작하면 새 키로 송출합니다.');
      let p;
      try {
        p = await rotateStreamKey(b.id);
      } catch (e) {
        // 실패해도(502, #131) 키는 이미 바뀌었을 수 있다. 옛 주소를 보여 두지 않는다.
        publishBox.replaceChildren(
          el('p', { className: 'hint', textContent: "'송출 주소 보기'를 다시 눌러 지금 주소를 확인하세요." }),
        );
        throw e;
      }
      showKey(p.whip_path, true);
    });
    publishBox.replaceChildren(
      el('p', { className: 'hint', textContent: 'WHIP 송출 주소입니다. 남에게 보여 주지 마세요(이 주소를 가진 사람은 이 방송으로 송출할 수 있습니다). OBS 30 이상의 WHIP 출력에도 쓸 수 있습니다.' }),
      el('code', { textContent: `${location.origin}${whipPath}` }),
      (() => {
        const copy = el('button', { type: 'button', className: 'small', textContent: '주소 복사' });
        // http(보안 연결이 아닌 곳)에서는 navigator.clipboard가 없어 바로 오류가 난다. 그때도 안내가 뜨게 한다(리뷰 지적 1)
        copy.onclick = () =>
          void Promise.resolve()
            .then(() => navigator.clipboard.writeText(`${location.origin}${whipPath}`))
            .then(() => (copy.textContent = '복사했습니다'))
            .catch(() => (copy.textContent = '복사하지 못했습니다'));
        return copy;
      })(),
      ...(rotated ? [el('p', { className: 'hint', textContent: '새 송출 키를 만들었습니다. 옛 주소는 더 쓸 수 없습니다.' })] : []),
      el('div', { className: 'row' }, rotate),
    );
  };
  showPublish.onclick = action(showPublish, error, async () => {
    const p = await publishInfo(b.id);
    showKey(p.whip_path);
  });

  const end = el('button', { type: 'button', className: 'danger', textContent: '방송 종료' });
  end.onclick = action(end, error, async () => {
    if (!confirm('방송을 종료할까요? 종료한 방송은 다시 송출할 수 없습니다.')) return;
    // 이 브라우저에서 송출 중이면 먼저 멈춘다(서버도 끊지만, 장치를 바로 끄려고)
    await publisher.stop('방송을 종료했습니다');
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
  // 방송 기록 요약(#110): 방송이 끝나면 한 번 불러와 보여 준다
  const summaryBox = el('p', { className: 'broadcast-summary', hidden: true });
  let summaryLoaded = false;
  const loadSummary = () => {
    if (summaryLoaded) return;
    summaryLoaded = true;
    getSummary(b.id)
      .then((s) => {
        const parts = [
          `방송 시간 ${s.duration_secs === null ? '-' : clock(s.duration_secs)}`,
          `최고 시청자 ${s.peak_viewers}명`,
          `채팅 ${s.chat_count}개`,
          `후원 ${s.donation_count}건 ${won(s.donation_total)}`,
        ];
        summaryBox.textContent = `방송 기록: ${parts.join(' · ')}`;
        summaryBox.hidden = false;
      })
      .catch(() => (summaryLoaded = false));
  };
  const update = (nb: Broadcast) => {
    b = nb;
    // 예정 시각은 대기 중일 때만 정한다(#114)
    scheduleRow.hidden = nb.status !== 'ready';
    const ended = nb.status === 'ended';
    showPublish.hidden = ended;
    end.hidden = ended;
    endedNote.hidden = !ended;
    if (ended) publishBox.replaceChildren();
    if (ended) loadSummary();
  };
  const panel = el(
    'section',
    { className: 'panel owner' },
    el('h3', { textContent: '방송 관리 (소유자)' }),
    title,
    description,
    category,
    el('div', { className: 'row' }, save, saved, showPublish, end, del),
    scheduleRow,
    endedNote,
    summaryBox,
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
    // 정상이면 아무것도 보이지 않는다. 문제가 있을 때만 알린다.
    .then((h: { status: string; db: string }) => (status.textContent = h.db === 'ok' ? '' : `서버 ${h.status}, DB ${h.db}`))
    .catch(() => (status.textContent = '서버에 연결하지 못했습니다. 잠시 뒤 새로고침해 주세요.'));

  // 토스 결제창에서 돌아온 주소(/donate/success?..., /donate/fail?...)는 주소 # 대신 경로로 온다(#41)
  if (location.pathname === '/donate/success' || location.pathname === '/donate/fail') {
    await donateReturnPage(main);
    return;
  }
  const hash = location.hash || '#/';
  if (hash !== '#/login') backAfterLogin = hash;
  const detail = /^#\/b\/(\d+)$/.exec(hash);
  const replay = /^#\/r\/(\d+)$/.exec(hash);
  const clip = /^#\/c\/(\d+)$/.exec(hash);
  if (hash === '#/login') loginPage(main);
  else if (hash === '#/new') newPage(main);
  else if (detail) await detailPage(main, Number(detail[1]), gen);
  else if (hash === '#/recordings') await recordingsPage(main, gen);
  else if (replay) await replayPage(main, Number(replay[1]), gen);
  else if (clip) await clipPage(main, Number(clip[1]), gen);
  else await listPage(main);
}

window.addEventListener('hashchange', route);
route();
