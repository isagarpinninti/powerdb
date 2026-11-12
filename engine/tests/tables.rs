//! Typed tables in storage: behaviour, schema changes on stored rows, integrity checking, and a random-operations model test.

use powerdb::storage::tables::Row;
use powerdb::types::schema::{Change, TableDef, widen_value};
use powerdb::types::{Decimal, Type, Value};
use powerdb::util::new_id;
use powerdb::{Db, Durability, Error, Options};
use std::collections::BTreeMap;
use std::path::PathBuf;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("powerdb-tbl-{}", new_id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(d: &Dir) -> Db {
    Db::open(
        &d.0,
        Options {
            durability: Durability::Never,
            ..Options::default()
        },
    )
    .unwrap()
}

fn add(name: &str, ty: Type, nullable: bool, default: Option<Value>) -> Change {
    Change::AddColumn {
        name: name.into(),
        ty,
        nullable,
        default,
    }
}

fn def(name: &str, cols: &[(&str, Type, bool, Option<Value>)], pk: &[&str]) -> TableDef {
    let mut t = TableDef::new(name).unwrap();
    for (n, ty, nullable, d) in cols {
        t = t
            .apply(&add(n, ty.clone(), *nullable, d.clone()), true)
            .unwrap()
            .0;
    }
    t.set_primary_key(pk).unwrap();
    t
}

fn people() -> TableDef {
    def(
        "people",
        &[
            ("id", Type::I64, false, None),
            ("name", Type::Text, false, None),
            ("age", Type::I32, true, None),
            ("active", Type::Bool, false, Some(Value::Bool(true))),
        ],
        &["id"],
    )
}

fn get<'a>(row: &'a Row, name: &str) -> &'a Option<Value> {
    &row.iter().find(|(n, _)| n == name).unwrap().1
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn clean(db: &Db) {
    let r = db.verify(true).unwrap();
    assert!(r.problems.is_empty(), "{:?}", r.problems);
}

// ---- tables ----

#[test]
fn creating_tables_checks_the_definition() {
    let d = Dir::new();
    let db = open(&d);
    let t = db.create_table(people()).unwrap();
    assert!(t.id > 0 && t.version == 1);
    assert_eq!(db.table("people").unwrap(), t);
    assert!(matches!(db.create_table(people()), Err(Error::Conflict(_))));
    assert!(matches!(db.table("nope"), Err(Error::NotFound(_))));
    // no primary key
    let mut no_pk = TableDef::new("nopk").unwrap();
    no_pk = no_pk
        .apply(&add("a", Type::I32, true, None), true)
        .unwrap()
        .0;
    assert!(matches!(db.create_table(no_pk), Err(Error::Invalid(_))));
    // a primary key column must be NOT NULL and indexable
    let mut bad = TableDef::new("bad").unwrap();
    bad = bad.apply(&add("a", Type::I32, true, None), true).unwrap().0;
    assert!(bad.set_primary_key(&["a"]).is_err(), "nullable key");
    let mut bad = TableDef::new("bad").unwrap();
    bad = bad
        .apply(&add("j", Type::Json, false, None), true)
        .unwrap()
        .0;
    assert!(bad.set_primary_key(&["j"]).is_err(), "json key");
    assert!(bad.set_primary_key(&["zz"]).is_err(), "unknown column");
    // types not available in tables yet
    for ty in [Type::Enum, Type::Number] {
        let mut t = TableDef::new("x").unwrap();
        t = t.apply(&add("id", Type::I32, false, None), true).unwrap().0;
        t = t.apply(&add("c", ty.clone(), true, None), true).unwrap().0;
        t.set_primary_key(&["id"]).unwrap();
        assert!(
            matches!(db.create_table(t), Err(Error::Invalid(_))),
            "{ty:?}"
        );
    }
    assert_eq!(db.tables().unwrap().len(), 1);
}

#[test]
fn the_catalog_survives_a_restart_including_defaults_and_types() {
    let d = Dir::new();
    let want = {
        let db = open(&d);
        db.create_table(def(
            "everything",
            &[
                ("id", Type::Uuid, false, None),
                (
                    "amount",
                    Type::Decimal,
                    false,
                    Some(Value::Decimal(Decimal::new(1250, 2).unwrap())),
                ),
                ("tags", Type::Array(Box::new(Type::Text)), true, None),
                ("meta", Type::Json, true, None),
                ("at", Type::TimestampTz, true, None),
                ("big", Type::U64, false, Some(Value::U64(u64::MAX))),
            ],
            &["id"],
        ))
        .unwrap()
    };
    let db = open(&d);
    assert_eq!(db.table("everything").unwrap(), want);
    clean(&db);
}

