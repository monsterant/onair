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
  type Comment,
  type Recording,
  getRecording,
  listBroadcastRecordings,
  listRecordings,
  type Status,
  confirmDonation,
  createDonation,
  listDonations,
  createComment,
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
  login,
  logout,
  publishInfo,
  signup,
  updateBroadcast,
} from './api';
// 플레이어(OvenPlayer+hls.js, 약 1MB)는 상세 화면에서만 불러온다. 목록 화면을 가볍게 하려는 것이다.
const player = () => import('./player');
import { mountWidget } from './toss';
import { CancelledError, type Facing, Publisher, type PublishState, cameraAndMic, keepScreenOn, screenAndMic } from './publisher';

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
  right.append(el('a', { href: '#/recordings', className: 'button ghost', textContent: '다시보기' }));
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

  // 심사자마다 따로 쓰는 테스트 계정(#35). 공용 계정이면 방송 수 한도·송출 키·삭제가 심사자끼리 섞인다.
  const demoError = el('p', { className: 'error', role: 'alert' });
  const guestBtn = el('button', { type: 'button', textContent: '테스트 계정으로 바로 시작' });
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
    studio?.update(b);
  };

  main.append(
    el('div', { className: 'detail-head' }, statusBox, titleEl),
    el('p', { className: 'hint', textContent: `방송자: ${b.owner_username}` }),
    playerBox,
    offAir,
    descEl,
  );
  // 이 화면에서 쓰는 송출기(소유자만). 화면을 떠나면 송출을 멈춘다.
  const publisher = isOwner ? new Publisher() : null;
  const owner = publisher ? ownerPanel(b, (nb) => { b = nb; paint(); }, publisher) : null;
  const studio = publisher ? studioPanel(() => b, publisher) : null;
  if (studio) main.append(studio.el);
  if (owner) main.append(owner.el);
  main.append(recordingsPanel(id), donationsPanel(b, gen), commentsPanel(id, gen));
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
        // 이 화면에서 송출 중이었으면 화면 공유·마이크도 바로 끈다(#30 P3)
        void publisher?.stop('방송이 삭제됐습니다');
        main.replaceChildren(el('p', { className: 'hint', textContent: '삭제된 방송입니다.' }), el('a', { href: '#/', textContent: '목록으로' }));
      }
    }
  }, 5_000);
  cleanup.push(stop);
  if (publisher) cleanup.push(() => void publisher.stop());
}

// ---------- 녹화·다시보기(#47) ----------

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
    el('div', { className: 'player-box' }, video),
    el('p', {}, el('a', { href: `#/b/${r.broadcast_id}`, textContent: '방송 화면으로' }), ' · ', el('a', { href: '#/recordings', textContent: '다시보기 목록' })),
  );
  // 화면을 떠나면 영상 내려받기를 멈춘다
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
 * 방송 상세 화면의 후원 칸. 누구나 합계와 내역을 본다. 로그인한 다른 사용자는 후원한다(자기 방송은 안 됨).
 * 후원하기 → 서버가 주문을 만든다 → 토스 결제위젯 → 결제창 → /donate/success로 돌아와 서버가 승인을 확인한다.
 */
function donationsPanel(b: Broadcast, gen: number): HTMLElement {
  const total = el('strong', { className: 'donation-total' });
  const list = el('ul', { className: 'donations' });
  const panel = el('section', { className: 'donations-panel panel' }, el('h3', {}, '후원 ', total), list);
  let stopped = false;

  const load = async () => {
    try {
      const s = await listDonations(b.id);
      if (stopped || gen !== routeGen) return;
      total.textContent = `${won(s.total)} · ${s.count}건`;
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
      const w = await mountWidget(checkout, b.id, '#toss-methods', '#toss-agreement').catch((e) => {
        widgetBox.hidden = true;
        throw tossError(e);
      });
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
function commentsPanel(broadcastId: number, gen: number): HTMLElement {
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
      preview.srcObject = stream;
      preview.hidden = false;
      releases.push(keepScreenOn());
      afterStart(stream);
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
    void begin(
      async (register) => {
        const t = await screenAndMic(register, withMic.checked);
        screenAudio = t.screenAudio;
        return t;
      },
      '화면 공유가 끝났습니다',
      () => {
        if (screenAudio === 0) {
          warn.textContent = '화면 소리가 함께 가지 않습니다. 공유 창에서 "탭 오디오 공유" 또는 "시스템 오디오 공유"를 켰는지 확인하세요(Chrome·Edge만 되고, "창" 공유에는 소리 칸이 없습니다).';
        }
      },
    );
  };
  startCamera.onclick = () => {
    const f = facing.value as Facing;
    let mic: MediaStreamTrack | undefined;
    void begin(
      async (register) => {
        const t = await cameraAndMic(register, f);
        mic = t.audio[0];
        return t;
      },
      '카메라가 꺼져 송출을 멈췄습니다',
      (stream) => {
        const video = stream.getVideoTracks()[0];
        // 경고는 종류별로 따로 들고 이어 붙여 보여 준다. 하나가 풀려도 다른 경고가 지워지지 않게(#33 2바퀴).
        const warns = { facing: '', mute: '', mic: '' };
        const showWarns = () => {
          if (publisher.state !== 'idle') warn.textContent = Object.values(warns).filter(Boolean).join(' ');
        };
        // 실제로 켜진 카메라 방향(브라우저가 알려 줄 때). 고른 카메라가 없으면 브라우저가 다른 카메라를 켠다(#33 M4).
        const actual = video?.getSettings().facingMode as Facing | undefined;
        if (actual && actual !== f) {
          warns.facing = `고른 ${f === 'user' ? '앞' : '뒤'} 카메라가 없어 다른 카메라로 보냅니다.`;
        }
        // 앞 카메라는 거울처럼 보여야 자연스럽다. 미리보기만 뒤집는다(시청자에게 가는 영상은 그대로).
        preview.classList.toggle('mirror', (actual ?? f) === 'user');

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
    screenRow,
    cameraRow,
    el('div', { className: 'row' }, stop, state),
    preview,
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

function ownerPanel(b: Broadcast, onChange: (b: Broadcast) => void, publisher: Publisher): { el: HTMLElement; update: (b: Broadcast) => void } {
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

  // 토스 결제창에서 돌아온 주소(/donate/success?..., /donate/fail?...)는 주소 # 대신 경로로 온다(#41)
  if (location.pathname === '/donate/success' || location.pathname === '/donate/fail') {
    await donateReturnPage(main);
    return;
  }
  const hash = location.hash || '#/';
  const detail = /^#\/b\/(\d+)$/.exec(hash);
  const replay = /^#\/r\/(\d+)$/.exec(hash);
  if (hash === '#/login') loginPage(main);
  else if (hash === '#/new') newPage(main);
  else if (detail) await detailPage(main, Number(detail[1]), gen);
  else if (hash === '#/recordings') await recordingsPage(main, gen);
  else if (replay) await replayPage(main, Number(replay[1]), gen);
  else await listPage(main);
}

window.addEventListener('hashchange', route);
route();
