//! Files in typed tables: `FILE` and `FILE[]` columns. The thing to get right is the reference counting: a file must stay while any row, record or
//! object holds it and go when the last one lets go, through every way a row can change or disappear.

use powerdb::storage::chunker::ChunkSizes;
use powerdb::storage::objects::BucketDef;
use powerdb::storage::tables::Row;
use powerdb::types::schema::{Change, OnDelete, TableDef};
use powerdb::types::{FileValue, Type, Value};
use powerdb::util::new_id;
use powerdb::{Db, Durability, Error, Options};
use std::collections::{BTreeMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("powerdb-tf-{}", new_id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn options() -> Options {
    Options {
        durability: Durability::Never,
        segment_bytes: 1 << 20,
        chunk: ChunkSizes {
            min: 8 << 10,
            avg: 32 << 10,
            max: 128 << 10,
        },
        verify_reads: true,
        cache_bytes: None,
    }
}

fn open(d: &Dir) -> Db {
    Db::open(&d.0, options()).unwrap()
}

type Content = (u8, u8);

fn bytes_of((base, variant): Content) -> Vec<u8> {
    let mut x = (base as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v: Vec<u8> = (0..150_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    if variant > 0 {
        let at = 40_000 + variant as usize * 20_000;
        for b in &mut v[at..at + 64] {
            *b = b.wrapping_add(variant);
        }
    }
    v
}

fn i(n: i64) -> Value {
    Value::I64(n)
}

fn docs(db: &Db) {
    let mut d = TableDef::new("docs").unwrap();
    for (n, ty, nullable) in [
        ("id", Type::I64, false),
        ("cover", Type::File, true),
        ("pages", Type::Array(Box::new(Type::File)), true),
        ("note", Type::Text, true),
    ] {
        d = d
            .apply(
                &Change::AddColumn {
                    name: n.into(),
                    ty,
                    nullable,
                    default: None,
                },
                true,
            )
            .unwrap()
            .0;
    }
    d.set_primary_key(&["id"]).unwrap();
    db.create_table(d).unwrap();
}

fn stage(db: &Db, c: Content) -> powerdb::storage::PendingFile {
    db.stage_table_file(
        "docs",
        "application/octet-stream",
        Some("f.bin"),
        Cursor::new(bytes_of(c)),
    )
    .unwrap()
}

fn get<'a>(row: &'a Row, name: &str) -> &'a Option<Value> {
    &row.iter().find(|(n, _)| n == name).unwrap().1
}

fn drain(db: &Db) {
    while db.gc_step(100).unwrap().files_released > 0 {}
}

fn clean(db: &Db) {
    let r = db.verify(true).unwrap();
    assert!(r.problems.is_empty(), "{:?}", r.problems);
}

fn stored_files(db: &Db) -> u64 {
    db.verify(false).unwrap().files
}

fn read(db: &Db, v: &Option<Value>) -> Vec<u8> {
    let Some(Value::File(f)) = v else {
        panic!("{v:?}")
    };
    db.read_file(&f.id).unwrap()
}

/// Inserts a row with a cover and pages.
fn put(db: &Db, id: i64, cover: Option<Content>, pages: &[Content]) {
    let mut staged = Vec::new();
    let mut vals: Vec<(&str, Value)> = vec![("id", i(id))];
    if let Some(c) = cover {
        let p = stage(db, c);
        vals.push(("cover", Value::File(p.file_value())));
        staged.push(p);
    }
    if !pages.is_empty() {
        let mut items = Vec::new();
        for c in pages {
            let p = stage(db, *c);
            items.push(Value::File(p.file_value()));
            staged.push(p);
        }
        vals.push(("pages", Value::Array(Type::File, items)));
    }
    let refs: Vec<&powerdb::storage::PendingFile> = staged.iter().collect();
    db.insert_row_with("docs", &vals, &refs).unwrap();
}

// ---- storing and reading ----

#[test]
fn a_file_is_stored_with_a_row_and_read_back() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    put(&db, 1, Some((1, 0)), &[(2, 0), (3, 0)]);
    let row = db.get_row("docs", &[i(1)]).unwrap().unwrap();
    assert_eq!(read(&db, get(&row, "cover")), bytes_of((1, 0)));
    let Some(Value::File(f)) = get(&row, "cover") else {
        panic!()
    };
    assert_eq!(
        (f.size, f.mime.as_str(), f.name.as_deref()),
        (150_000, "application/octet-stream", Some("f.bin"))
    );
    let Some(Value::Array(_, pages)) = get(&row, "pages") else {
        panic!()
    };
    assert_eq!(pages.len(), 2);
    assert_eq!(read(&db, &Some(pages[1].clone())), bytes_of((3, 0)));
    clean(&db);
    // survives a restart
    drop(db);
    let db = open(&d);
    let row = db.get_row("docs", &[i(1)]).unwrap().unwrap();
    assert_eq!(read(&db, get(&row, "cover")), bytes_of((1, 0)));
    clean(&db);
}

#[test]
fn the_same_content_is_one_stored_file_however_many_rows_hold_it() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    for n in 1..=5 {
        put(&db, n, Some((4, 0)), &[]);
    }
    assert_eq!(stored_files(&db), 1);
    for n in 1..=4 {
        db.delete_row("docs", &[i(n)]).unwrap();
    }
    drain(&db);
    db.compact(0.0).unwrap();
    let row = db.get_row("docs", &[i(5)]).unwrap().unwrap();
    assert_eq!(
        read(&db, get(&row, "cover")),
        bytes_of((4, 0)),
        "the last row still has its file"
    );
    db.delete_row("docs", &[i(5)]).unwrap();
    drain(&db);
    assert_eq!(
        stored_files(&db),
        0,
        "the file went with its last reference"
    );
    clean(&db);
}