#[test]
fn table_ids_are_never_reused_and_a_dropped_tables_rows_never_come_back() {
    let d = Dir::new();
    let db = open(&d);
    let a = db.create_table(people()).unwrap();
    db.insert_row("people", &[("id", Value::I64(1)), ("name", text("Ann"))])
        .unwrap();
    assert_eq!(db.drop_table("people").unwrap(), 1);
    assert!(matches!(db.table("people"), Err(Error::NotFound(_))));
    let b = db.create_table(people()).unwrap();
    assert!(b.id > a.id, "a table id was reused");
    assert_eq!(
        db.count_rows("people").unwrap(),
        0,
        "the old table's row came back"
    );
    clean(&db);
}

#[test]
fn dropping_a_large_table_removes_every_row() {
    let d = Dir::new();
    let db = open(&d);
    db.create_table(people()).unwrap();
    for i in 0..4500i64 {
        db.insert_row("people", &[("id", Value::I64(i)), ("name", text("n"))])
            .unwrap();
    }
    db.create_table(def(
        "other",
        &[("id", Value::I64(0).type_of(), false, None)],
        &["id"],
    ))
    .unwrap();
    db.insert_row("other", &[("id", Value::I64(7))]).unwrap();
    assert_eq!(db.drop_table("people").unwrap(), 4500);
    assert_eq!(
        db.count_rows("other").unwrap(),
        1,
        "another table's rows must be untouched"
    );
    clean(&db);
}

// ---- rows ----

#[test]
fn rows_are_inserted_read_updated_and_deleted() {
    let d = Dir::new();
    let db = open(&d);
    db.create_table(people()).unwrap();
    let pk = db
        .insert_row("people", &[("id", Value::I64(1)), ("name", text("Ann"))])
        .unwrap();
    assert_eq!(pk, vec![Value::I64(1)]);
    let row = db.get_row("people", &pk).unwrap().unwrap();
    assert_eq!(get(&row, "name"), &Some(text("Ann")));
    assert_eq!(
        get(&row, "age"),
        &None,
        "a nullable column left out is null"
    );
    assert_eq!(
        get(&row, "active"),
        &Some(Value::Bool(true)),
        "a column left out takes its default"
    );

    let after = db
        .update_row(
            "people",
            &pk,
            &[
                ("age", Some(Value::I32(30))),
                ("active", Some(Value::Bool(false))),
            ],
        )
        .unwrap();
    assert_eq!(get(&after, "age"), &Some(Value::I32(30)));
    assert_eq!(
        get(&db.get_row("people", &pk).unwrap().unwrap(), "active"),
        &Some(Value::Bool(false))
    );
    let after = db.update_row("people", &pk, &[("age", None)]).unwrap();
    assert_eq!(get(&after, "age"), &None);
    assert_eq!(
        get(&after, "name"),
        &Some(text("Ann")),
        "other columns are untouched"
    );

    assert!(db.delete_row("people", &pk).unwrap());
    assert!(!db.delete_row("people", &pk).unwrap());
    assert!(db.get_row("people", &pk).unwrap().is_none());
    assert!(matches!(
        db.update_row("people", &pk, &[]),
        Err(Error::NotFound(_))
    ));
    clean(&db);
}

