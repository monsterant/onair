// 백엔드 API 호출 도우미. 화면은 같은 주소의 /api로 부른다(개발: vite 프록시, 배포: Caddy).
// 로그인 쿠키(HttpOnly)는 브라우저가 알아서 붙이므로 스크립트가 만지지 않는다.

export type User = { id: number; username: string };

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
export const demoLogin = (n: 1 | 2) => api<User>('POST', `/auth/demo/${n}`);