#[test]
fn replacing_and_clearing_a_file_releases_the_old_one() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    put(&db, 1, Some((1, 0)), &[]);
    let p = stage(&db, (2, 0));
    let v = Value::File(p.file_value());
    db.update_row_with("docs", &[i(1)], &[("cover", Some(v))], &[&p])
        .unwrap();
    drop(p);
    drain(&db);
    assert_eq!(stored_files(&db), 1, "the replaced file was released");
    let row = db.get_row("docs", &[i(1)]).unwrap().unwrap();
    assert_eq!(read(&db, get(&row, "cover")), bytes_of((2, 0)));
    db.update_row("docs", &[i(1)], &[("cover", None)]).unwrap();
    drain(&db);
    assert_eq!(stored_files(&db), 0, "a file set to null is released");
    clean(&db);
}

#[test]
fn a_list_of_files_counts_each_reference_and_updates_by_difference() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    // the same content twice in one list is two references to one file
    put(&db, 1, None, &[(1, 0), (1, 0), (2, 0)]);
    assert_eq!(stored_files(&db), 2);
    clean(&db);
    // keep one of the repeated files and the other, drop nothing stored twice by mistake
    let row = db.get_row("docs", &[i(1)]).unwrap().unwrap();
    let Some(Value::Array(_, pages)) = get(&row, "pages") else {
        panic!()
    };
    let shorter = Value::Array(Type::File, vec![pages[0].clone(), pages[2].clone()]);
    db.update_row("docs", &[i(1)], &[("pages", Some(shorter))])
        .unwrap();
    drain(&db);
    db.compact(0.0).unwrap();
    let row = db.get_row("docs", &[i(1)]).unwrap().unwrap();
    let Some(Value::Array(_, pages)) = get(&row, "pages") else {
        panic!()
    };
    assert_eq!(
        read(&db, &Some(pages[0].clone())),
        bytes_of((1, 0)),
        "one reference to the repeated file remains"
    );
    assert_eq!(read(&db, &Some(pages[1].clone())), bytes_of((2, 0)));
    clean(&db);
    db.delete_row("docs", &[i(1)]).unwrap();
    drain(&db);
    assert_eq!(stored_files(&db), 0);
    clean(&db);
}

