//! Secondary indexes on typed tables: building them on existing rows, keeping them current, finding rows through them, and a random model that
//! compares every lookup with a plain filter and sort of the model's rows.

use powerdb::storage::tables::Row;
use powerdb::types::schema::{Change, TableDef};
use powerdb::types::{Type, Value};
use powerdb::util::new_id;
use powerdb::{Db, Durability, Error, Options};
use std::path::PathBuf;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("powerdb-idx-{}", new_id()));
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

fn clean(db: &Db) {
    let r = db.verify(true).unwrap();
    assert!(r.problems.is_empty(), "{:?}", r.problems);
}

fn i(n: i64) -> Value {
    Value::I64(n)
}
fn t(s: &str) -> Value {
    Value::Text(s.into())
}

fn people(db: &Db) {
    let mut d = TableDef::new("people").unwrap();
    for (n, ty, nullable) in [
        ("id", Type::I64, false),
        ("city", Type::Text, true),
        ("age", Type::I32, true),
        ("email", Type::Text, true),
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

fn index(db: &Db, name: &str, cols: &[&str]) -> Result<TableDef, Error> {
    db.alter_table(
        "people",
        &Change::AddIndex {
            name: name.into(),
            columns: cols.iter().map(|c| c.to_string()).collect(),
        },
    )
}

fn drop_index(db: &Db, name: &str) -> Result<TableDef, Error> {
    db.alter_table("people", &Change::DropIndex { name: name.into() })
}

fn person(db: &Db, id: i64, city: Option<&str>, age: Option<i32>) {
    let mut v = vec![("id", i(id))];
    if let Some(c) = city {
        v.push(("city", t(c)));
    }
    if let Some(a) = age {
        v.push(("age", Value::I32(a)));
    }
    db.insert_row("people", &v).unwrap();
}

fn ids(rows: &[Row]) -> Vec<i64> {
    rows.iter()
        .map(|r| match &r.iter().find(|(n, _)| n == "id").unwrap().1 {
            Some(Value::I64(n)) => *n,
            other => panic!("{other:?}"),
        })
        .collect()
}

fn find(db: &Db, idx: &str, prefix: &[Value]) -> Vec<i64> {
    let mut out = Vec::new();
    let mut after: Option<Vec<u8>> = None;
    loop {
        let page = db
            .scan_index("people", idx, prefix, after.as_deref(), 3)
            .unwrap();
        out.extend(ids(&page.rows));
        match page.next {
            Some(n) => after = Some(n),
            None => return out,
        }
        assert!(out.len() < 100_000, "paging never ended");
    }
}

#[test]
fn an_index_finds_rows_by_value_in_index_order() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    index(&db, "by_city", &["city"]).unwrap();
    for (id, city) in [
        (5, "oslo"),
        (1, "bergen"),
        (9, "oslo"),
        (3, "oslo"),
        (7, "bergen"),
        (2, "tromso"),
    ] {
        person(&db, id, Some(city), None);
    }
    assert_eq!(
        find(&db, "by_city", &[t("oslo")]),
        vec![3, 5, 9],
        "within a value, in primary-key order"
    );
    assert_eq!(find(&db, "by_city", &[t("bergen")]), vec![1, 7]);
    assert_eq!(find(&db, "by_city", &[t("nowhere")]), Vec::<i64>::new());
    // no values: every indexed row, ordered by the indexed value
    assert_eq!(find(&db, "by_city", &[]), vec![1, 7, 3, 5, 9, 2]);
    clean(&db);
}

#[test]
fn an_index_added_to_a_table_with_rows_is_built_from_them() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    for n in 0..30 {
        person(
            &db,
            n,
            Some(if n % 3 == 0 { "a" } else { "b" }),
            Some((n % 5) as i32),
        );
    }
    person(&db, 100, None, Some(1)); // a null city: not in the city index
    index(&db, "by_city", &["city"]).unwrap();
    assert_eq!(find(&db, "by_city", &[t("a")]).len(), 10);
    assert_eq!(
        find(&db, "by_city", &[]).len(),
        30,
        "the null row is not indexed"
    );
    clean(&db);
    // rows written afterwards keep it current
    person(&db, 200, Some("a"), None);
    assert_eq!(find(&db, "by_city", &[t("a")]).len(), 11);
    clean(&db);
}

