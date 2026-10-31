import type { CollectionDef, FieldDef } from '@sdk';
import { db } from '../session';
import { Card, CopyButton } from '../ui';

const sample = (f: FieldDef): string => {
  switch (f.kind) {
    case 'number': return '123';
    case 'bool': return 'true';
    case 'json': return '{ }';
    case 'relation': return "'RECORD_ID'";
    case 'file': return 'fileInput.files[0]';
    default: return `'example ${f.name}'`;
  }
};

function Snippet({ title, code }: { title: string; code: string }) {
  return (
    <Card className="overflow-hidden">
      <div className="flex items-center justify-between border-b border-line px-4 py-2">
        <h3 className="text-sm font-semibold">{title}</h3>
        <CopyButton text={code} />
      </div>
      <pre className="overflow-x-auto border-t border-line bg-raised p-4 text-[13px] leading-relaxed text-ink"><code>{code}</code></pre>
    </Card>
  );
}

export function CodeTab({ def }: { def: CollectionDef }) {
  const url = db.baseUrl;
  const n = def.name;
  const plain = def.fields.filter((f) => f.kind !== 'file');
  const files = def.fields.filter((f) => f.kind === 'file');
  const first = plain.find((f) => f.kind === 'text') ?? plain[0];
  const body = def.fields.map((f) => `  ${f.name}: ${sample(f)},`).join('\n');
  const filter = first ? (first.kind === 'number' ? `${first.name} >= 100` : first.kind === 'bool' ? `${first.name} == true` : `${first.name} ~ "acme"`) : 'created_at > 0';

  const ts = `import { PowerDB } from 'powerdb-sdk';

const db = new PowerDB('${url}', process.env.POWERDB_KEY); // an API key or a session token

// CREATE${files.length ? ': fields and files travel in one atomic call' : ''}
const record = await db.collection('${n}').create({
${body}
});

// READ: a page of the newest records matching a filter
const page = await db.collection('${n}')
  .where('${filter}')
  .sort('-created_at')
  .limit(20)
  .get();

// values that came from a user go through the template tag, which quotes them safely
const found = await db.collection('${n}').where\`${first?.name ?? 'id'} == \${userInput}\`.get();

// UPDATE and DELETE
await db.collection('${n}').update(record.id, { ${first ? `${first.name}: ${sample(first)}` : ''} });
await db.collection('${n}').delete(record.id);${files.length ? `\n\n// FILES: each file field comes back with a link that works for about a minute\nconst bytes = await db.download(record.${files[0]!.name});` : ''}`;

  const data = JSON.stringify(Object.fromEntries(plain.map((f) => [f.name, f.kind === 'number' ? 123 : f.kind === 'bool' ? true : f.kind === 'json' ? {} : `example ${f.name}`])));
  const createCurl = files.length
    ? `curl -X POST '${url}/api/v1/collections/${n}/records' \\\n  -H 'Authorization: Bearer $POWERDB_KEY' \\\n  -F 'data=${data};type=application/json' \\\n${files.map((f) => `  -F '${f.name}=@./${f.name}.file'`).join(' \\\n')}`
    : `curl -X POST '${url}/api/v1/collections/${n}/records' \\\n  -H 'Authorization: Bearer $POWERDB_KEY' \\\n  -H 'Content-Type: application/json' \\\n  -d '${data}'`;
  const rest = `${createCurl}

# list: filter, sort and page in the URL (the data part must come first when files are sent)
curl -G '${url}/api/v1/collections/${n}/records' \\
  -H 'Authorization: Bearer $POWERDB_KEY' \\
  --data-urlencode 'filter=${filter}' \\
  --data-urlencode 'sort=-created_at' \\
  --data-urlencode 'limit=20'

curl -X PATCH '${url}/api/v1/collections/${n}/records/RECORD_ID' \\
  -H 'Authorization: Bearer $POWERDB_KEY' -H 'Content-Type: application/json' \\
  -d '{${first ? `"${first.name}": ${first.kind === 'number' ? 1 : first.kind === 'bool' ? 'false' : '"changed"'}` : ''}}'

curl -X DELETE '${url}/api/v1/collections/${n}/records/RECORD_ID' -H 'Authorization: Bearer $POWERDB_KEY'`;

  return (
    <div className="mx-auto max-w-4xl space-y-5 p-6">
      <p className="text-sm text-muted">Generated from this collection's schema. Whether a call succeeds depends on the access rules and on the key or session you send.</p>
      <Snippet title="TypeScript / JavaScript (powerdb-sdk)" code={ts} />
      <Snippet title="HTTP (curl): works from any language" code={rest} />
    </div>
  );
}