#[test]
fn a_record_an_object_and_a_row_can_hold_one_file_and_each_keeps_it_alive() {
    use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    db.create_collection(CollectionDef {
        name: "recs".into(),
        fields: vec![FieldDef {
            name: "f".into(),
            kind: FieldKind::File,
            required: false,
            max: None,
            mime: vec![],
            relation: None,
        }],
        rules: Rules::default(),
        encrypt: false,
        created_at: 0,
    })
    .unwrap();
    db.create_bucket(BucketDef {
        name: "b".into(),
        versioning: false,
        encrypt: false,
        created_at: 0,
    })
    .unwrap();
    let c = (5, 0);
    put(&db, 1, Some(c), &[]);
    let f = db
        .stage_file(
            "recs",
            "application/octet-stream",
            None,
            Cursor::new(bytes_of(c)),
        )
        .unwrap();
    let rec = db
        .create("recs", Default::default(), vec![("f".into(), f)])
        .unwrap();
    let o = db
        .stage_object(
            "b",
            "application/octet-stream",
            None,
            Cursor::new(bytes_of(c)),
        )
        .unwrap();
    db.put_object("b", "k", o, Default::default()).unwrap();
    assert_eq!(stored_files(&db), 1, "one stored copy for all three");
    clean(&db);
    let check_alive = |db: &Db, who: &str| {
        drain(db);
        db.compact(0.0).unwrap();
        clean(db);
        assert!(
            db.verify(true).unwrap().files == 1,
            "{who}: the file went while something still holds it"
        );
    };
    db.delete_row("docs", &[i(1)]).unwrap();
    check_alive(&db, "after the row went");
    db.delete("recs", &rec.id).unwrap();
    check_alive(&db, "after the record went");
    let m = db.get_object("b", "k", None).unwrap();
    assert_eq!(
        db.read_file(&powerdb::util::from_hex_32(&m.file_id).unwrap())
            .unwrap(),
        bytes_of(c)
    );
    db.delete_object("b", "k", None).unwrap();
    drain(&db);
    assert_eq!(stored_files(&db), 0);
    clean(&db);
}

#[test]
fn an_objects_file_can_be_attached_to_a_row_and_outlives_the_object() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    db.create_bucket(BucketDef {
        name: "b".into(),
        versioning: true,
        encrypt: false,
        created_at: 0,
    })
    .unwrap();
    let o = db
        .stage_object(
            "b",
            "text/plain",
            Some("contract.txt"),
            Cursor::new(bytes_of((6, 0))),
        )
        .unwrap();
    db.put_object("b", "contracts/acme", o, Default::default())
        .unwrap();
    let fv = db
        .file_value_of_object("b", "contracts/acme", None)
        .unwrap();
    assert_eq!(
        (fv.mime.as_str(), fv.name.as_deref()),
        ("text/plain", Some("contract.txt"))
    );
    db.insert_row("docs", &[("id", i(1)), ("cover", Value::File(fv))])
        .unwrap();
    assert_eq!(stored_files(&db), 1, "attached, not copied");
    // remove every version of the object: the row still has the file
    for v in db.object_versions("b", "contracts/acme").unwrap() {
        db.delete_object("b", "contracts/acme", Some(&v.version_id))
            .unwrap();
    }
    drain(&db);
    db.compact(0.0).unwrap();
    let row = db.get_row("docs", &[i(1)]).unwrap().unwrap();
    assert_eq!(read(&db, get(&row, "cover")), bytes_of((6, 0)));
    clean(&db);
    assert!(db.file_value_of_object("b", "nope", None).is_err());
}

// ---- refusals ----

