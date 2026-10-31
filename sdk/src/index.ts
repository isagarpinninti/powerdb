/**
 * PowerDB client. No dependencies: it uses `fetch`, `FormData` and `Blob`, which Node 18+ and every browser provide.
 *
 *   const db = new PowerDB('https://db.example.com', process.env.POWERDB_KEY);
 *   const invoice = await db.collection('invoices').create({ client: 'Acme', amount: 4500, document: file });
 *   const page = await db.collection('invoices').where('status == "paid" && amount >= 1000').sort('-created_at').limit(20).get();
 */

export type Fields = Record<string, unknown>;

export interface FileInfo {
  name: string | null;
  size: number;
  mime: string;
  /** Present when the caller may view the record: a link that works for about a minute. Relative to the server; `PowerDB.fileUrl` makes it absolute. */
  url?: string;
}

export type PowerRecord<T extends Fields = Fields> = T & { id: string; collection: string; created_at: number; updated_at: number };

export interface Page<T extends Fields = Fields> {
  items: PowerRecord<T>[];
  limit: number;
  offset: number;
  has_more: boolean;
  /** Known when the whole result was examined; null when the server stopped early because the page was full. */
  total: number | null;
}

export class PowerDBError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = 'PowerDBError';
  }
}

/** Anything that can be sent as a file. Node `Buffer`s are `Uint8Array`s. */
export type FileLike = Blob | Uint8Array | ArrayBuffer;

function isFileLike(v: unknown): v is FileLike {
  return (typeof Blob !== 'undefined' && v instanceof Blob) || v instanceof Uint8Array || v instanceof ArrayBuffer;
}

/** Quotes a value for use inside a filter expression: strings are escaped, numbers and booleans pass through, null stays null. */
export function quote(v: unknown): string {
  if (v === null || v === undefined) return 'null';
  if (typeof v === 'number') {
    if (!Number.isFinite(v)) throw new TypeError('a filter value must be a finite number');
    return String(v);
  }
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  if (Array.isArray(v)) return '[' + v.map(quote).join(', ') + ']';
  if (typeof v === 'string') {
    // the filter language understands \\ \" \' \n \t; any other control character cannot be written, so it is dropped rather than sent half-escaped
    let out = '"';
    for (const ch of v) {
      const c = ch.codePointAt(0)!;
      if (ch === '\\') out += '\\\\';
      else if (ch === '"') out += '\\"';
      else if (ch === '\n') out += '\\n';
      else if (ch === '\t') out += '\\t';
      else if (c < 0x20 || c === 0x7f) continue;
      else out += ch;
    }
    return out + '"';
  }
  throw new TypeError(`cannot use a ${typeof v} in a filter`);
}

export class PowerDB {
  readonly baseUrl: string;
  private token: string | undefined;
  readonly auth: Auth;
  readonly admin: Admin;

  /** `key` is an API key (`pdb_live_...`) or a session token from `auth.login`. */
  constructor(baseUrl: string, key?: string) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.token = key;
    this.auth = new Auth(this);
    this.admin = new Admin(this);
  }

  setToken(token: string | undefined): void {
    this.token = token;
  }

  collection<T extends Fields = Fields>(name: string): Collection<T> {
    return new Collection<T>(this, name);
  }

  /** The absolute address of a file's download link. */
  fileUrl(file: FileInfo | undefined): string | undefined {
    if (!file?.url) return undefined;
    return /^https?:\/\//.test(file.url) ? file.url : this.baseUrl + file.url;
  }

  /** Downloads a file through its signed link (no credentials are sent). */
  async download(file: FileInfo | undefined): Promise<Uint8Array> {
    const url = this.fileUrl(file);
    if (!url) throw new PowerDBError(403, 'forbidden', 'this file has no download link: the caller may not view the record');
    const res = await fetch(url);
    if (!res.ok) throw await toError(res);
    return new Uint8Array(await res.arrayBuffer());
  }

  /** @internal */
  async request(method: string, path: string, body?: unknown, auth = true): Promise<Response> {
    const headers: Record<string, string> = {};
    if (auth && this.token) headers['authorization'] = `Bearer ${this.token}`;
    let payload: BodyInit | undefined;
    if (body instanceof FormData) payload = body;
    else if (body !== undefined) {
      headers['content-type'] = 'application/json';
      payload = JSON.stringify(body);
    }
    const res = await fetch(this.baseUrl + path, { method, headers, body: payload });
    if (!res.ok) throw await toError(res);
    return res;
  }

  /** @internal */
  async json<T>(method: string, path: string, body?: unknown): Promise<T> {
    return (await (await this.request(method, path, body)).json()) as T;
  }
}

async function toError(res: Response): Promise<PowerDBError> {
  try {
    const j = (await res.json()) as { error?: { code?: string; message?: string } };
    return new PowerDBError(res.status, j.error?.code ?? 'error', j.error?.message ?? res.statusText);
  } catch {
    return new PowerDBError(res.status, 'error', res.statusText || `HTTP ${res.status}`);
  }
}

export interface User {
  id: string;
  email: string;
  role: 'admin' | 'user';
  created_at: number;
}

