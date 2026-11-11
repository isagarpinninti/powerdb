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
  /** Buckets and the objects (files) in them: `db.buckets.create('docs')`, `db.bucket('docs').put('a/b.pdf', bytes)`. */
  readonly buckets: Buckets;

  /** `key` is an API key (`pdb_live_...`) or a session token from `auth.login`. */
  constructor(baseUrl: string, key?: string) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.token = key;
    this.auth = new Auth(this);
    this.admin = new Admin(this);
    this.buckets = new Buckets(this);
  }

  /** A bucket to read and write objects in. It does not check that the bucket exists until you use it. */
  bucket(name: string): BucketHandle {
    return new BucketHandle(this, name);
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

  /** @internal A request with a body of the caller's own (raw bytes, a stream) and headers. */
  async raw(
    method: string,
    path: string,
    init: { headers?: Record<string, string>; body?: BodyInit; duplex?: 'half' } = {},
  ): Promise<Response> {
    const headers: Record<string, string> = { ...(init.headers ?? {}) };
    if (this.token) headers['authorization'] = `Bearer ${this.token}`;
    const res = await fetch(this.baseUrl + path, { method, headers, body: init.body, ...(init.duplex ? { duplex: init.duplex } : {}) } as RequestInit);
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

  /** The sign-in providers the server offers ("google", "github", ...). */
  async providers(): Promise<string[]> {
    return (await this.db.json<{ providers: string[] }>('GET', '/api/v1/auth/oauth/providers')).providers;
  }

  /**
   * Where to send the browser to sign in with a provider. `redirect` must be one of the URLs the server's settings allow;
   * the browser comes back there with `#token=...` (or `#error=...`) in the URL fragment: pass that to `finishOAuth`.
   */
  oauthUrl(provider: string, redirect?: string): string {
    const q = redirect ? `?redirect=${encodeURIComponent(redirect)}` : '';
    return `${this.db.baseUrl}/api/v1/auth/oauth/${encodeURIComponent(provider)}/start${q}`;
  }

  /**
   * Reads the result of an OAuth sign-in from `location.hash`, keeps the session token, and returns it. Throws with the
   * server's reason (`no_account`, `unverified_email`, `access_denied`, ...) when sign-in did not happen; returns null
   * when the fragment holds no sign-in result at all.
   */
  finishOAuth(hash: string): string | null {
    const p = new URLSearchParams(hash.replace(/^#/, ''));
    const error = p.get('error');
    if (error) throw new PowerDBError(401, error, `sign-in failed: ${error}`);
    const token = p.get('token');
    if (!token) return null;
    this.db.setToken(token);
    return token;
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
  /** Takes a consistent backup of the running database (writers are not blocked). Written on the server, under its backup directory. */
  backup(opts: { name?: string; includeSecret?: boolean; deep?: boolean } = {}): Promise<{ name: string; files: number; bytes: number; includes_secret: boolean }> {
    return this.db.json('POST', '/api/v1/maintenance/backups', { name: opts.name, include_secret: opts.includeSecret, deep: opts.deep });
  }

  backups(): Promise<{ backups: { name: string; created_at: number; files: number; bytes: number; includes_secret: boolean; powerdb_version: string }[] }> {
    return this.db.json('GET', '/api/v1/maintenance/backups');
  }

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


// ---- buckets and objects ----

export interface BucketInfo {
  name: string;
  /** Every put is kept as a version; a delete hides the key and keeps the data. */
  versioning: boolean;
  /** The bucket's file data is encrypted at rest. */
  encrypt: boolean;
  created_at: number;
}

/** One version of an object. A delete marker has no file (`delete_marker` is true and `size` is 0). */
export interface ObjectInfo {
  /** Names the object across its versions. */
  id: string;
  bucket: string;
  key: string;
  version_id: string;
  delete_marker: boolean;
  size: number;
  mime: string;
  filename?: string;
  /** BLAKE3 of the content, hex. */
  checksum: string;
  created_at: number;
  metadata: Record<string, string>;
}

export interface PutOptions {
  /** The content type. Default: `application/octet-stream`. The server refuses a file whose bytes contradict it (a program called image/png). */
  type?: string;
  /** The name a browser should save it as. */
  filename?: string;
  /** Custom metadata: names of letters, digits, `_`, `.`, `-`; at most 32 entries. */
  metadata?: Record<string, string>;
}

export interface ReadOptions {
  /** A specific version instead of the newest. */
  version?: string;
  /** Only these bytes, inclusive (`{ start: 0, end: 1023 }` is the first KiB). */
  range?: { start: number; end?: number };
}

export interface ListOptions {
  prefix?: string;
  /** The `next` of the previous page. */
  after?: string;
  limit?: number;
}

export interface ObjectPage {
  items: ObjectInfo[];
  /** Pass as `after` for the next page; absent on the last. */
  next?: string;
  limit: number;
}

const encKey = (key: string): string => key.split('/').map(encodeURIComponent).join('/');

class Buckets {
  constructor(private readonly db: PowerDB) {}

  async list(): Promise<BucketInfo[]> {
    return (await this.db.json<{ items: BucketInfo[] }>('GET', '/api/v1/buckets')).items;
  }

  async create(name: string, opts: { versioning?: boolean; encrypt?: boolean } = {}): Promise<BucketInfo> {
    return (await this.db.json<{ bucket: BucketInfo }>('POST', '/api/v1/buckets', { name, ...opts })).bucket;
  }

  async get(name: string): Promise<BucketInfo> {
    return (await this.db.json<{ bucket: BucketInfo }>('GET', `/api/v1/buckets/${encodeURIComponent(name)}`)).bucket;
  }

  /** Turns versioning on. It cannot be turned off. */
  async enableVersioning(name: string): Promise<BucketInfo> {
    return (await this.db.json<{ bucket: BucketInfo }>('PATCH', `/api/v1/buckets/${encodeURIComponent(name)}`, { versioning: true })).bucket;
  }

  /** Deletes an empty bucket (a bucket that holds any version of any key is refused). */
  async delete(name: string): Promise<void> {
    await this.db.request('DELETE', `/api/v1/buckets/${encodeURIComponent(name)}`);
  }
}

export class BucketHandle {
  constructor(
    private readonly db: PowerDB,
    readonly name: string,
  ) {}

  private path(key: string): string {
    return `/api/v1/buckets/${encodeURIComponent(this.name)}/objects/${encKey(key)}`;
  }

  /**
   * Stores `body` as the object `key`. A string, bytes, a Blob or a stream (a stream is sent without being held in memory). In a bucket
   * without versioning this replaces the object; with versioning it adds a version.
   */
  async put(key: string, body: string | Uint8Array | ArrayBuffer | Blob | ReadableStream<Uint8Array>, opts: PutOptions = {}): Promise<ObjectInfo> {
    const headers: Record<string, string> = { 'content-type': opts.type ?? 'application/octet-stream' };
    for (const [k, v] of Object.entries(opts.metadata ?? {})) headers[`x-powerdb-meta-${k}`] = v;
    const isStream = typeof ReadableStream !== 'undefined' && body instanceof ReadableStream;
    const q = opts.filename ? `?filename=${encodeURIComponent(opts.filename)}` : '';
    const res = await this.db.raw('PUT', this.path(key) + q, {
      headers,
      body: body as BodyInit,
      ...(isStream ? { duplex: 'half' as const } : {}),
    });
    return ((await res.json()) as { object: ObjectInfo }).object;
  }

  /** The raw response, for streaming a large object (`res.body`) or reading headers. Use `download` for the bytes. */
  async response(key: string, opts: ReadOptions = {}): Promise<Response> {
    const headers: Record<string, string> = {};
    if (opts.range) headers['range'] = `bytes=${opts.range.start}-${opts.range.end ?? ''}`;
    const q = opts.version ? `?version=${encodeURIComponent(opts.version)}` : '';
    return this.db.raw('GET', this.path(key) + q, { headers });
  }

  /** The object's bytes (or the requested range of them). */
  async download(key: string, opts: ReadOptions = {}): Promise<Uint8Array> {
    return new Uint8Array(await (await this.response(key, opts)).arrayBuffer());
  }

  /** What is stored under `key` without downloading it. */
  async head(key: string, opts: { version?: string } = {}): Promise<{ size: number; type: string; checksum: string; versionId: string; metadata: Record<string, string> }> {
    const q = opts.version ? `?version=${encodeURIComponent(opts.version)}` : '';
    const res = await this.db.raw('HEAD', this.path(key) + q);
    const metadata: Record<string, string> = {};
    res.headers.forEach((v, k) => {
      if (k.startsWith('x-powerdb-meta-')) metadata[k.slice('x-powerdb-meta-'.length)] = v;
    });
    return {
      size: Number(res.headers.get('content-length') ?? 0),
      type: res.headers.get('content-type') ?? '',
      checksum: (res.headers.get('etag') ?? '').replace(/"/g, ''),
      versionId: res.headers.get('x-powerdb-version-id') ?? '',
      metadata,
    };
  }

  /** Deletes `key` (in a versioned bucket this hides it); with `version`, removes that version for good. */
  async delete(key: string, opts: { version?: string } = {}): Promise<void> {
    const q = opts.version ? `?version=${encodeURIComponent(opts.version)}` : '';
    await this.db.raw('DELETE', this.path(key) + q);
  }

  /** One page of the newest version of each key, in key order. */
  async list(opts: ListOptions = {}): Promise<ObjectPage> {
    const p = new URLSearchParams();
    if (opts.prefix) p.set('prefix', opts.prefix);
    if (opts.after) p.set('after', opts.after);
    if (opts.limit) p.set('limit', String(opts.limit));
    const q = p.toString();
    const r = await this.db.json<{ items: ObjectInfo[]; next?: string | null; limit: number }>(
      'GET',
      `/api/v1/buckets/${encodeURIComponent(this.name)}/objects${q ? '?' + q : ''}`,
    );
    return { items: r.items, next: r.next ?? undefined, limit: r.limit };
  }

  /** Every object whose key starts with `prefix`, page by page. */
  async *each(prefix = ''): AsyncGenerator<ObjectInfo> {
    let after: string | undefined;
    for (;;) {
      const page = await this.list({ prefix, after, limit: 1000 });
      yield* page.items;
      if (!page.next) return;
      after = page.next;
    }
  }

  /** Every version of `key`, newest first, delete markers included. */
  async versions(key: string): Promise<ObjectInfo[]> {
    return (await this.db.json<{ items: ObjectInfo[] }>('GET', `/api/v1/buckets/${encodeURIComponent(this.name)}/versions?key=${encodeURIComponent(key)}`)).items;
  }

  /** Makes an old version the newest again (as a new version sharing its data). Only in a bucket with versioning. */
  async restore(key: string, version: string): Promise<ObjectInfo> {
    return (await this.db.json<{ object: ObjectInfo }>('POST', `/api/v1/buckets/${encodeURIComponent(this.name)}/restore`, { key, version })).object;
  }
}