#[test]
fn a_file_value_must_name_a_stored_file_of_that_size() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    put(&db, 1, Some((1, 0)), &[]);
    let row = db.get_row("docs", &[i(1)]).unwrap().unwrap();
    let Some(Value::File(good)) = get(&row, "cover").clone() else {
        panic!()
    };
    let lie = FileValue {
        size: good.size + 1,
        ..good.clone()
    };
    assert!(
        matches!(
            db.insert_row("docs", &[("id", i(2)), ("cover", Value::File(lie))]),
            Err(Error::Invalid(_))
        ),
        "a wrong size"
    );
    let ghost = FileValue {
        id: [7; 32],
        ..good.clone()
    };
    assert!(
        matches!(
            db.insert_row("docs", &[("id", i(2)), ("cover", Value::File(ghost))]),
            Err(Error::NotFound(_))
        ),
        "a file that is not stored"
    );
    assert!(db.get_row("docs", &[i(2)]).unwrap().is_none());
    // an existing file is fine
    db.insert_row("docs", &[("id", i(2)), ("cover", Value::File(good))])
        .unwrap();
    // a file value in a column that is not a file column
    let p = stage(&db, (2, 0));
    let v = Value::File(p.file_value());
    assert!(
        db.insert_row_with("docs", &[("id", i(3)), ("note", v)], &[&p])
            .is_err()
    );
    clean(&db);
}

#[test]
fn file_columns_have_rules() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    let add = |name: &str, ty: Type, nullable: bool, default: Option<Value>| {
        db.alter_table(
            "docs",
            &Change::AddColumn {
                name: name.into(),
                ty,
                nullable,
                default,
            },
        )
    };
    assert!(
        add("d", Type::File, true, Some(Value::Text("x".into()))).is_err(),
        "no default for a file column"
    );
    // a file cannot be a key, unique or indexed
    assert!(
        db.alter_table(
            "docs",
            &Change::AddUnique {
                name: "u".into(),
                columns: vec!["cover".into()]
            }
        )
        .is_err()
    );
    assert!(
        db.alter_table(
            "docs",
            &Change::AddIndex {
                name: "ix".into(),
                columns: vec!["cover".into()]
            }
        )
        .is_err()
    );
    let mut t = TableDef::new("bad").unwrap();
    t = t
        .apply(
            &Change::AddColumn {
                name: "f".into(),
                ty: Type::File,
                nullable: false,
                default: None,
            },
            true,
        )
        .unwrap()
        .0;
    assert!(t.set_primary_key(&["f"]).is_err());
    // NOT NULL needs a file
    add("req", Type::File, false, None).unwrap();
    assert!(matches!(
        db.insert_row("docs", &[("id", i(1))]),
        Err(Error::Invalid(_))
    ));
    // a check can read a file's labels
    db.alter_table(
        "docs",
        &Change::AddCheck {
            name: "small".into(),
            expr: "cover == null || cover.size < 100000".into(),
        },
    )
    .unwrap();
    let p = stage(&db, (1, 0)); // 150,000 bytes
    let big = Value::File(p.file_value());
    let req = Value::File(p.file_value());
    assert!(matches!(
        db.insert_row_with("docs", &[("id", i(2)), ("cover", big), ("req", req)], &[&p]),
        Err(Error::Invalid(_))
    ));
}

// ---- releasing ----