#[test]
fn inserts_and_updates_enforce_the_schema() {
    let d = Dir::new();
    let db = open(&d);
    db.create_table(people()).unwrap();
    db.insert_row("people", &[("id", Value::I64(1)), ("name", text("Ann"))])
        .unwrap();
    let dup = db.insert_row("people", &[("id", Value::I64(1)), ("name", text("Bob"))]);
    assert!(matches!(dup, Err(Error::Conflict(_))), "{dup:?}");
    assert!(
        matches!(
            db.insert_row("people", &[("id", Value::I64(2))]),
            Err(Error::Invalid(_))
        ),
        "NOT NULL name has no value"
    );
    assert!(
        matches!(
            db.insert_row("people", &[("id", Value::I64(2)), ("name", Value::I32(5))]),
            Err(Error::Invalid(_))
        ),
        "wrong type"
    );
    assert!(
        matches!(
            db.insert_row("people", &[("id", Value::I32(2)), ("name", text("x"))]),
            Err(Error::Invalid(_))
        ),
        "a key of the wrong type"
    );
    assert!(
        matches!(
            db.insert_row(
                "people",
                &[
                    ("id", Value::I64(2)),
                    ("name", text("x")),
                    ("zip", Value::I32(1))
                ]
            ),
            Err(Error::Invalid(_))
        ),
        "unknown column"
    );
    let pk = [Value::I64(1)];
    assert!(
        matches!(
            db.update_row("people", &pk, &[("name", None)]),
            Err(Error::Invalid(_))
        ),
        "NOT NULL cannot be nulled"
    );
    assert!(
        matches!(
            db.update_row("people", &pk, &[("active", None)]),
            Err(Error::Invalid(_))
        ),
        "a column with a default cannot be nulled"
    );
    assert!(
        matches!(
            db.update_row("people", &pk, &[("id", Some(Value::I64(9)))]),
            Err(Error::Invalid(_))
        ),
        "the key cannot change"
    );
    assert!(
        db.update_row("people", &pk, &[("id", Some(Value::I64(1)))])
            .is_ok(),
        "naming the key with its own value is fine"
    );
    assert!(matches!(
        db.update_row("people", &pk, &[("age", Some(text("x")))]),
        Err(Error::Invalid(_))
    ));
    assert!(
        matches!(
            db.get_row("people", &[Value::I64(1), Value::I64(2)]),
            Err(Error::Invalid(_))
        ),
        "the wrong number of key values"
    );
    // a refused write left the row as it was
    let row = db.get_row("people", &pk).unwrap().unwrap();
    assert_eq!(get(&row, "name"), &Some(text("Ann")));
    clean(&db);
}

#[test]
fn composite_keys_and_scan_order_follow_the_values_not_the_bytes() {
    let d = Dir::new();
    let db = open(&d);
    db.create_table(def(
        "events",
        &[
            ("tenant", Type::Text, false, None),
            ("seq", Type::I64, false, None),
            ("note", Type::Text, true, None),
        ],
        &["tenant", "seq"],
    ))
    .unwrap();
    let mut keys: Vec<(String, i64)> = Vec::new();
    for t in ["b", "a", "ab", "a\0z", ""] {
        for s in [5i64, -3, 0, i64::MIN, i64::MAX, -1] {
            db.insert_row("events", &[("tenant", text(t)), ("seq", Value::I64(s))])
                .unwrap();
            keys.push((t.to_string(), s));
        }
    }
    keys.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()).then(a.1.cmp(&b.1)));
    // page through with every page size
    for limit in [1usize, 4, 7, 1000] {
        let mut seen: Vec<(String, i64)> = Vec::new();
        let mut after: Option<Vec<Value>> = None;
        let mut pages = 0;
        loop {
            pages += 1;
            assert!(
                pages <= keys.len() + 2,
                "limit {limit}: paging never ended (the cursor repeats a row)"
            );
            let page = db.scan_rows("events", after.as_deref(), limit).unwrap();
            if page.is_empty() {
                break;
            }
            for r in &page {
                let Some(Value::Text(t)) = get(r, "tenant") else {
                    panic!()
                };
                let Some(Value::I64(s)) = get(r, "seq") else {
                    panic!()
                };
                seen.push((t.clone(), *s));
            }
            let last = page.last().unwrap();
            after = Some(vec![
                get(last, "tenant").clone().unwrap(),
                get(last, "seq").clone().unwrap(),
            ]);
        }
        assert_eq!(
            seen, keys,
            "limit {limit}: not every row once, in key order"
        );
    }
    // a row is found by its whole key only
    assert!(
        db.get_row("events", &[text("a"), Value::I64(5)])
            .unwrap()
            .is_some()
    );
    assert!(
        db.get_row("events", &[text("a"), Value::I64(6)])
            .unwrap()
            .is_none()
    );
    clean(&db);
}

// ---- schema changes on stored rows ----