#[test]
fn updates_and_deletes_keep_the_index_current() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    index(&db, "by_city", &["city"]).unwrap();
    person(&db, 1, Some("a"), None);
    person(&db, 2, Some("a"), None);
    db.update_row("people", &[i(1)], &[("city", Some(t("b")))])
        .unwrap();
    assert_eq!(find(&db, "by_city", &[t("a")]), vec![2]);
    assert_eq!(find(&db, "by_city", &[t("b")]), vec![1]);
    db.update_row("people", &[i(1)], &[("city", None)]).unwrap();
    assert_eq!(
        find(&db, "by_city", &[t("b")]),
        Vec::<i64>::new(),
        "a null leaves the index"
    );
    db.update_row("people", &[i(1)], &[("city", Some(t("a")))])
        .unwrap();
    assert_eq!(
        find(&db, "by_city", &[t("a")]),
        vec![1, 2],
        "and comes back"
    );
    db.update_row("people", &[i(1)], &[("age", Some(Value::I32(5)))])
        .unwrap(); // another column: the index is untouched
    assert_eq!(find(&db, "by_city", &[t("a")]), vec![1, 2]);
    db.delete_row("people", &[i(2)]).unwrap();
    assert_eq!(find(&db, "by_city", &[t("a")]), vec![1]);
    clean(&db);
}

#[test]
fn a_composite_index_is_searched_by_a_leading_prefix() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    index(&db, "city_age", &["city", "age"]).unwrap();
    for (id, city, age) in [
        (1, "a", 30),
        (2, "a", 20),
        (3, "a", 20),
        (4, "b", 20),
        (5, "a", -1),
    ] {
        person(&db, id, Some(city), Some(age));
    }
    person(&db, 6, Some("a"), None); // a null age: not in this index
    assert_eq!(find(&db, "city_age", &[t("a"), Value::I32(20)]), vec![2, 3]);
    assert_eq!(
        find(&db, "city_age", &[t("a")]),
        vec![5, 2, 3, 1],
        "by age within the city, negative first"
    );
    assert_eq!(find(&db, "city_age", &[t("b")]), vec![4]);
    assert_eq!(
        find(&db, "city_age", &[t("a"), Value::I32(99)]),
        Vec::<i64>::new()
    );
    let too_many = db.scan_index(
        "people",
        "city_age",
        &[t("a"), Value::I32(1), Value::I32(2)],
        None,
        10,
    );
    assert!(matches!(too_many, Err(Error::Invalid(_))));
    let wrong_type = db.scan_index("people", "city_age", &[Value::I32(1)], None, 10);
    assert!(matches!(wrong_type, Err(Error::Invalid(_))));
    clean(&db);
}

#[test]
fn paging_through_an_index_returns_every_row_once_for_any_page_size() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    index(&db, "by_age", &["age"]).unwrap();
    for n in 0..40 {
        person(&db, n, None, Some((n % 4) as i32));
    }
    let all = find(&db, "by_age", &[]);
    assert_eq!(all.len(), 40);
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 40, "a row repeated");
    for limit in [1usize, 2, 7, 40, 1000] {
        let mut out = Vec::new();
        let mut after: Option<Vec<u8>> = None;
        for _ in 0..200 {
            let p = db
                .scan_index("people", "by_age", &[], after.as_deref(), limit)
                .unwrap();
            out.extend(ids(&p.rows));
            match p.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        assert_eq!(out, all, "limit {limit}");
    }
}