#[test]
fn deleting_a_row_by_cascade_releases_its_files_and_set_null_keeps_them() {
    let d = Dir::new();
    let db = open(&d);
    let make = |name: &str, on_delete: OnDelete| {
        let mut p = TableDef::new(&format!("{name}_parent")).unwrap();
        p = p
            .apply(
                &Change::AddColumn {
                    name: "id".into(),
                    ty: Type::I64,
                    nullable: false,
                    default: None,
                },
                true,
            )
            .unwrap()
            .0;
        p.set_primary_key(&["id"]).unwrap();
        db.create_table(p).unwrap();
        let mut c = TableDef::new(&format!("{name}_child")).unwrap();
        for (n, ty, nullable) in [
            ("id", Type::I64, false),
            ("pid", Type::I64, true),
            ("doc", Type::File, true),
        ] {
            c = c
                .apply(
                    &Change::AddColumn {
                        name: n.into(),
                        ty,
                        nullable,
                        default: None,
                    },
                    true,
                )
                .unwrap()
                .0;
        }
        c.set_primary_key(&["id"]).unwrap();
        db.create_table(c).unwrap();
        db.alter_table(
            &format!("{name}_child"),
            &Change::AddForeignKey {
                name: "fk".into(),
                columns: vec!["pid".into()],
                ref_table: format!("{name}_parent"),
                ref_table_id: 0,
                ref_pk_types: vec![],
                on_delete,
            },
        )
        .unwrap();
        db.insert_row(&format!("{name}_parent"), &[("id", i(1))])
            .unwrap();
        let p = db
            .stage_table_file(
                &format!("{name}_child"),
                "text/plain",
                None,
                Cursor::new(bytes_of((8, 0))),
            )
            .unwrap();
        let v = Value::File(p.file_value());
        db.insert_row_with(
            &format!("{name}_child"),
            &[("id", i(10)), ("pid", i(1)), ("doc", v)],
            &[&p],
        )
        .unwrap();
    };
    make("cas", OnDelete::Cascade);
    assert_eq!(stored_files(&db), 1);
    db.delete_row("cas_parent", &[i(1)]).unwrap();
    drain(&db);
    assert_eq!(
        stored_files(&db),
        0,
        "the cascade deleted the child and released its file"
    );
    clean(&db);
    make("nul", OnDelete::SetNull);
    db.delete_row("nul_parent", &[i(1)]).unwrap();
    drain(&db);
    db.compact(0.0).unwrap();
    assert_eq!(
        stored_files(&db),
        1,
        "set null left the child, and its file"
    );
    let row = db.get_row("nul_child", &[i(10)]).unwrap().unwrap();
    assert_eq!(read(&db, get(&row, "doc")), bytes_of((8, 0)));
    clean(&db);
}

#[test]
fn dropping_a_table_releases_every_file_in_it() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    // more rows than one batch of the drop, all holding the same file: thousands of references to release
    let c = (1, 0);
    let p = stage(&db, c);
    let fv = Value::File(p.file_value());
    db.insert_row_with("docs", &[("id", i(0)), ("cover", fv.clone())], &[&p])
        .unwrap();
    for n in 1..2300 {
        db.insert_row("docs", &[("id", i(n)), ("cover", fv.clone())])
            .unwrap();
    }
    drop(p);
    assert_eq!(stored_files(&db), 1);
    assert_eq!(db.drop_table("docs").unwrap(), 2300);
    drain(&db);
    assert_eq!(stored_files(&db), 0);
    clean(&db);
}

#[test]
fn dropping_a_file_column_releases_its_files_and_leaves_the_rest() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    put(&db, 1, Some((1, 0)), &[(2, 0)]);
    put(&db, 2, Some((1, 0)), &[]);
    put(&db, 3, None, &[(3, 0)]);
    assert_eq!(stored_files(&db), 3);
    db.alter_table(
        "docs",
        &Change::DropColumn {
            name: "cover".into(),
        },
    )
    .unwrap();
    drain(&db);
    db.compact(0.0).unwrap();
    assert_eq!(
        stored_files(&db),
        2,
        "the cover files went; the pages stayed"
    );
    let row = db.get_row("docs", &[i(1)]).unwrap().unwrap();
    assert!(row.iter().all(|(n, _)| n != "cover"));
    let Some(Value::Array(_, pages)) = get(&row, "pages") else {
        panic!()
    };
    assert_eq!(read(&db, &Some(pages[0].clone())), bytes_of((2, 0)));
    // a new column of the same name does not pick the old files up
    db.alter_table(
        "docs",
        &Change::AddColumn {
            name: "cover".into(),
            ty: Type::File,
            nullable: true,
            default: None,
        },
    )
    .unwrap();
    assert_eq!(
        get(&db.get_row("docs", &[i(1)]).unwrap().unwrap(), "cover"),
        &None
    );
    clean(&db);
}

