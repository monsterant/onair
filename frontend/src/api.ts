// 백엔드 API 호출 도우미. 화면은 같은 주소의 /api로 부른다(개발: vite 프록시, 배포: Caddy).
// 로그인 쿠키(HttpOnly)는 브라우저가 알아서 붙이므로 스크립트가 만지지 않는다.

export type User = { id: number; username: string };

export type Status = 'ready' | 'live' | 'ended';

export type Broadcast = {
  id: number;
  title: string;
  description: string;
  status: Status;
  stream_name: string;
  owner_id: number;
  owner_username: string;
  started_at: string | null;
  ended_at: string | null;
  created_at: string;
  /** 지금 보는 사람 수(#85). 방송 중이고 서버가 셀 수 있을 때만 숫자 */
  viewers?: number | null;
  /** 천천히 모드(초, 0이면 끔, #87) */
  slow_mode_secs: number;
};

export type PublishInfo = { stream_name: string; stream_key: string; whip_path: string };

/** 방송별 댓글(#37) */
export type Comment = {
  id: number;
  broadcast_id: number;
  author_id: number;
  author_username: string;
  body: string;
  created_at: string;
  updated_at: string | null;
};

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

export async function api<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: 'same-origin',
  });
  if (res.status === 204) return undefined as T;
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new ApiError(res.status, (data as { error?: string }).error ?? `요청 실패 (${res.status})`);
  }
  return data as T;
}

/** 지금 로그인한 사용자. 로그인하지 않았으면 null */
export async function currentUser(): Promise<User | null> {
  try {
    return await api<User>('GET', '/auth/me');
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) return null;
    throw e;
  }
}

export const signup = (username: string, password: string) =>
  api<User>('POST', '/auth/signup', { username, password });
export const login = (username: string, password: string) =>
  api<User>('POST', '/auth/login', { username, password });
export const logout = () => api<void>('POST', '/auth/logout');
/** 심사자마다 따로 쓰는 테스트 계정. 부를 때마다 새 계정이 만들어진다(#35). */
export const guestLogin = () => api<User>('POST', '/auth/guest');
/** 켜진 기능(#25 A7) */
export const getConfig = () => api<{ demo_login: boolean; donations: boolean }>('GET', '/config');

/** 방송 목록. q가 있으면 제목에 그 글자가 들어 있는 방송만(#96) */
export const listBroadcasts = (q = '') =>
  api<Broadcast[]>('GET', q.trim() ? `/broadcasts?q=${encodeURIComponent(q.trim())}` : '/broadcasts');
export const getBroadcast = (id: number) => api<Broadcast>('GET', `/broadcasts/${id}`);
export const createBroadcast = (title: string, description: string) =>
  api<Broadcast>('POST', '/broadcasts', { title, description });
export const updateBroadcast = (id: number, title: string, description: string) =>
  api<Broadcast>('PATCH', `/broadcasts/${id}`, { title, description });
export const deleteBroadcast = (id: number) => api<void>('DELETE', `/broadcasts/${id}`);
export const endBroadcast = (id: number) => api<Broadcast>('POST', `/broadcasts/${id}/end`);
export const publishInfo = (id: number) => api<PublishInfo>('GET', `/broadcasts/${id}/publish`);
/** 송출 키를 새로 만든다(#28). 옛 키로 송출 중이면 끊긴다. */
export const rotateStreamKey = (id: number) => api<PublishInfo>('POST', `/broadcasts/${id}/stream-key`);

export const listComments = (broadcastId: number) => api<Comment[]>('GET', `/broadcasts/${broadcastId}/comments`);
/** 실시간 채팅 한 줄(#86). kind가 "donation"이면 후원 안내다. */
export type ChatMessage = { id: number; kind: 'chat' | 'donation'; user_id: number | null; username: string; text: string; at: string };

/** 시청자 제한(#87). until이 null이면 해제할 때까지 */
export type Ban = { user_id: number; username: string; until: string | null };
export const listBans = (broadcastId: number) => api<Ban[]>('GET', `/broadcasts/${broadcastId}/bans`);
export const banUser = (broadcastId: number, userId: number, tenMinutes: boolean) =>
  api<void>('PUT', `/broadcasts/${broadcastId}/bans/${userId}`, { ten_minutes: tenMinutes });
export const unbanUser = (broadcastId: number, userId: number) =>
  api<void>('DELETE', `/broadcasts/${broadcastId}/bans/${userId}`);
export const setSlowMode = (broadcastId: number, secs: number) => api<void>('PUT', `/broadcasts/${broadcastId}/slow`, { secs });
export const sendChat = (broadcastId: number, text: string) => api<void>('POST', `/broadcasts/${broadcastId}/chat`, { text });
/** 채팅 받기 주소(SSE). 브라우저의 EventSource로 연다. */
export const chatStreamUrl = (broadcastId: number) => `/api/broadcasts/${broadcastId}/chat/stream`;

export const createComment = (broadcastId: number, body: string) =>
  api<Comment>('POST', `/broadcasts/${broadcastId}/comments`, { body });
export const updateComment = (id: number, body: string) => api<Comment>('PATCH', `/comments/${id}`, { body });
export const deleteComment = (id: number) => api<void>('DELETE', `/comments/${id}`);

/** 도네이션(#41) */
export type DonationItem = { id: number; donor_username: string; amount: number; message: string; paid_at: string | null };
export type DonationSummary = { total: number; count: number; items: DonationItem[] };
export type Checkout = { order_id: string; order_name: string; amount: number; client_key: string; customer_key: string };
export type DonationStatus = { id: number; order_id: string; broadcast_id: number; amount: number; message: string; status: 'pending' | 'paid' | 'failed' };

export const listDonations = (broadcastId: number) => api<DonationSummary>('GET', `/broadcasts/${broadcastId}/donations`);
export const createDonation = (broadcastId: number, amount: number, message: string) =>
  api<Checkout>('POST', `/broadcasts/${broadcastId}/donations`, { amount, message });
export const confirmDonation = (paymentKey: string, orderId: string, amount: number) =>
  api<DonationStatus>('POST', '/donations/confirm', { paymentKey, orderId, amount });

/** 녹화·다시보기(#47) */
export type Recording = {
  id: number;
  broadcast_id: number;
  title: string;
  owner_username: string;
  format: 'mp4' | 'webm';
  started_at: string;
  ended_at: string | null;
  bytes: number;
  /** 재생 주소(/vod/...). 화면에서는 /api를 붙인다 */
  url: string;
};

export const listRecordings = () => api<Recording[]>('GET', '/recordings');
export const getRecording = (id: number) => api<Recording>('GET', `/recordings/${id}`);
export const listBroadcastRecordings = (broadcastId: number) => api<Recording[]>('GET', `/broadcasts/${broadcastId}/recordings`);