#[test]
fn schema_changes_apply_to_rows_already_stored_without_rewriting_them() {
    let d = Dir::new();
    let db = open(&d);
    db.create_table(people()).unwrap();
    for i in 1..=3i64 {
        db.insert_row(
            "people",
            &[
                ("id", Value::I64(i)),
                ("name", text(&format!("p{i}"))),
                ("age", Value::I32(20 + i as i32)),
            ],
        )
        .unwrap();
    }
    let pk = |i: i64| [Value::I64(i)];
    // add, with a default: old rows read it
    db.alter_table(
        "people",
        &add("country", Type::Text, false, Some(text("NL"))),
    )
    .unwrap();
    assert_eq!(
        get(&db.get_row("people", &pk(1)).unwrap().unwrap(), "country"),
        &Some(text("NL"))
    );
    // rename: the data follows
    db.alter_table(
        "people",
        &Change::RenameColumn {
            from: "name".into(),
            to: "full_name".into(),
        },
    )
    .unwrap();
    assert_eq!(
        get(&db.get_row("people", &pk(2)).unwrap().unwrap(), "full_name"),
        &Some(text("p2"))
    );
    // widen: old values read as the wider type
    db.alter_table(
        "people",
        &Change::WidenColumn {
            name: "age".into(),
            to: Type::I64,
        },
    )
    .unwrap();
    assert_eq!(
        get(&db.get_row("people", &pk(3)).unwrap().unwrap(), "age"),
        &Some(Value::I64(23))
    );
    // drop: gone from reads, and a new column of the same name does not inherit the old values
    db.alter_table("people", &Change::DropColumn { name: "age".into() })
        .unwrap();
    assert!(
        db.get_row("people", &pk(1))
            .unwrap()
            .unwrap()
            .iter()
            .all(|(n, _)| n != "age")
    );
    db.alter_table("people", &add("age", Type::I32, true, None))
        .unwrap();
    assert_eq!(
        get(&db.get_row("people", &pk(1)).unwrap().unwrap(), "age"),
        &None
    );
    // an update rewrites the row under the new schema and drops what no longer exists
    db.update_row("people", &pk(1), &[("age", Some(Value::I32(40)))])
        .unwrap();
    clean(&db);
    // all of it survives a restart
    drop(db);
    let db = open(&d);
    assert_eq!(
        get(&db.get_row("people", &pk(1)).unwrap().unwrap(), "age"),
        &Some(Value::I32(40))
    );
    assert_eq!(db.table("people").unwrap().version, 6);
    clean(&db);
}

#[test]
fn a_new_not_null_is_checked_against_the_rows_first() {
    let d = Dir::new();
    let db = open(&d);
    db.create_table(people()).unwrap();
    db.insert_row(
        "people",
        &[
            ("id", Value::I64(1)),
            ("name", text("a")),
            ("age", Value::I32(1)),
        ],
    )
    .unwrap();
    db.insert_row("people", &[("id", Value::I64(2)), ("name", text("b"))])
        .unwrap(); // no age
    let before = db.table("people").unwrap();
    let err = db
        .alter_table("people", &Change::SetNotNull { name: "age".into() })
        .unwrap_err();
    assert!(
        matches!(err, Error::Invalid(_)) && err.to_string().contains("age"),
        "{err}"
    );
    assert_eq!(
        db.table("people").unwrap(),
        before,
        "a refused change left the catalog alone"
    );
    db.update_row("people", &[Value::I64(2)], &[("age", Some(Value::I32(2)))])
        .unwrap();
    db.alter_table("people", &Change::SetNotNull { name: "age".into() })
        .unwrap();
    assert!(matches!(
        db.insert_row("people", &[("id", Value::I64(3)), ("name", text("c"))]),
        Err(Error::Invalid(_))
    ));
    // a NOT NULL column with no default cannot be added to a table with rows, and can to an empty one
    assert!(
        db.alter_table("people", &add("x", Type::I32, false, None))
            .is_err()
    );
    db.delete_row("people", &[Value::I64(1)]).unwrap();
    db.delete_row("people", &[Value::I64(2)]).unwrap();
    assert!(
        db.alter_table("people", &add("x", Type::I32, false, None))
            .is_ok()
    );
    clean(&db);
}