#[test]
fn check_finds_a_row_that_refers_to_a_missing_file() {
    let d = Dir::new();
    let db = open(&d);
    docs(&db);
    put(&db, 1, Some((1, 0)), &[]);
    clean(&db);
    // damage: a row holding a file that is not stored
    let t = db.table("docs").unwrap();
    let ghost = FileValue {
        id: [9; 32],
        size: 1,
        mime: "x".into(),
        name: None,
    };
    let raw = powerdb::types::row::encode_row(&[
        (t.column("id").unwrap().id, i(2)),
        (t.column("cover").unwrap().id, Value::File(ghost)),
    ])
    .unwrap();
    let mut key = t.id.to_be_bytes().to_vec();
    key.extend_from_slice(&t.key_for_values(&[i(2)]).unwrap());
    db.keyspace("rows").unwrap().insert(key, raw).unwrap();
    let all = db.verify(false).unwrap().problems.join("\n");
    assert!(all.contains("missing"), "{all}");
}

// ---- the model ----

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[derive(Clone, Default)]
struct ModelRow {
    cover: Option<Content>,
    pages: Vec<Content>,
}

fn run(seed: u64, steps: usize) {
    let d = Dir::new();
    let mut db = Some(open(&d));
    let mut r = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
    docs(db.as_ref().unwrap());
    db.as_ref()
        .unwrap()
        .create_bucket(BucketDef {
            name: "b".into(),
            versioning: false,
            encrypt: false,
            created_at: 0,
        })
        .unwrap();
    let mut rows: BTreeMap<i64, ModelRow> = BTreeMap::new();
    let mut objects: BTreeMap<String, Content> = BTreeMap::new();
    let pick = |r: &mut Rng| -> Content { (r.below(5) as u8, r.below(3) as u8) };
    for step in 0..steps {
        let dbr = db.as_ref().unwrap();
        let ctx = format!("seed {seed} step {step}");
        match r.below(100) {
            // insert a row
            0..=29 => {
                let id = r.below(15) as i64;
                let cover = if r.below(3) == 0 {
                    None
                } else {
                    Some(pick(&mut r))
                };
                let pages: Vec<Content> = (0..r.below(3)).map(|_| pick(&mut r)).collect();
                match rows.entry(id) {
                    std::collections::btree_map::Entry::Occupied(_) => {
                        // a duplicate key: refused, and the files staged for it are only garbage
                        let p = stage(dbr, pick(&mut r));
                        let v = Value::File(p.file_value());
                        assert!(
                            dbr.insert_row_with("docs", &[("id", i(id)), ("cover", v)], &[&p])
                                .is_err(),
                            "{ctx}"
                        );
                    }
                    std::collections::btree_map::Entry::Vacant(e) => {
                        put(dbr, id, cover, &pages);
                        e.insert(ModelRow { cover, pages });
                    }
                }
            }
            // replace the cover, or clear it
            30..=44 => {
                let id = r.below(15) as i64;
                if rows.contains_key(&id) {
                    if r.below(3) == 0 {
                        dbr.update_row("docs", &[i(id)], &[("cover", None)])
                            .unwrap();
                        rows.get_mut(&id).unwrap().cover = None;
                    } else {
                        let c = pick(&mut r);
                        let p = stage(dbr, c);
                        let v = Value::File(p.file_value());
                        dbr.update_row_with("docs", &[i(id)], &[("cover", Some(v))], &[&p])
                            .unwrap();
                        rows.get_mut(&id).unwrap().cover = Some(c);
                    }
                }
            }
            // replace the pages
            45..=54 => {
                let id = r.below(15) as i64;
                if rows.contains_key(&id) {
                    let pages: Vec<Content> = (0..r.below(4)).map(|_| pick(&mut r)).collect();
                    if pages.is_empty() {
                        dbr.update_row("docs", &[i(id)], &[("pages", None)])
                            .unwrap();
                    } else {
                        let staged: Vec<_> = pages.iter().map(|c| stage(dbr, *c)).collect();
                        let v = Value::Array(
                            Type::File,
                            staged.iter().map(|p| Value::File(p.file_value())).collect(),
                        );
                        let refs: Vec<&powerdb::storage::PendingFile> = staged.iter().collect();
                        dbr.update_row_with("docs", &[i(id)], &[("pages", Some(v))], &refs)
                            .unwrap();
                    }
                    rows.get_mut(&id).unwrap().pages = pages;
                }
            }
            55..=66 => {
                let id = r.below(15) as i64;
                assert_eq!(
                    dbr.delete_row("docs", &[i(id)]).unwrap(),
                    rows.remove(&id).is_some(),
                    "{ctx}"
                );
            }
            // objects sharing the same pool of contents
            67..=76 => {
                let key = format!("k{}", r.below(5));
                let c = pick(&mut r);
                let o = dbr
                    .stage_object(
                        "b",
                        "application/octet-stream",
                        None,
                        Cursor::new(bytes_of(c)),
                    )
                    .unwrap();
                dbr.put_object("b", &key, o, Default::default()).unwrap();
                objects.insert(key, c);
            }
            77..=82 => {
                let key = format!("k{}", r.below(5));
                assert_eq!(
                    dbr.delete_object("b", &key, None).unwrap(),
                    objects.remove(&key).is_some(),
                    "{ctx}"
                );
            }
            83..=88 => {
                dbr.gc_step(1 + r.below(4)).unwrap();
            }
            89..=93 => {
                drain(dbr);
                dbr.compact(0.0).unwrap();
            }
            94..=96 => {
                drop(db.take());
                db = Some(open(&d));
            }
            _ => {}
        }
        let dbr = db.as_ref().unwrap();
        // every row reads as the model says, file bytes included
        for (id, m) in &rows {
            let row = dbr
                .get_row("docs", &[i(*id)])
                .unwrap()
                .unwrap_or_else(|| panic!("{ctx}: row {id} is gone"));
            match (&m.cover, get(&row, "cover")) {
                (None, None) => {}
                (Some(c), v @ Some(_)) => {
                    assert!(read(dbr, v) == bytes_of(*c), "{ctx}: the cover of {id}")
                }
                (a, b) => panic!("{ctx}: row {id}: cover {a:?} vs {b:?}"),
            }
            let pages: Vec<Vec<u8>> = match get(&row, "pages") {
                Some(Value::Array(_, items)) => {
                    items.iter().map(|v| read(dbr, &Some(v.clone()))).collect()
                }
                _ => vec![],
            };
            assert!(
                pages == m.pages.iter().map(|c| bytes_of(*c)).collect::<Vec<_>>(),
                "{ctx}: the pages of {id}"
            );
        }
        for (k, c) in &objects {
            let m = dbr.get_object("b", k, None).unwrap();
            assert!(
                dbr.read_file(&powerdb::util::from_hex_32(&m.file_id).unwrap())
                    .unwrap()
                    == bytes_of(*c),
                "{ctx}: object {k}"
            );
        }
        if step % 10 == 9 {
            let rep = dbr.verify(true).unwrap();
            assert!(
                rep.problems.is_empty(),
                "{ctx}: verify found {:?}",
                rep.problems
            );
        }
    }
    // once the release queue is empty, the stored files are exactly the distinct files something still holds
    let dbr = db.as_ref().unwrap();
    drain(dbr);
    dbr.compact(0.0).unwrap();
    let rep = dbr.verify(true).unwrap();
    assert!(
        rep.problems.is_empty(),
        "seed {seed} end: {:?}",
        rep.problems
    );
    let mut held: HashSet<Content> = HashSet::new();
    for m in rows.values() {
        held.extend(m.cover);
        held.extend(m.pages.iter().copied());
    }
    held.extend(objects.values().copied());
    assert_eq!(
        rep.files,
        held.len() as u64,
        "seed {seed}: stored files differ from the files in use (a leak, or a premature release)"
    );
}

#[test]
fn random_file_operations_across_rows_and_objects_keep_the_counts_exact() {
    let n: u64 = std::env::var("POWERDB_MODEL_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    for seed in 1..=n {
        run(seed, 130);
    }
}
