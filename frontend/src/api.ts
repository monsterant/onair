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

export const listBroadcasts = () => api<Broadcast[]>('GET', '/broadcasts');
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