#[test]
fn the_primary_key_cannot_be_dropped_or_retyped_and_a_type_change_needs_an_empty_table() {
    let d = Dir::new();
    let db = open(&d);
    db.create_table(people()).unwrap();
    db.insert_row("people", &[("id", Value::I64(1)), ("name", text("a"))])
        .unwrap();
    for c in [
        Change::DropColumn { name: "id".into() },
        Change::SetNullable { name: "id".into() },
        Change::WidenColumn {
            name: "id".into(),
            to: Type::I64,
        },
        Change::ChangeType {
            name: "id".into(),
            to: Type::Text,
        },
    ] {
        assert!(
            matches!(db.alter_table("people", &c), Err(Error::Invalid(_))),
            "{c:?}"
        );
    }
    assert!(
        db.alter_table(
            "people",
            &Change::ChangeType {
                name: "name".into(),
                to: Type::I32
            }
        )
        .is_err(),
        "text to int32 on a table with rows"
    );
    db.delete_row("people", &[Value::I64(1)]).unwrap();
    assert!(
        db.alter_table(
            "people",
            &Change::ChangeType {
                name: "name".into(),
                to: Type::I32
            }
        )
        .is_ok()
    );
    // the key column can still be renamed: its id, and so every stored key, is unchanged
    db.alter_table(
        "people",
        &Change::RenameColumn {
            from: "id".into(),
            to: "person_id".into(),
        },
    )
    .unwrap();
    db.insert_row(
        "people",
        &[("person_id", Value::I64(5)), ("name", Value::I32(1))],
    )
    .unwrap();
    clean(&db);
}

// ---- integrity checking ----

#[test]
fn check_finds_damaged_rows_and_catalogs() {
    let d = Dir::new();
    let db = open(&d);
    let t = db.create_table(people()).unwrap();
    db.insert_row("people", &[("id", Value::I64(1)), ("name", text("a"))])
        .unwrap();
    clean(&db);
    let rows = db.keyspace("rows").unwrap();
    let mut key = t.id.to_be_bytes().to_vec();
    // a row of a table that does not exist
    rows.insert([&99u32.to_be_bytes()[..], &[1, 2]].concat(), vec![1, 0])
        .unwrap();
    // a row that does not decode
    rows.insert([&key[..], &[0xAB]].concat(), vec![0xFF, 0xFF])
        .unwrap();
    // a row stored under a key that is not its own primary key
    let wrong = t
        .build_row(&[("id", Value::I64(7)), ("name", text("x"))])
        .unwrap();
    key.extend_from_slice(&t.key_for_values(&[Value::I64(8)]).unwrap());
    rows.insert(key, powerdb::types::row::encode_row(&wrong).unwrap())
        .unwrap();
    // a row that does not fit the schema (a NOT NULL column has no value)
    let mut k2 = t.id.to_be_bytes().to_vec();
    k2.extend_from_slice(&t.key_for_values(&[Value::I64(9)]).unwrap());
    rows.insert(
        k2,
        powerdb::types::row::encode_row(&[(t.column("id").unwrap().id, Value::I64(9))]).unwrap(),
    )
    .unwrap();
    let r = db.verify(false).unwrap();
    let all = r.problems.join("\n");
    for needle in [
        "does not exist",
        "does not decode",
        "not its primary key",
        "does not fit the table",
    ] {
        assert!(
            all.contains(needle),
            "check did not report: {needle}\n{all}"
        );
    }
    // a damaged catalog entry
    db.keyspace("tables")
        .unwrap()
        .insert(b"broken".to_vec(), b"{not json".to_vec())
        .unwrap();
    assert!(
        db.verify(false)
            .unwrap()
            .problems
            .iter()
            .any(|p| p.contains("broken"))
    );
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

fn random_type(r: &mut Rng) -> Type {
    [
        Type::I8,
        Type::I16,
        Type::I32,
        Type::I64,
        Type::F32,
        Type::F64,
        Type::Text,
        Type::Bool,
    ][r.below(8)]
    .clone()
}

fn random_value(r: &mut Rng, t: &Type) -> Value {
    let n = r.next();
    match t {
        Type::I8 => Value::I8(n as i8),
        Type::I16 => Value::I16(n as i16),
        Type::I32 => Value::I32(n as i32),
        Type::I64 => Value::I64(n as i64),
        Type::F32 => Value::F32((n % 1000) as f32 / 4.0),
        Type::F64 => Value::F64((n % 1000) as f64 / 4.0),
        Type::Text => Value::Text(format!("t{}", n % 50)),
        Type::Bool => Value::Bool(n.is_multiple_of(2)),
        _ => unreachable!(),
    }
}

type ModelRow = BTreeMap<String, Option<Value>>;

fn same(a: &Option<Value>, b: &Option<Value>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => x.same(y),
        (None, None) => true,
        _ => false,
    }
}