class Auth {
  constructor(private readonly db: PowerDB) {}

  async register(email: string, password: string): Promise<User> {
    return (await this.db.json<{ user: User }>('POST', '/api/v1/auth/register', { email, password })).user;
  }

  /** Signs in and keeps the session token for later calls. */
  async login(email: string, password: string): Promise<{ token: string; user: User }> {
    const r = await this.db.json<{ token: string; user: User }>('POST', '/api/v1/auth/login', { email, password });
    this.db.setToken(r.token);
    return r;
  }

  async me(): Promise<{ kind: 'user' | 'key'; user?: User }> {
    return this.db.json('GET', '/api/v1/auth/me');
  }

  async refresh(): Promise<void> {
    const r = await this.db.json<{ token: string }>('POST', '/api/v1/auth/refresh');
    this.db.setToken(r.token);
  }

  logout(): void {
    this.db.setToken(undefined);
  }
}

export type Rule = string | null | undefined;

export interface FieldDef {
  name: string;
  kind: 'text' | 'number' | 'bool' | 'json' | 'relation' | 'file';
  required?: boolean;
  /** text: longest value in bytes; file: largest file in bytes */
  max?: number;
  /** file: allowed content types, exact or by family (`image/*`) */
  mime?: string[];
  relation?: string;
}

export interface CollectionDef {
  name: string;
  fields: FieldDef[];
  /** One expression per action. Leave out for "administrators only"; `""` makes it public. */
  rules?: { list?: Rule; view?: Rule; create?: Rule; update?: Rule; delete?: Rule };
  /** Encrypt this collection's files at rest (private files). Cannot be changed later. */
  encrypt?: boolean;
}

export interface ApiKeyMeta {
  id: string;
  name: string;
  access: 'read' | 'write' | 'read_write';
  collections: string[];
  admin: boolean;
  created_at: number;
}

/** Administrator operations. They need an administrator's session or an admin API key. */
class Admin {
  constructor(private readonly db: PowerDB) {}

  readonly collections = {
    list: async (): Promise<CollectionDef[]> => (await this.db.json<{ items: CollectionDef[] }>('GET', '/api/v1/collections')).items,
    get: async (name: string): Promise<{ collection: CollectionDef; records: number }> => this.db.json('GET', `/api/v1/collections/${encodeURIComponent(name)}`),
    create: async (def: CollectionDef): Promise<CollectionDef> => (await this.db.json<{ collection: CollectionDef }>('POST', '/api/v1/collections', def)).collection,
    update: async (def: CollectionDef): Promise<CollectionDef> => (await this.db.json<{ collection: CollectionDef }>('PATCH', `/api/v1/collections/${encodeURIComponent(def.name)}`, def)).collection,
    drop: async (name: string): Promise<number> => (await this.db.json<{ deleted_records: number }>('DELETE', `/api/v1/collections/${encodeURIComponent(name)}`)).deleted_records,
  };

  readonly keys = {
    list: async (): Promise<ApiKeyMeta[]> => (await this.db.json<{ items: ApiKeyMeta[] }>('GET', '/api/v1/keys')).items,
    /** The returned `key` is shown only once. */
    create: async (o: { name: string; access?: 'read' | 'write' | 'read_write'; collections?: string[]; admin?: boolean }): Promise<{ key: string; meta: ApiKeyMeta }> => this.db.json('POST', '/api/v1/keys', o),
    revoke: async (id: string): Promise<void> => void (await this.db.request('DELETE', `/api/v1/keys/${encodeURIComponent(id)}`)),
  };

  readonly users = {
    list: async (): Promise<User[]> => (await this.db.json<{ items: User[] }>('GET', '/api/v1/users')).items,
    create: async (o: { email: string; password: string; role?: 'admin' | 'user' }): Promise<User> => (await this.db.json<{ user: User }>('POST', '/api/v1/users', o)).user,
    delete: async (id: string): Promise<void> => void (await this.db.request('DELETE', `/api/v1/users/${encodeURIComponent(id)}`)),
  };

  stats(): Promise<{ segments: number; log_bytes: number; dead_bytes: number; gc_pending: number }> {
    return this.db.json('GET', '/api/v1/stats');
  }
  /** Releases deleted files' space now instead of waiting for the background pass. */
  gc(): Promise<{ files_released: number; bytes_reclaimed: number; segments_compacted: number }> {
    return this.db.json('POST', '/api/v1/maintenance/gc');
  }
}

export interface WriteOptions {
  /** Names of file fields to detach (update only). */
  removeFiles?: string[];
}

/** Splits an object into the JSON fields and the file fields. A file may be a Blob/File, bytes, or `{ file, name, type }`. */
function split(data: Fields): { fields: Fields; files: Array<[string, Blob, string | undefined]> } {
  const fields: Fields = {};
  const files: Array<[string, Blob, string | undefined]> = [];
  for (const [k, v] of Object.entries(data)) {
    if (v && typeof v === 'object' && !isFileLike(v) && 'file' in (v as object) && isFileLike((v as { file: unknown }).file)) {
      const o = v as { file: FileLike; name?: string; type?: string };
      files.push([k, toBlob(o.file, o.type), o.name]);
    } else if (isFileLike(v)) files.push([k, toBlob(v), (v as { name?: string }).name]);
    else fields[k] = v;
  }
  return { fields, files };
}