#[test]
fn the_index_behind_a_unique_constraint_or_foreign_key_can_be_searched_too() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    db.alter_table(
        "people",
        &Change::AddUnique {
            name: "email_u".into(),
            columns: vec!["email".into()],
        },
    )
    .unwrap();
    db.insert_row("people", &[("id", i(1)), ("email", t("a@x"))])
        .unwrap();
    db.insert_row("people", &[("id", i(2)), ("email", t("b@x"))])
        .unwrap();
    assert_eq!(find(&db, "email_u", &[t("b@x")]), vec![2]);
    db.alter_table(
        "people",
        &Change::AddCheck {
            name: "ck".into(),
            expr: "id > 0".into(),
        },
    )
    .unwrap();
    assert!(
        matches!(
            db.scan_index("people", "ck", &[], None, 5),
            Err(Error::Invalid(_))
        ),
        "a check is not an index"
    );
    assert!(matches!(
        db.scan_index("people", "nope", &[], None, 5),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn dropping_an_index_removes_its_entries() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    index(&db, "by_city", &["city"]).unwrap();
    person(&db, 1, Some("a"), None);
    drop_index(&db, "by_city").unwrap();
    assert!(matches!(
        db.scan_index("people", "by_city", &[], None, 5),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        drop_index(&db, "by_city"),
        Err(Error::NotFound(_))
    ));
    clean(&db); // no stray entries
    // and it can be made again, from the rows that exist
    index(&db, "by_city", &["city"]).unwrap();
    assert_eq!(find(&db, "by_city", &[t("a")]), vec![1]);
    clean(&db);
}

#[test]
fn an_indexed_column_cannot_be_dropped_or_retyped_but_can_be_renamed() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    index(&db, "by_age", &["age"]).unwrap();
    person(&db, 1, None, Some(30));
    assert!(
        db.alter_table("people", &Change::DropColumn { name: "age".into() })
            .is_err()
    );
    assert!(
        db.alter_table(
            "people",
            &Change::WidenColumn {
                name: "age".into(),
                to: Type::I64
            }
        )
        .is_err()
    );
    db.alter_table(
        "people",
        &Change::RenameColumn {
            from: "age".into(),
            to: "years".into(),
        },
    )
    .unwrap();
    assert_eq!(
        find(&db, "by_age", &[Value::I32(30)]),
        vec![1],
        "the index follows the rename"
    );
    drop_index(&db, "by_age").unwrap();
    db.alter_table(
        "people",
        &Change::DropColumn {
            name: "years".into(),
        },
    )
    .unwrap();
    clean(&db);
}

#[test]
fn index_definitions_are_checked() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    index(&db, "ix", &["city"]).unwrap();
    assert!(
        matches!(index(&db, "ix", &["age"]), Err(Error::Conflict(_))),
        "a repeated name"
    );
    db.alter_table(
        "people",
        &Change::AddUnique {
            name: "uq".into(),
            columns: vec!["email".into()],
        },
    )
    .unwrap();
    assert!(
        matches!(index(&db, "uq", &["age"]), Err(Error::Conflict(_))),
        "a name a constraint has"
    );
    assert!(index(&db, "bad name", &["age"]).is_err());
    assert!(index(&db, "none", &[]).is_err(), "no columns");
    assert!(index(&db, "none", &["nope"]).is_err(), "an unknown column");
    assert!(
        index(&db, "none", &["age", "age"]).is_err(),
        "a column twice"
    );
    // a json column cannot be indexed
    let mut j = TableDef::new("j").unwrap();
    j = j
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
    j = j
        .apply(
            &Change::AddColumn {
                name: "doc".into(),
                ty: Type::Json,
                nullable: true,
                default: None,
            },
            true,
        )
        .unwrap()
        .0;
    j.set_primary_key(&["id"]).unwrap();
    db.create_table(j).unwrap();
    assert!(
        db.alter_table(
            "j",
            &Change::AddIndex {
                name: "d".into(),
                columns: vec!["doc".into()]
            }
        )
        .is_err()
    );
}

#[test]
fn indexes_survive_a_restart_and_a_table_drop_removes_them() {
    let d = Dir::new();
    {
        let db = open(&d);
        people(&db);
        index(&db, "by_city", &["city"]).unwrap();
        person(&db, 1, Some("a"), None);
        person(&db, 2, Some("a"), None);
    }
    let db = open(&d);
    assert_eq!(find(&db, "by_city", &[t("a")]), vec![1, 2]);
    clean(&db);
    db.drop_table("people").unwrap();
    clean(&db); // the table's index entries went with it
}