fn check_all(db: &Db, model: &BTreeMap<i64, ModelRow>, ctx: &str) {
    let mut got: Vec<Row> = Vec::new();
    let mut after: Option<Vec<Value>> = None;
    let mut pages = 0;
    loop {
        pages += 1;
        assert!(
            pages <= model.len() + 2,
            "{ctx}: paging never ended (the cursor repeats a row)"
        );
        let page = db.scan_rows("t", after.as_deref(), 7).unwrap();
        if page.is_empty() {
            break;
        }
        after = Some(vec![get(page.last().unwrap(), "k").clone().unwrap()]);
        got.extend(page);
    }
    assert_eq!(got.len(), model.len(), "{ctx}: the number of rows");
    for (row, (k, m)) in got.iter().zip(model) {
        assert_eq!(
            get(row, "k"),
            &Some(Value::I64(*k)),
            "{ctx}: rows are not in key order"
        );
        assert_eq!(
            row.len(),
            m.len(),
            "{ctx}: key {k}: columns {:?} vs {:?}",
            row.iter().map(|c| &c.0).collect::<Vec<_>>(),
            m.keys().collect::<Vec<_>>()
        );
        for (name, v) in row {
            assert!(
                same(v, &m[name]),
                "{ctx}: key {k} column {name}: read {v:?}, the model says {:?}",
                m[name]
            );
        }
    }
    assert_eq!(
        db.count_rows("t").unwrap(),
        model.len() as u64,
        "{ctx}: count"
    );
}