function toBlob(v: FileLike, type?: string): Blob {
  if (typeof Blob !== 'undefined' && v instanceof Blob) return type && !v.type ? v.slice(0, v.size, type) : v;
  return new Blob([v as BlobPart], type ? { type } : {});
}

function body(fields: Fields, files: Array<[string, Blob, string | undefined]>, extra?: Fields): FormData | Fields {
  const all = { ...fields, ...extra };
  if (files.length === 0) return all;
  const form = new FormData();
  // the `data` part must come first: the server authorizes the request from it before it accepts any file
  form.append('data', new Blob([JSON.stringify(all)], { type: 'application/json' }));
  for (const [name, blob, filename] of files) form.append(name, blob, filename ?? name);
  return form;
}

export class Collection<T extends Fields = Fields> {
  constructor(
    private readonly db: PowerDB,
    readonly name: string,
  ) {}

  private path(id?: string): string {
    return `/api/v1/collections/${encodeURIComponent(this.name)}/records${id ? '/' + encodeURIComponent(id) : ''}`;
  }

  /** Creates a record. Any value that is a Blob, File, Buffer or `{ file, name, type }` is uploaded as a file in the same call. */
  async create(data: Fields): Promise<PowerRecord<T>> {
    const { fields, files } = split(data);
    return this.db.json('POST', this.path(), body(fields, files));
  }

  async get(id: string): Promise<PowerRecord<T>> {
    return this.db.json('GET', this.path(id));
  }

  /** Changes the given fields; other fields stay. A `null` value removes a field. A file value replaces that file. */
  async update(id: string, patch: Fields, opts: WriteOptions = {}): Promise<PowerRecord<T>> {
    const { fields, files } = split(patch);
    return this.db.json('PATCH', this.path(id), body(fields, files, opts.removeFiles ? { _remove_files: opts.removeFiles } : undefined));
  }

  async delete(id: string): Promise<void> {
    await this.db.request('DELETE', this.path(id));
  }

  query(): Query<T> {
    return new Query<T>(this.db, this.path());
  }

  /** `where('status == "paid"')`, or with a template tag that quotes values safely: `where\`status == ${userInput}\``. */
  where(expr: string | TemplateStringsArray, ...values: unknown[]): Query<T> {
    return this.query().where(expr, ...values);
  }
  sort(spec: string): Query<T> {
    return this.query().sort(spec);
  }
  limit(n: number): Query<T> {
    return this.query().limit(n);
  }
  all(): Promise<PowerRecord<T>[]> {
    return this.query().all();
  }
  list(): Promise<Page<T>> {
    return this.query().get();
  }
}

export class Query<T extends Fields = Fields> {
  private filters: string[] = [];
  private sortSpec = '';
  private lim?: number;
  private off?: number;

  constructor(
    private readonly db: PowerDB,
    private readonly path: string,
  ) {}

  /** Adds a condition; several calls are combined with &&. Use the template-tag form for any text that came from a user. */
  where(expr: string | TemplateStringsArray, ...values: unknown[]): this {
    const text = typeof expr === 'string' ? expr : expr.reduce((acc, part, i) => acc + part + (i < values.length ? quote(values[i]) : ''), '');
    this.filters.push(`(${text})`);
    return this;
  }
  /** `'-created_at'` newest first, `'title'` A to Z, `'-amount,title'` several keys. */
  sort(spec: string): this {
    this.sortSpec = spec;
    return this;
  }
  limit(n: number): this {
    this.lim = n;
    return this;
  }
  offset(n: number): this {
    this.off = n;
    return this;
  }

  private url(offset?: number): string {
    const p = new URLSearchParams();
    if (this.filters.length) p.set('filter', this.filters.join(' && '));
    if (this.sortSpec) p.set('sort', this.sortSpec);
    if (this.lim !== undefined) p.set('limit', String(this.lim));
    const o = offset ?? this.off;
    if (o) p.set('offset', String(o));
    const q = p.toString();
    return this.path + (q ? '?' + q : '');
  }

  async get(): Promise<Page<T>> {
    return this.db.json('GET', this.url());
  }

  async first(): Promise<PowerRecord<T> | undefined> {
    const was = this.lim;
    this.lim = 1;
    try {
      return (await this.get()).items[0];
    } finally {
      this.lim = was;
    }
  }

  /** Every match, fetched page by page. */
  async all(pageSize = 200): Promise<PowerRecord<T>[]> {
    const out: PowerRecord<T>[] = [];
    const was = this.lim;
    this.lim = pageSize;
    try {
      for (let offset = this.off ?? 0; ; offset += pageSize) {
        const page = await this.db.json<Page<T>>('GET', this.url(offset));
        out.push(...page.items);
        if (!page.has_more) return out;
      }
    } finally {
      this.lim = was;
    }
  }
}