#[test]
fn check_finds_a_damaged_index() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    index(&db, "by_city", &["city"]).unwrap();
    person(&db, 1, Some("a"), None);
    person(&db, 2, Some("b"), None);
    clean(&db);
    let idx = db.keyspace("indexes").unwrap();
    let first = idx.iter().next().unwrap().key().unwrap().to_vec();
    idx.remove(first).unwrap();
    assert!(
        db.verify(false)
            .unwrap()
            .problems
            .iter()
            .any(|p| p.contains("missing"))
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

/// id -> (a, b)
type Rows = std::collections::BTreeMap<i64, (Option<i32>, Option<String>)>;

/// The model of a lookup: rows with every indexed column non-null and the leading values equal, ordered by the indexed values and then the id.
fn expect(rows: &Rows, cols: &[&str], prefix: &[Value]) -> Vec<i64> {
    let mut hits: Vec<(Vec<u8>, i64)> = Vec::new();
    for (id, (a, b)) in rows {
        let mut key: Vec<Vec<u8>> = Vec::new();
        let mut ok = true;
        for (n, c) in cols.iter().enumerate() {
            let v: Option<Vec<u8>> = match *c {
                "a" => a.map(|x| ((x as u32) ^ (1 << 31)).to_be_bytes().to_vec()),
                _ => b.as_ref().map(|s| s.as_bytes().to_vec()),
            };
            match v {
                None => {
                    ok = false;
                    break;
                }
                Some(bytes) => {
                    if n < prefix.len() {
                        let want = match (&prefix[n], *c) {
                            (Value::I32(x), "a") => {
                                ((*x as u32) ^ (1 << 31)).to_be_bytes().to_vec()
                            }
                            (Value::Text(s), "b") => s.as_bytes().to_vec(),
                            _ => unreachable!(),
                        };
                        if bytes != want {
                            ok = false;
                            break;
                        }
                    }
                    key.push(bytes);
                }
            }
        }
        if ok {
            // a text value compares as its bytes, and "x" sorts before "x\0..": encode with a terminator the way the index does
            let mut flat = Vec::new();
            for (n, k) in key.iter().enumerate() {
                if cols[n] == "b" {
                    for byte in k {
                        flat.push(*byte);
                        if *byte == 0 {
                            flat.push(0xFF);
                        }
                    }
                    flat.extend_from_slice(&[0, 0]);
                } else {
                    flat.extend_from_slice(k);
                }
            }
            hits.push((flat, *id));
        }
    }
    hits.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.cmp(&y.1)));
    hits.into_iter().map(|(_, id)| id).collect()
}