fn run(seed: u64, steps: usize) {
    let d = Dir::new();
    let mut db = Some(open(&d));
    let mut r = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
    db.as_ref()
        .unwrap()
        .create_table(def("t", &[("k", Type::I64, false, None)], &["k"]))
        .unwrap();
    let mut model: BTreeMap<i64, ModelRow> = BTreeMap::new();
    for step in 0..steps {
        let dbr = db.as_ref().unwrap();
        let tdef = dbr.table("t").unwrap();
        let cols: Vec<_> = tdef
            .columns
            .iter()
            .filter(|c| c.name != "k")
            .cloned()
            .collect();
        let ctx = format!("seed {seed} step {step}");
        match r.below(100) {
            0..=34 => {
                let k = r.below(60) as i64 - 30;
                let mut values: Vec<(String, Value)> = vec![("k".into(), Value::I64(k))];
                for c in &cols {
                    let required = !c.nullable && c.default.is_none();
                    if required || r.below(10) < 6 {
                        values.push((c.name.clone(), random_value(&mut r, &c.ty)));
                    }
                }
                let refs: Vec<(&str, Value)> = values
                    .iter()
                    .map(|(n, v)| (n.as_str(), v.clone()))
                    .collect();
                match dbr.insert_row("t", &refs) {
                    Ok(_) => {
                        assert!(
                            !model.contains_key(&k),
                            "{ctx}: a duplicate key was accepted"
                        );
                        let mut row = ModelRow::new();
                        row.insert("k".into(), Some(Value::I64(k)));
                        for c in &cols {
                            let v = values
                                .iter()
                                .find(|(n, _)| *n == c.name)
                                .map(|(_, v)| v.clone())
                                .or_else(|| c.default.clone());
                            row.insert(c.name.clone(), v);
                        }
                        model.insert(k, row);
                    }
                    Err(Error::Conflict(_)) => assert!(
                        model.contains_key(&k),
                        "{ctx}: a refused insert of a new key"
                    ),
                    Err(e) => panic!("{ctx}: insert: {e}"),
                }
            }
            35..=54 if !cols.is_empty() => {
                let k = r.below(60) as i64 - 30;
                let mut patch: Vec<(String, Option<Value>)> = Vec::new();
                for _ in 0..1 + r.below(2) {
                    let c = &cols[r.below(cols.len())];
                    let v = if r.below(3) == 0 {
                        None
                    } else {
                        Some(random_value(&mut r, &c.ty))
                    };
                    patch.retain(|(n, _)| *n != c.name);
                    patch.push((c.name.clone(), v));
                }
                let refs: Vec<(&str, Option<Value>)> =
                    patch.iter().map(|(n, v)| (n.as_str(), v.clone())).collect();
                // a null is refused for a column that is NOT NULL or has a default
                let bad_null = patch.iter().any(|(n, v)| {
                    v.is_none()
                        && cols
                            .iter()
                            .any(|c| c.name == *n && (!c.nullable || c.default.is_some()))
                });
                match dbr.update_row("t", &[Value::I64(k)], &refs) {
                    Ok(_) => {
                        assert!(
                            !bad_null && model.contains_key(&k),
                            "{ctx}: an update that must be refused was accepted"
                        );
                        let row = model.get_mut(&k).unwrap();
                        for (n, v) in patch {
                            row.insert(n, v);
                        }
                    }
                    Err(Error::NotFound(_)) => assert!(!model.contains_key(&k), "{ctx}"),
                    Err(Error::Invalid(_)) => assert!(
                        bad_null && model.contains_key(&k),
                        "{ctx}: a valid update was refused"
                    ),
                    Err(e) => panic!("{ctx}: update: {e}"),
                }
            }
            55..=64 => {
                let k = r.below(60) as i64 - 30;
                assert_eq!(
                    dbr.delete_row("t", &[Value::I64(k)]).unwrap(),
                    model.remove(&k).is_some(),
                    "{ctx}: delete"
                );
            }
            65..=72 => {
                let name = format!("c{}", r.below(6));
                let ty = random_type(&mut r);
                let nullable = r.below(2) == 0;
                let default = if r.below(2) == 0 {
                    Some(random_value(&mut r, &ty))
                } else {
                    None
                };
                let should_fail = tdef.column(&name).is_some()
                    || (!nullable && default.is_none() && !model.is_empty());
                match dbr.alter_table("t", &add(&name, ty, nullable, default.clone())) {
                    Ok(_) => {
                        assert!(
                            !should_fail,
                            "{ctx}: an add that must be refused was accepted"
                        );
                        for row in model.values_mut() {
                            row.insert(name.clone(), default.clone());
                        }
                    }
                    Err(_) => assert!(should_fail, "{ctx}: a valid add was refused"),
                }
            }
            73..=77 if !cols.is_empty() => {
                let name = cols[r.below(cols.len())].name.clone();
                dbr.alter_table("t", &Change::DropColumn { name: name.clone() })
                    .unwrap();
                for row in model.values_mut() {
                    row.remove(&name);
                }
            }
            78..=82 if !cols.is_empty() => {
                let from = cols[r.below(cols.len())].name.clone();
                let to = format!("c{}", r.below(6));
                let clash = to != from && tdef.column(&to).is_some();
                match dbr.alter_table(
                    "t",
                    &Change::RenameColumn {
                        from: from.clone(),
                        to: to.clone(),
                    },
                ) {
                    Ok(_) => {
                        assert!(!clash);
                        if from != to {
                            for row in model.values_mut() {
                                let v = row.remove(&from).unwrap();
                                row.insert(to.clone(), v);
                            }
                        }
                    }
                    Err(_) => assert!(clash, "{ctx}"),
                }
            }
            83..=87 if !cols.is_empty() => {
                let c = cols[r.below(cols.len())].clone();
                let to = random_type(&mut r);
                if dbr
                    .alter_table(
                        "t",
                        &Change::WidenColumn {
                            name: c.name.clone(),
                            to: to.clone(),
                        },
                    )
                    .is_ok()
                {
                    for row in model.values_mut() {
                        if let Some(Some(v)) = row.get(&c.name).cloned() {
                            row.insert(c.name.clone(), Some(widen_value(v, &to).unwrap()));
                        }
                    }
                }
            }
            88..=91 if !cols.is_empty() => {
                let name = cols[r.below(cols.len())].name.clone();
                if r.below(2) == 0 {
                    dbr.alter_table("t", &Change::SetNullable { name }).unwrap();
                } else {
                    // the engine checks the rows; the model says what the answer has to be
                    let all_have = model.values().all(|row| row[&name].is_some());
                    let res = dbr.alter_table("t", &Change::SetNotNull { name: name.clone() });
                    assert_eq!(res.is_ok(), all_have, "{ctx}: NOT NULL on {name}: {res:?}");
                }
            }
            92..=95 => {
                drop(db.take());
                db = Some(open(&d));
            }
            _ => {}
        }
        let dbr = db.as_ref().unwrap();
        check_all(dbr, &model, &ctx);
        if step % 8 == 7 {
            let rep = dbr.verify(true).unwrap();
            assert!(
                rep.problems.is_empty(),
                "{ctx}: verify found {:?}",
                rep.problems
            );
        }
    }
}

#[test]
fn random_row_and_schema_operations_agree_with_the_model() {
    let n: u64 = std::env::var("POWERDB_MODEL_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25);
    for seed in 1..=n {
        run(seed, 150);
    }
}
