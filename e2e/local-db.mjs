// 로컬 브라우저 시험이 DB에 직접 SQL을 넣을 때 쓰는 컨테이너 이름.
// docker compose는 저장소 폴더 이름으로 컨테이너 이름을 짓는다(예: 폴더 onair → onair-db-1). 다르면 E2E_DB_CONTAINER로 알려 준다.
import { basename, dirname, resolve } from 'path';
import { fileURLToPath } from 'url';

const folder = basename(resolve(dirname(fileURLToPath(import.meta.url)), '..'))
  .toLowerCase()
  .replace(/[^a-z0-9_-]/g, '');
export const DB_CONTAINER = process.env.E2E_DB_CONTAINER || `${folder}-db-1`;