fn run(seed: u64, steps: usize) {
    let d = Dir::new();
    let mut r = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
    let mut db = Some(open(&d));
    {
        let db = db.as_ref().unwrap();
        let mut dd = TableDef::new("m").unwrap();
        for (n, ty, nullable) in [
            ("id", Type::I64, false),
            ("a", Type::I32, true),
            ("b", Type::Text, true),
        ] {
            dd = dd
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
        dd.set_primary_key(&["id"]).unwrap();
        db.create_table(dd).unwrap();
    }
    let defs: [(&str, &[&str]); 3] = [("ia", &["a"]), ("ib", &["b"]), ("iab", &["a", "b"])];
    let mut present = [false; 3];
    let mut rows = Rows::new();
    let word = |r: &mut Rng| -> Option<String> {
        if r.below(4) == 0 {
            None
        } else {
            Some(format!("w{}", r.below(5)))
        }
    };
    for step in 0..steps {
        let dbr = db.as_ref().unwrap();
        let ctx = format!("seed {seed} step {step}");
        match r.below(100) {
            0..=39 => {
                let id = r.below(40) as i64;
                let (a, b) = (
                    if r.below(4) == 0 {
                        None
                    } else {
                        Some(r.below(6) as i32 - 2)
                    },
                    word(&mut r),
                );
                let mut v = vec![("id", i(id))];
                if let Some(x) = a {
                    v.push(("a", Value::I32(x)));
                }
                if let Some(s) = &b {
                    v.push(("b", t(s)));
                }
                let res = dbr.insert_row("m", &v);
                assert_eq!(res.is_ok(), !rows.contains_key(&id), "{ctx}: insert {id}");
                if res.is_ok() {
                    rows.insert(id, (a, b));
                }
            }
            40..=59 => {
                let id = r.below(40) as i64;
                let patch: (&str, Option<Value>) = if r.below(2) == 0 {
                    (
                        "a",
                        if r.below(3) == 0 {
                            None
                        } else {
                            Some(Value::I32(r.below(6) as i32 - 2))
                        },
                    )
                } else {
                    ("b", word(&mut r).map(|s| t(&s)))
                };
                let res = dbr.update_row("m", &[i(id)], &[(patch.0, patch.1.clone())]);
                assert_eq!(res.is_ok(), rows.contains_key(&id), "{ctx}: update {id}");
                if res.is_ok() {
                    let e = rows.get_mut(&id).unwrap();
                    match (patch.0, patch.1) {
                        ("a", Some(Value::I32(x))) => e.0 = Some(x),
                        ("a", _) => e.0 = None,
                        (_, Some(Value::Text(s))) => e.1 = Some(s),
                        _ => e.1 = None,
                    }
                }
            }
            60..=72 => {
                let id = r.below(40) as i64;
                assert_eq!(
                    dbr.delete_row("m", &[i(id)]).unwrap(),
                    rows.remove(&id).is_some(),
                    "{ctx}: delete"
                );
            }
            73..=80 => {
                // add or drop an index: built from the rows that exist now
                let k = r.below(3);
                let (name, cols) = defs[k];
                if present[k] {
                    dbr.alter_table("m", &Change::DropIndex { name: name.into() })
                        .unwrap();
                } else {
                    dbr.alter_table(
                        "m",
                        &Change::AddIndex {
                            name: name.into(),
                            columns: cols.iter().map(|c| c.to_string()).collect(),
                        },
                    )
                    .unwrap();
                }
                present[k] = !present[k];
            }
            81..=85 => {
                drop(db.take());
                db = Some(open(&d));
            }
            _ => {}
        }
        let dbr = db.as_ref().unwrap();
        // every present index answers every kind of lookup as the model says
        for (k, (name, cols)) in defs.iter().enumerate() {
            if !present[k] {
                assert!(
                    matches!(
                        dbr.scan_index("m", name, &[], None, 5),
                        Err(Error::NotFound(_))
                    ),
                    "{ctx}: {name} should not exist"
                );
                continue;
            }
            let prefixes: Vec<Vec<Value>> = match *name {
                "ia" => vec![vec![], vec![Value::I32(r.below(6) as i32 - 2)]],
                "ib" => vec![vec![], vec![t(&format!("w{}", r.below(5)))]],
                _ => vec![
                    vec![],
                    vec![Value::I32(r.below(6) as i32 - 2)],
                    vec![
                        Value::I32(r.below(6) as i32 - 2),
                        t(&format!("w{}", r.below(5))),
                    ],
                ],
            };
            for p in prefixes {
                let mut got = Vec::new();
                let mut after: Option<Vec<u8>> = None;
                for _ in 0..200 {
                    let page = dbr.scan_index("m", name, &p, after.as_deref(), 3).unwrap();
                    got.extend(ids(&page.rows));
                    match page.next {
                        Some(n) => after = Some(n),
                        None => break,
                    }
                }
                assert_eq!(got, expect(&rows, cols, &p), "{ctx}: {name} {p:?}");
            }
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
}

#[test]
fn random_changes_and_lookups_agree_with_the_model() {
    let n: u64 = std::env::var("POWERDB_MODEL_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25);
    for seed in 1..=n {
        run(seed, 160);
    }
}

/// Measured, not asserted: `cargo test --release --test indexes -- --ignored --nocapture`.
#[test]
#[ignore]
fn an_index_lookup_against_a_scan() {
    let d = Dir::new();
    let db = open(&d);
    people(&db);
    let n = 100_000i64;
    for k in 0..n {
        db.insert_row(
            "people",
            &[
                ("id", i(k)),
                ("city", t(&format!("city{}", k % 1000))),
                ("age", Value::I32((k % 90) as i32)),
            ],
        )
        .unwrap();
    }
    let t0 = std::time::Instant::now();
    index(&db, "by_city", &["city"]).unwrap();
    println!("built an index over {n} rows in {:?}", t0.elapsed());
    let t1 = std::time::Instant::now();
    let mut hits = 0;
    for q in 0..200 {
        hits += db
            .scan_index(
                "people",
                "by_city",
                &[t(&format!("city{}", q * 5))],
                None,
                1000,
            )
            .unwrap()
            .rows
            .len();
    }
    let via_index = t1.elapsed();
    let t2 = std::time::Instant::now();
    let mut scan_hits = 0;
    for q in 0..5 {
        let want = format!("city{}", q * 5);
        let mut after: Option<Vec<Value>> = None;
        loop {
            let page = db.scan_rows("people", after.as_deref(), 10_000).unwrap();
            if page.is_empty() {
                break;
            }
            scan_hits += page
                .iter()
                .filter(|r| r.iter().any(|(nm, v)| nm == "city" && *v == Some(t(&want))))
                .count();
            after = Some(vec![
                page.last()
                    .unwrap()
                    .iter()
                    .find(|(nm, _)| nm == "id")
                    .unwrap()
                    .1
                    .clone()
                    .unwrap(),
            ]);
        }
    }
    let via_scan = t2.elapsed();
    println!(
        "200 indexed lookups ({hits} rows): {via_index:?}; 5 full scans ({scan_hits} rows): {via_scan:?}"
    );
}
