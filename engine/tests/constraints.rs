//! Constraints inside the commit: generated values, UNIQUE, CHECK and FOREIGN KEY (with restrict, cascade and set null), their checking by
//! `powerdb check`, and a random-operations model over four linked tables.

use powerdb::storage::tables::Row;
use powerdb::types::schema::{Change, Generator, OnDelete, TableDef};
use powerdb::types::{Type, Value};
use powerdb::util::new_id;
use powerdb::{Db, Durability, Error, Options};
use std::collections::BTreeMap;
use std::path::PathBuf;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("powerdb-con-{}", new_id()));
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

/// A table with these columns (name, type, nullable) and the primary key `pk`.
fn make(db: &Db, name: &str, cols: &[(&str, Type, bool)], pk: &[&str]) -> TableDef {
    let mut t = TableDef::new(name).unwrap();
    for (n, ty, nullable) in cols {
        t = t
            .apply(
                &Change::AddColumn {
                    name: (*n).into(),
                    ty: ty.clone(),
                    nullable: *nullable,
                    default: None,
                },
                true,
            )
            .unwrap()
            .0;
    }
    t.set_primary_key(pk).unwrap();
    db.create_table(t).unwrap()
}

fn unique(db: &Db, table: &str, name: &str, cols: &[&str]) -> Result<TableDef, Error> {
    db.alter_table(
        table,
        &Change::AddUnique {
            name: name.into(),
            columns: cols.iter().map(|c| c.to_string()).collect(),
        },
    )
}

fn check(db: &Db, table: &str, name: &str, expr: &str) -> Result<TableDef, Error> {
    db.alter_table(
        table,
        &Change::AddCheck {
            name: name.into(),
            expr: expr.into(),
        },
    )
}

fn fk(
    db: &Db,
    table: &str,
    name: &str,
    cols: &[&str],
    to: &str,
    on_delete: OnDelete,
) -> Result<TableDef, Error> {
    db.alter_table(
        table,
        &Change::AddForeignKey {
            name: name.into(),
            columns: cols.iter().map(|c| c.to_string()).collect(),
            ref_table: to.into(),
            ref_table_id: 0,
            ref_pk_types: vec![],
            on_delete,
        },
    )
}

fn drop_constraint(db: &Db, table: &str, name: &str) -> Result<TableDef, Error> {
    db.alter_table(table, &Change::DropConstraint { name: name.into() })
}

fn get<'a>(row: &'a Row, name: &str) -> &'a Option<Value> {
    &row.iter().find(|(n, _)| n == name).unwrap().1
}

fn i(n: i64) -> Value {
    Value::I64(n)
}
fn t(s: &str) -> Value {
    Value::Text(s.into())
}

fn ins(db: &Db, table: &str, v: &[(&str, Value)]) -> Result<Vec<Value>, Error> {
    db.insert_row(table, v)
}

// ---- generated values ----

#[test]
fn generated_columns_fill_in_what_an_insert_leaves_out() {
    let d = Dir::new();
    let db = open(&d);
    make(&db, "events", &[("name", Type::Text, false)], &["name"]);
    for (name, ty, g) in [
        ("id", Type::Uuid, Generator::Uuid),
        ("at", Type::TimestampTz, Generator::Now),
        ("seq", Type::I32, Generator::Sequence),
    ] {
        db.alter_table(
            "events",
            &Change::AddGeneratedColumn {
                name: name.into(),
                ty,
                nullable: false,
                generate: g,
            },
        )
        .unwrap();
    }
    let mut ids = std::collections::HashSet::new();
    for n in 1..=5i32 {
        let pk = ins(&db, "events", &[("name", t(&format!("e{n}")))]).unwrap();
        let row = db.get_row("events", &pk).unwrap().unwrap();
        let Some(Value::Uuid(u)) = get(&row, "id") else {
            panic!()
        };
        assert_eq!(u[6] >> 4, 7, "a version 7 uuid");
        assert_eq!(u[8] >> 6, 2, "the RFC variant");
        assert!(ids.insert(*u), "a uuid repeated");
        assert_eq!(
            get(&row, "seq"),
            &Some(Value::I32(n)),
            "the sequence counts up"
        );
        let Some(Value::TimestampTz(us)) = get(&row, "at") else {
            panic!()
        };
        let now = powerdb::util::now_ms() * 1000;
        assert!((now - us).abs() < 60_000_000, "the time of the insert");
    }
    // the sequence does not reuse numbers after a delete, or after a restart
    db.delete_row("events", &[t("e5")]).unwrap();
    drop(db);
    let db = open(&d);
    let pk = ins(&db, "events", &[("name", t("e6"))]).unwrap();
    assert_eq!(
        get(&db.get_row("events", &pk).unwrap().unwrap(), "seq"),
        &Some(Value::I32(6))
    );
    // an explicit value wins over the generator
    let pk = ins(
        &db,
        "events",
        &[("name", t("e7")), ("seq", Value::I32(100))],
    )
    .unwrap();
    assert_eq!(
        get(&db.get_row("events", &pk).unwrap().unwrap(), "seq"),
        &Some(Value::I32(100))
    );
    clean(&db);
}

#[test]
fn a_failed_insert_does_not_use_up_a_sequence_number() {
    let d = Dir::new();
    let db = open(&d);
    make(&db, "tickets", &[("code", Type::Text, false)], &["code"]);
    db.alter_table(
        "tickets",
        &Change::AddGeneratedColumn {
            name: "n".into(),
            ty: Type::I64,
            nullable: false,
            generate: Generator::Sequence,
        },
    )
    .unwrap();
    ins(&db, "tickets", &[("code", t("a"))]).unwrap();
    assert!(
        ins(&db, "tickets", &[("code", t("a"))]).is_err(),
        "a duplicate key"
    );
    let pk = ins(&db, "tickets", &[("code", t("b"))]).unwrap();
    assert_eq!(
        get(&db.get_row("tickets", &pk).unwrap().unwrap(), "n"),
        &Some(i(2))
    );
}

#[test]
fn generated_columns_have_rules() {
    let d = Dir::new();
    let db = open(&d);
    make(&db, "g", &[("k", Type::I64, false)], &["k"]);
    let add = |name: &str, ty: Type, nullable: bool, g: Generator| {
        db.alter_table(
            "g",
            &Change::AddGeneratedColumn {
                name: name.into(),
                ty,
                nullable,
                generate: g,
            },
        )
    };
    assert!(
        add("a", Type::Text, true, Generator::Uuid).is_err(),
        "uuid needs a uuid column"
    );
    assert!(
        add("b", Type::I32, true, Generator::Now).is_err(),
        "now needs a time column"
    );
    assert!(
        add("c", Type::Text, true, Generator::Sequence).is_err(),
        "sequence needs an integer"
    );
    ins(&db, "g", &[("k", i(1))]).unwrap();
    assert!(
        add("d", Type::Uuid, false, Generator::Uuid).is_err(),
        "NOT NULL cannot be generated for rows that exist"
    );
    assert!(add("e", Type::Uuid, true, Generator::Uuid).is_ok());
    let row = db.get_row("g", &[i(1)]).unwrap().unwrap();
    assert_eq!(get(&row, "e"), &None, "an old row has no generated value");
    // a sequence runs out at the largest value its column holds
    db.alter_table(
        "g",
        &Change::AddGeneratedColumn {
            name: "tiny".into(),
            ty: Type::I8,
            nullable: true,
            generate: Generator::Sequence,
        },
    )
    .unwrap();
    for k in 2..=128 {
        ins(&db, "g", &[("k", i(k))]).unwrap();
    }
    assert!(
        matches!(ins(&db, "g", &[("k", i(500))]), Err(Error::Invalid(_))),
        "the 128th sequence value does not fit an int8"
    );
}

// ---- unique ----

#[test]
fn unique_constraints_are_enforced_on_insert_and_update() {
    let d = Dir::new();
    let db = open(&d);
    make(
        &db,
        "users",
        &[
            ("id", Type::I64, false),
            ("email", Type::Text, true),
            ("org", Type::Text, true),
            ("handle", Type::Text, true),
        ],
        &["id"],
    );
    unique(&db, "users", "email_unique", &["email"]).unwrap();
    ins(&db, "users", &[("id", i(1)), ("email", t("a@x"))]).unwrap();
    let dup = ins(&db, "users", &[("id", i(2)), ("email", t("a@x"))]);
    assert!(matches!(dup, Err(Error::Conflict(_))), "{dup:?}");
    assert!(dup.unwrap_err().to_string().contains("email_unique"));
    assert!(
        db.get_row("users", &[i(2)]).unwrap().is_none(),
        "a refused insert wrote nothing"
    );
    // nulls are distinct
    ins(&db, "users", &[("id", i(3))]).unwrap();
    ins(&db, "users", &[("id", i(4))]).unwrap();
    // updating to another row's value is refused; to its own is fine; to a free value moves the entry
    ins(&db, "users", &[("id", i(5)), ("email", t("b@x"))]).unwrap();
    assert!(matches!(
        db.update_row("users", &[i(5)], &[("email", Some(t("a@x")))]),
        Err(Error::Conflict(_))
    ));
    db.update_row("users", &[i(5)], &[("email", Some(t("b@x")))])
        .unwrap();
    db.update_row("users", &[i(5)], &[("email", Some(t("c@x")))])
        .unwrap();
    ins(&db, "users", &[("id", i(6)), ("email", t("b@x"))]).unwrap(); // the old value is free again
    assert!(matches!(
        ins(&db, "users", &[("id", i(7)), ("email", t("c@x"))]),
        Err(Error::Conflict(_))
    ));
    // a null makes it unconstrained, and back
    db.update_row("users", &[i(5)], &[("email", None)]).unwrap();
    ins(&db, "users", &[("id", i(7)), ("email", t("c@x"))]).unwrap();
    // deleting frees the value
    db.delete_row("users", &[i(7)]).unwrap();
    ins(&db, "users", &[("id", i(8)), ("email", t("c@x"))]).unwrap();
    // composite: the pair must be unique, one column may repeat
    unique(&db, "users", "org_handle", &["org", "handle"]).unwrap();
    ins(
        &db,
        "users",
        &[("id", i(10)), ("org", t("o")), ("handle", t("h"))],
    )
    .unwrap();
    ins(
        &db,
        "users",
        &[("id", i(11)), ("org", t("o")), ("handle", t("h2"))],
    )
    .unwrap();
    ins(
        &db,
        "users",
        &[("id", i(12)), ("org", t("o2")), ("handle", t("h"))],
    )
    .unwrap();
    assert!(matches!(
        ins(
            &db,
            "users",
            &[("id", i(13)), ("org", t("o")), ("handle", t("h"))]
        ),
        Err(Error::Conflict(_))
    ));
    ins(&db, "users", &[("id", i(14)), ("org", t("o"))]).unwrap(); // a null in the pair: unconstrained
    ins(&db, "users", &[("id", i(15)), ("org", t("o"))]).unwrap();
    clean(&db);
}

#[test]
fn adding_unique_to_a_table_with_rows_checks_them_and_builds_the_index() {
    let d = Dir::new();
    let db = open(&d);
    make(
        &db,
        "p",
        &[("id", Type::I64, false), ("code", Type::Text, true)],
        &["id"],
    );
    ins(&db, "p", &[("id", i(1)), ("code", t("x"))]).unwrap();
    ins(&db, "p", &[("id", i(2)), ("code", t("x"))]).unwrap();
    ins(&db, "p", &[("id", i(3))]).unwrap();
    let before = db.table("p").unwrap();
    let err = unique(&db, "p", "code_u", &["code"]).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert_eq!(
        db.table("p").unwrap(),
        before,
        "a refused constraint left the table as it was"
    );
    clean(&db);
    db.delete_row("p", &[i(2)]).unwrap();
    unique(&db, "p", "code_u", &["code"]).unwrap();
    assert!(
        matches!(
            ins(&db, "p", &[("id", i(9)), ("code", t("x"))]),
            Err(Error::Conflict(_))
        ),
        "the index built from existing rows is used"
    );
    clean(&db);
    // dropping it removes the entries; the value can repeat
    drop_constraint(&db, "p", "code_u").unwrap();
    ins(&db, "p", &[("id", i(9)), ("code", t("x"))]).unwrap();
    clean(&db);
}

// ---- check ----

#[test]
fn check_constraints_reject_rows_that_fail_them() {
    let d = Dir::new();
    let db = open(&d);
    make(
        &db,
        "people",
        &[
            ("id", Type::I64, false),
            ("age", Type::I32, true),
            ("status", Type::Text, true),
            ("score", Type::F64, true),
        ],
        &["id"],
    );
    check(&db, "people", "adult", "age == null || age >= 18").unwrap();
    check(
        &db,
        "people",
        "status_ok",
        "status == null || status in [\"new\", \"active\"]",
    )
    .unwrap();
    ins(
        &db,
        "people",
        &[("id", i(1)), ("age", Value::I32(30)), ("status", t("new"))],
    )
    .unwrap();
    ins(&db, "people", &[("id", i(2))]).unwrap(); // nulls pass these checks as written
    let bad = ins(&db, "people", &[("id", i(3)), ("age", Value::I32(10))]);
    assert!(
        matches!(bad, Err(Error::Invalid(_))) && bad.unwrap_err().to_string().contains("adult")
    );
    assert!(matches!(
        ins(&db, "people", &[("id", i(3)), ("status", t("gone"))]),
        Err(Error::Invalid(_))
    ));
    assert!(db.get_row("people", &[i(3)]).unwrap().is_none());
    assert!(matches!(
        db.update_row("people", &[i(1)], &[("age", Some(Value::I32(5)))]),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        get(&db.get_row("people", &[i(1)]).unwrap().unwrap(), "age"),
        &Some(Value::I32(30)),
        "a refused update changed nothing"
    );
    // a check written without the null clause fails on null: the comparison is false
    check(&db, "people", "strict", "score >= 0").unwrap_err(); // existing rows have no score
    drop_constraint(&db, "people", "adult").unwrap();
    ins(&db, "people", &[("id", i(3)), ("age", Value::I32(10))]).unwrap();
    clean(&db);
}

/// Regression: a column named `id` is the commonest primary key there is, and the expression language reads a bare `id` as the record's own id,
/// which for a table row is empty: a check on `id` rejected every row.
#[test]
fn a_check_can_read_a_column_called_id() {
    let d = Dir::new();
    let db = open(&d);
    make(
        &db,
        "things",
        &[("id", Type::I64, false), ("name", Type::Text, true)],
        &["id"],
    );
    check(&db, "things", "positive_id", "id > 0").unwrap();
    ins(&db, "things", &[("id", i(5))]).unwrap();
    assert!(matches!(
        ins(&db, "things", &[("id", i(-1))]),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        ins(&db, "things", &[("id", i(0))]),
        Err(Error::Invalid(_))
    ));
    // the same through a table that already has rows
    make(&db, "more", &[("id", Type::I64, false)], &["id"]);
    ins(&db, "more", &[("id", i(3))]).unwrap();
    check(&db, "more", "small", "id < 10").unwrap();
    assert!(
        check(&db, "more", "tiny", "id < 2").is_err(),
        "an existing row fails it"
    );
    clean(&db);
}

#[test]
fn adding_a_check_validates_existing_rows_and_the_expression() {
    let d = Dir::new();
    let db = open(&d);
    make(
        &db,
        "t",
        &[("id", Type::I64, false), ("n", Type::I32, true)],
        &["id"],
    );
    ins(&db, "t", &[("id", i(1)), ("n", Value::I32(-5))]).unwrap();
    let before = db.table("t").unwrap();
    assert!(
        matches!(check(&db, "t", "positive", "n > 0"), Err(Error::Invalid(_))),
        "an existing row fails it"
    );
    assert_eq!(db.table("t").unwrap(), before);
    assert!(
        check(&db, "t", "bad", "nosuchcolumn > 0").is_err(),
        "an unknown column"
    );
    assert!(
        check(&db, "t", "bad", "n >").is_err(),
        "an expression that does not parse"
    );
    assert!(
        check(&db, "t", "bad", "@request.auth.id != \"\"").is_err(),
        "a request value"
    );
    assert!(
        check(&db, "t", "Bad Name", "n > 0").is_err(),
        "a bad constraint name"
    );
    check(&db, "t", "negative", "n < 0").unwrap();
    assert!(
        matches!(
            check(&db, "t", "negative", "n < 0"),
            Err(Error::Conflict(_))
        ),
        "a repeated name"
    );
    // a column a check reads cannot be dropped or renamed from under it
    assert!(
        db.alter_table("t", &Change::DropColumn { name: "n".into() })
            .is_err()
    );
    assert!(
        db.alter_table(
            "t",
            &Change::RenameColumn {
                from: "n".into(),
                to: "m".into()
            }
        )
        .is_err()
    );
    drop_constraint(&db, "t", "negative").unwrap();
    db.alter_table(
        "t",
        &Change::RenameColumn {
            from: "n".into(),
            to: "m".into(),
        },
    )
    .unwrap();
    clean(&db);
}

// ---- foreign keys ----

fn parent_child(db: &Db, on_delete: OnDelete) {
    make(
        db,
        "parents",
        &[("id", Type::I64, false), ("name", Type::Text, true)],
        &["id"],
    );
    make(
        db,
        "children",
        &[
            ("id", Type::I64, false),
            ("parent_id", Type::I64, true),
            ("note", Type::Text, true),
        ],
        &["id"],
    );
    fk(
        db,
        "children",
        "child_parent",
        &["parent_id"],
        "parents",
        on_delete,
    )
    .unwrap();
}

#[test]
fn a_foreign_key_requires_its_row_to_exist() {
    let d = Dir::new();
    let db = open(&d);
    parent_child(&db, OnDelete::Restrict);
    ins(&db, "parents", &[("id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(10)), ("parent_id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(11))]).unwrap(); // null refers to nothing
    let e = ins(&db, "children", &[("id", i(12)), ("parent_id", i(99))]).unwrap_err();
    assert!(
        matches!(e, Error::Conflict(_)) && e.to_string().contains("child_parent"),
        "{e}"
    );
    assert!(db.get_row("children", &[i(12)]).unwrap().is_none());
    assert!(matches!(
        db.update_row("children", &[i(10)], &[("parent_id", Some(i(99)))]),
        Err(Error::Conflict(_))
    ));
    db.update_row("children", &[i(11)], &[("parent_id", Some(i(1)))])
        .unwrap();
    db.update_row("children", &[i(11)], &[("parent_id", None)])
        .unwrap();
    clean(&db);
}

#[test]
fn a_foreign_key_definition_is_checked() {
    let d = Dir::new();
    let db = open(&d);
    make(
        &db,
        "a",
        &[
            ("id", Type::I64, false),
            ("s", Type::Text, true),
            ("n", Type::I64, false),
        ],
        &["id"],
    );
    make(
        &db,
        "b",
        &[
            ("x", Type::Text, false),
            ("y", Type::I64, false),
            ("z", Type::I64, true),
            ("w", Type::I32, true),
        ],
        &["x", "y"],
    );
    assert!(
        fk(&db, "a", "k", &["s"], "b", OnDelete::Restrict).is_err(),
        "b has a two-column key"
    );
    assert!(
        fk(&db, "a", "k", &["s", "n"], "b", OnDelete::Restrict).is_ok(),
        "text then int64 matches b's key"
    );
    assert!(
        fk(&db, "a", "k2", &["n"], "a", OnDelete::Restrict).is_ok(),
        "a table may refer to itself"
    );
    assert!(
        fk(&db, "a", "k3", &["s"], "a", OnDelete::Restrict).is_err(),
        "text against an int64 key"
    );
    assert!(
        fk(&db, "a", "k4", &["n"], "nosuch", OnDelete::Restrict).is_err(),
        "an unknown table"
    );
    assert!(
        fk(&db, "a", "k5", &["nope"], "a", OnDelete::Restrict).is_err(),
        "an unknown column"
    );
    assert!(
        fk(&db, "a", "k6", &["n"], "a", OnDelete::SetNull).is_err(),
        "set null needs a nullable column"
    );
    // a column named by a foreign key cannot be dropped or widened
    assert!(
        db.alter_table("a", &Change::DropColumn { name: "s".into() })
            .is_err()
    );
    assert!(
        db.alter_table(
            "b",
            &Change::WidenColumn {
                name: "w".into(),
                to: Type::I64
            }
        )
        .is_ok(),
        "w is in no constraint"
    );
    clean(&db);
}

#[test]
fn restrict_refuses_the_delete_while_rows_refer_to_the_row() {
    let d = Dir::new();
    let db = open(&d);
    parent_child(&db, OnDelete::Restrict);
    ins(&db, "parents", &[("id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(10)), ("parent_id", i(1))]).unwrap();
    let e = db.delete_row("parents", &[i(1)]).unwrap_err();
    assert!(
        matches!(e, Error::Conflict(_)) && e.to_string().contains("child_parent"),
        "{e}"
    );
    assert!(
        db.get_row("parents", &[i(1)]).unwrap().is_some(),
        "the row is still there"
    );
    db.delete_row("children", &[i(10)]).unwrap();
    assert!(db.delete_row("parents", &[i(1)]).unwrap());
    clean(&db);
}

#[test]
fn cascade_deletes_the_rows_that_refer_to_the_row_and_theirs() {
    let d = Dir::new();
    let db = open(&d);
    parent_child(&db, OnDelete::Cascade);
    make(
        &db,
        "grandchildren",
        &[("id", Type::I64, false), ("child_id", Type::I64, true)],
        &["id"],
    );
    fk(
        &db,
        "grandchildren",
        "gc_child",
        &["child_id"],
        "children",
        OnDelete::Cascade,
    )
    .unwrap();
    for p in 1..=2 {
        ins(&db, "parents", &[("id", i(p))]).unwrap();
        for c in 1..=3 {
            ins(
                &db,
                "children",
                &[("id", i(p * 10 + c)), ("parent_id", i(p))],
            )
            .unwrap();
            ins(
                &db,
                "grandchildren",
                &[("id", i(p * 100 + c)), ("child_id", i(p * 10 + c))],
            )
            .unwrap();
        }
    }
    assert!(db.delete_row("parents", &[i(1)]).unwrap());
    assert_eq!(db.count_rows("parents").unwrap(), 1);
    assert_eq!(
        db.count_rows("children").unwrap(),
        3,
        "the other parent's children are untouched"
    );
    assert_eq!(db.count_rows("grandchildren").unwrap(), 3);
    clean(&db);
}

#[test]
fn set_null_clears_the_reference_and_keeps_the_row() {
    let d = Dir::new();
    let db = open(&d);
    parent_child(&db, OnDelete::SetNull);
    ins(&db, "parents", &[("id", i(1))]).unwrap();
    ins(
        &db,
        "children",
        &[("id", i(10)), ("parent_id", i(1)), ("note", t("keep me"))],
    )
    .unwrap();
    db.delete_row("parents", &[i(1)]).unwrap();
    let row = db.get_row("children", &[i(10)]).unwrap().unwrap();
    assert_eq!(get(&row, "parent_id"), &None);
    assert_eq!(get(&row, "note"), &Some(t("keep me")));
    clean(&db);
}

#[test]
fn a_self_referencing_table_cascades_down_a_tree() {
    let d = Dir::new();
    let db = open(&d);
    make(
        &db,
        "nodes",
        &[("id", Type::I64, false), ("parent", Type::I64, true)],
        &["id"],
    );
    fk(
        &db,
        "nodes",
        "tree",
        &["parent"],
        "nodes",
        OnDelete::Cascade,
    )
    .unwrap();
    // 1 -> 2, 3 ; 2 -> 4, 5 ; 4 -> 6 ; and a separate root 7
    for (id, parent) in [
        (1, None),
        (2, Some(1)),
        (3, Some(1)),
        (4, Some(2)),
        (5, Some(2)),
        (6, Some(4)),
        (7, None),
    ] {
        let mut v = vec![("id", i(id))];
        if let Some(p) = parent {
            v.push(("parent", i(p)));
        }
        ins(&db, "nodes", &v).unwrap();
    }
    db.delete_row("nodes", &[i(2)]).unwrap();
    let left: Vec<i64> = db
        .scan_rows("nodes", None, 100)
        .unwrap()
        .iter()
        .map(|r| match get(r, "id") {
            Some(Value::I64(n)) => *n,
            _ => panic!(),
        })
        .collect();
    assert_eq!(left, vec![1, 3, 7], "2 and everything under it went");
    clean(&db);
}

#[test]
fn a_restrict_deep_in_a_cascade_refuses_the_whole_delete() {
    let d = Dir::new();
    let db = open(&d);
    parent_child(&db, OnDelete::Cascade);
    make(
        &db,
        "pins",
        &[("id", Type::I64, false), ("child_id", Type::I64, true)],
        &["id"],
    );
    fk(
        &db,
        "pins",
        "pin_child",
        &["child_id"],
        "children",
        OnDelete::Restrict,
    )
    .unwrap();
    ins(&db, "parents", &[("id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(10)), ("parent_id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(11)), ("parent_id", i(1))]).unwrap();
    ins(&db, "pins", &[("id", i(100)), ("child_id", i(11))]).unwrap();
    assert!(matches!(
        db.delete_row("parents", &[i(1)]),
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        db.count_rows("parents").unwrap(),
        1,
        "the parent is still there"
    );
    assert_eq!(
        db.count_rows("children").unwrap(),
        2,
        "and so are both children: none of the delete happened"
    );
    clean(&db);
}

#[test]
fn set_null_that_would_break_a_check_refuses_the_delete() {
    let d = Dir::new();
    let db = open(&d);
    parent_child(&db, OnDelete::SetNull);
    check(&db, "children", "needs_parent", "parent_id != null").unwrap();
    ins(&db, "parents", &[("id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(10)), ("parent_id", i(1))]).unwrap();
    assert!(matches!(
        db.delete_row("parents", &[i(1)]),
        Err(Error::Invalid(_))
    ));
    assert_eq!(db.count_rows("parents").unwrap(), 1);
    assert_eq!(
        get(
            &db.get_row("children", &[i(10)]).unwrap().unwrap(),
            "parent_id"
        ),
        &Some(i(1))
    );
    clean(&db);
}

#[test]
fn adding_a_foreign_key_to_rows_that_exist_checks_them() {
    let d = Dir::new();
    let db = open(&d);
    make(&db, "parents", &[("id", Type::I64, false)], &["id"]);
    make(
        &db,
        "children",
        &[("id", Type::I64, false), ("parent_id", Type::I64, true)],
        &["id"],
    );
    ins(&db, "parents", &[("id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(10)), ("parent_id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(11)), ("parent_id", i(2))]).unwrap(); // dangling
    let before = db.table("children").unwrap();
    assert!(matches!(
        fk(
            &db,
            "children",
            "fk",
            &["parent_id"],
            "parents",
            OnDelete::Restrict
        ),
        Err(Error::Conflict(_))
    ));
    assert_eq!(db.table("children").unwrap(), before);
    db.delete_row("children", &[i(11)]).unwrap();
    fk(
        &db,
        "children",
        "fk",
        &["parent_id"],
        "parents",
        OnDelete::Restrict,
    )
    .unwrap();
    assert!(
        matches!(db.delete_row("parents", &[i(1)]), Err(Error::Conflict(_))),
        "the index built from existing rows is used"
    );
    clean(&db);
    drop_constraint(&db, "children", "fk").unwrap();
    db.delete_row("parents", &[i(1)]).unwrap();
    clean(&db);
}

#[test]
fn a_table_that_others_refer_to_cannot_be_dropped_until_the_key_is() {
    let d = Dir::new();
    let db = open(&d);
    parent_child(&db, OnDelete::Cascade);
    ins(&db, "parents", &[("id", i(1))]).unwrap();
    ins(&db, "children", &[("id", i(10)), ("parent_id", i(1))]).unwrap();
    assert!(matches!(db.drop_table("parents"), Err(Error::Conflict(_))));
    // dropping the child table removes its rows and its index entries, then the parent can go
    assert_eq!(db.drop_table("children").unwrap(), 1);
    clean(&db);
    assert_eq!(db.drop_table("parents").unwrap(), 1);
    clean(&db);
}

// ---- checking ----

#[test]
fn check_finds_damaged_index_entries() {
    let d = Dir::new();
    let db = open(&d);
    make(
        &db,
        "p",
        &[("id", Type::I64, false), ("code", Type::Text, true)],
        &["id"],
    );
    unique(&db, "p", "u", &["code"]).unwrap();
    ins(&db, "p", &[("id", i(1)), ("code", t("a"))]).unwrap();
    ins(&db, "p", &[("id", i(2)), ("code", t("b"))]).unwrap();
    clean(&db);
    let idx = db.keyspace("indexes").unwrap();
    let first = idx.iter().next().unwrap().key().unwrap().to_vec();
    idx.remove(first).unwrap(); // a row loses its index entry
    idx.insert(vec![0, 0, 0, 9, 0, 0, 0, 9, 1], vec![1])
        .unwrap(); // an entry with no row
    let all = db.verify(false).unwrap().problems.join("\n");
    assert!(all.contains("missing"), "{all}");
    assert!(all.contains("belongs to no row"), "{all}");
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

/// A child row: (parent, score, tag).
type ChildRow = (Option<i64>, Option<i32>, Option<String>);

#[derive(Clone, Default, PartialEq, Debug)]
struct Model {
    p: BTreeMap<i64, Option<String>>, // id -> code (unique)
    c: BTreeMap<i64, ChildRow>,       // id -> (parent, score >= 0, tag unique)
    g: BTreeMap<i64, Option<i64>>,    // id -> child
    n: BTreeMap<i64, Option<i64>>,    // id -> parent (tree, cascade)
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Out {
    Ok,
    Conflict,
    Invalid,
    Missing,
}

fn kind(r: Result<impl Sized, Error>) -> Out {
    match r {
        Ok(_) => Out::Ok,
        Err(Error::Conflict(_)) => Out::Conflict,
        Err(Error::Invalid(_)) => Out::Invalid,
        Err(Error::NotFound(_)) => Out::Missing,
        Err(e) => panic!("{e}"),
    }
}

impl Model {
    /// Deletes by recursion, applying each step at once; the caller restores a copy if it fails. (The engine plans the whole delete first, so this is
    /// a different way to reach the same answer.)
    fn del_p(&mut self, id: i64, mode_c: OnDelete, mode_g: OnDelete) -> Result<(), ()> {
        self.p.remove(&id);
        let kids: Vec<i64> = self
            .c
            .iter()
            .filter(|(_, v)| v.0 == Some(id))
            .map(|(k, _)| *k)
            .collect();
        for k in kids {
            match mode_c {
                OnDelete::Restrict => return Err(()),
                OnDelete::Cascade => self.del_c(k, mode_g)?,
                OnDelete::SetNull => self.c.get_mut(&k).unwrap().0 = None,
            }
        }
        Ok(())
    }
    fn del_c(&mut self, id: i64, mode_g: OnDelete) -> Result<(), ()> {
        self.c.remove(&id);
        let kids: Vec<i64> = self
            .g
            .iter()
            .filter(|(_, v)| **v == Some(id))
            .map(|(k, _)| *k)
            .collect();
        for k in kids {
            match mode_g {
                OnDelete::Restrict => return Err(()),
                OnDelete::Cascade => {
                    self.g.remove(&k);
                }
                OnDelete::SetNull => {
                    self.g.insert(k, None);
                }
            }
        }
        Ok(())
    }
    fn del_n(&mut self, id: i64) {
        self.n.remove(&id);
        let kids: Vec<i64> = self
            .n
            .iter()
            .filter(|(_, v)| **v == Some(id))
            .map(|(k, _)| *k)
            .collect();
        for k in kids {
            self.del_n(k);
        }
    }
}

fn setup(db: &Db, mode_c: OnDelete, mode_g: OnDelete) {
    make(
        db,
        "p",
        &[("id", Type::I64, false), ("code", Type::Text, true)],
        &["id"],
    );
    unique(db, "p", "p_code", &["code"]).unwrap();
    make(
        db,
        "c",
        &[
            ("id", Type::I64, false),
            ("pid", Type::I64, true),
            ("score", Type::I32, true),
            ("tag", Type::Text, true),
        ],
        &["id"],
    );
    unique(db, "c", "c_tag", &["tag"]).unwrap();
    check(db, "c", "c_score", "score == null || score >= 0").unwrap();
    fk(db, "c", "c_p", &["pid"], "p", mode_c).unwrap();
    make(
        db,
        "g",
        &[("id", Type::I64, false), ("cid", Type::I64, true)],
        &["id"],
    );
    fk(db, "g", "g_c", &["cid"], "c", mode_g).unwrap();
    make(
        db,
        "n",
        &[("id", Type::I64, false), ("parent", Type::I64, true)],
        &["id"],
    );
    fk(db, "n", "n_self", &["parent"], "n", OnDelete::Cascade).unwrap();
}

fn read_all(db: &Db, table: &str) -> Vec<Row> {
    let mut out = Vec::new();
    let mut after: Option<Vec<Value>> = None;
    loop {
        let page = db.scan_rows(table, after.as_deref(), 50).unwrap();
        if page.is_empty() {
            return out;
        }
        after = Some(vec![get(page.last().unwrap(), "id").clone().unwrap()]);
        out.extend(page);
    }
}

fn int(r: &Row, name: &str) -> Option<i64> {
    match get(r, name) {
        Some(Value::I64(n)) => Some(*n),
        Some(Value::I32(n)) => Some(*n as i64),
        None => None,
        other => panic!("{other:?}"),
    }
}

fn text(r: &Row, name: &str) -> Option<String> {
    match get(r, name) {
        Some(Value::Text(s)) => Some(s.clone()),
        None => None,
        other => panic!("{other:?}"),
    }
}

fn agree(db: &Db, m: &Model, ctx: &str) {
    let p: BTreeMap<i64, Option<String>> = read_all(db, "p")
        .iter()
        .map(|r| (int(r, "id").unwrap(), text(r, "code")))
        .collect();
    assert_eq!(p, m.p, "{ctx}: table p");
    let c: BTreeMap<i64, ChildRow> = read_all(db, "c")
        .iter()
        .map(|r| {
            (
                int(r, "id").unwrap(),
                (
                    int(r, "pid"),
                    int(r, "score").map(|s| s as i32),
                    text(r, "tag"),
                ),
            )
        })
        .collect();
    assert_eq!(c, m.c, "{ctx}: table c");
    let g: BTreeMap<i64, Option<i64>> = read_all(db, "g")
        .iter()
        .map(|r| (int(r, "id").unwrap(), int(r, "cid")))
        .collect();
    assert_eq!(g, m.g, "{ctx}: table g");
    let n: BTreeMap<i64, Option<i64>> = read_all(db, "n")
        .iter()
        .map(|r| (int(r, "id").unwrap(), int(r, "parent")))
        .collect();
    assert_eq!(n, m.n, "{ctx}: table n");
}

fn run(seed: u64, steps: usize) {
    let d = Dir::new();
    let mut r = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
    let modes = [OnDelete::Restrict, OnDelete::Cascade, OnDelete::SetNull];
    let (mode_c, mode_g) = (modes[r.below(3)], modes[r.below(3)]);
    let mut db = Some(open(&d));
    setup(db.as_ref().unwrap(), mode_c, mode_g);
    let mut m = Model::default();
    let id = |r: &mut Rng| r.below(12) as i64;
    let opt = |r: &mut Rng, hi: usize| -> Option<i64> {
        if r.below(4) == 0 {
            None
        } else {
            Some(r.below(hi) as i64)
        }
    };
    let word = |r: &mut Rng| -> Option<String> {
        if r.below(3) == 0 {
            None
        } else {
            Some(format!("w{}", r.below(6)))
        }
    };
    for step in 0..steps {
        let dbr = db.as_ref().unwrap();
        let ctx = format!("seed {seed} (c: {mode_c:?}, g: {mode_g:?}) step {step}");
        match r.below(100) {
            // insert p
            0..=9 => {
                let (k, code) = (id(&mut r), word(&mut r));
                let mut v = vec![("id", i(k))];
                if let Some(c) = &code {
                    v.push(("code", t(c)));
                }
                let want = if m.p.contains_key(&k)
                    || (code.is_some() && m.p.values().any(|x| *x == code))
                {
                    Out::Conflict
                } else {
                    Out::Ok
                };
                assert_eq!(
                    kind(ins(dbr, "p", &v)),
                    want,
                    "{ctx}: insert p {k} {code:?}"
                );
                if want == Out::Ok {
                    m.p.insert(k, code);
                }
            }
            // insert c
            10..=24 => {
                let (k, pid, tag) = (id(&mut r), opt(&mut r, 12), word(&mut r));
                let score = if r.below(3) == 0 {
                    None
                } else {
                    Some(r.below(20) as i32 - 5)
                };
                let mut v = vec![("id", i(k))];
                if let Some(p) = pid {
                    v.push(("pid", i(p)));
                }
                if let Some(s) = score {
                    v.push(("score", Value::I32(s)));
                }
                if let Some(tg) = &tag {
                    v.push(("tag", t(tg)));
                }
                // the engine checks, in order: the CHECK, the foreign key, the primary key, then unique
                let want = if score.is_some_and(|s| s < 0) {
                    Out::Invalid
                } else if pid.is_some_and(|p| !m.p.contains_key(&p))
                    || m.c.contains_key(&k)
                    || (tag.is_some() && m.c.values().any(|x| x.2 == tag))
                {
                    Out::Conflict
                } else {
                    Out::Ok
                };
                assert_eq!(
                    kind(ins(dbr, "c", &v)),
                    want,
                    "{ctx}: insert c {k} {pid:?} {score:?} {tag:?}"
                );
                if want == Out::Ok {
                    m.c.insert(k, (pid, score, tag));
                }
            }
            // insert g
            25..=32 => {
                let (k, cid) = (id(&mut r), opt(&mut r, 12));
                let mut v = vec![("id", i(k))];
                if let Some(c) = cid {
                    v.push(("cid", i(c)));
                }
                let want = if cid.is_some_and(|c| !m.c.contains_key(&c)) || m.g.contains_key(&k) {
                    Out::Conflict
                } else {
                    Out::Ok
                };
                assert_eq!(kind(ins(dbr, "g", &v)), want, "{ctx}: insert g {k} {cid:?}");
                if want == Out::Ok {
                    m.g.insert(k, cid);
                }
            }
            // insert n
            33..=40 => {
                let (k, par) = (id(&mut r), opt(&mut r, 12));
                let mut v = vec![("id", i(k))];
                if let Some(p) = par {
                    v.push(("parent", i(p)));
                }
                let want = if par.is_some_and(|p| !m.n.contains_key(&p)) || m.n.contains_key(&k) {
                    Out::Conflict
                } else {
                    Out::Ok
                };
                assert_eq!(kind(ins(dbr, "n", &v)), want, "{ctx}: insert n {k} {par:?}");
                if want == Out::Ok {
                    m.n.insert(k, par);
                }
            }
            // update c: one column
            41..=55 => {
                let k = id(&mut r);
                let (col, newv): (&str, Option<Value>) = match r.below(3) {
                    0 => ("pid", opt(&mut r, 12).map(i)),
                    1 => (
                        "score",
                        if r.below(3) == 0 {
                            None
                        } else {
                            Some(Value::I32(r.below(20) as i32 - 5))
                        },
                    ),
                    _ => ("tag", word(&mut r).map(|w| t(&w))),
                };
                let want = match m.c.get(&k) {
                    None => Out::Missing,
                    Some(cur) => {
                        let mut new = cur.clone();
                        match (col, &newv) {
                            ("pid", v) => {
                                new.0 = v.as_ref().map(|x| {
                                    if let Value::I64(n) = x {
                                        *n
                                    } else {
                                        unreachable!()
                                    }
                                })
                            }
                            ("score", v) => {
                                new.1 = v.as_ref().map(|x| {
                                    if let Value::I32(n) = x {
                                        *n
                                    } else {
                                        unreachable!()
                                    }
                                })
                            }
                            (_, v) => {
                                new.2 = v.as_ref().map(|x| {
                                    if let Value::Text(s) = x {
                                        s.clone()
                                    } else {
                                        unreachable!()
                                    }
                                })
                            }
                        }
                        if new.1.is_some_and(|s| s < 0) {
                            Out::Invalid
                        } else if new.0.is_some_and(|p| !m.p.contains_key(&p))
                            || (new.2.is_some()
                                && m.c.iter().any(|(ok, ov)| *ok != k && ov.2 == new.2))
                        {
                            Out::Conflict
                        } else {
                            Out::Ok
                        }
                    }
                };
                assert_eq!(
                    kind(dbr.update_row("c", &[i(k)], &[(col, newv.clone())])),
                    want,
                    "{ctx}: update c {k} {col} {newv:?}"
                );
                if want == Out::Ok {
                    let e = m.c.get_mut(&k).unwrap();
                    match col {
                        "pid" => {
                            e.0 = newv.map(|x| {
                                if let Value::I64(n) = x {
                                    n
                                } else {
                                    unreachable!()
                                }
                            })
                        }
                        "score" => {
                            e.1 = newv.map(|x| {
                                if let Value::I32(n) = x {
                                    n
                                } else {
                                    unreachable!()
                                }
                            })
                        }
                        _ => {
                            e.2 = newv.map(|x| {
                                if let Value::Text(s) = x {
                                    s
                                } else {
                                    unreachable!()
                                }
                            })
                        }
                    }
                }
            }
            // delete p
            56..=65 => {
                let k = id(&mut r);
                let got = dbr.delete_row("p", &[i(k)]);
                if !m.p.contains_key(&k) {
                    assert!(matches!(got, Ok(false)), "{ctx}: delete of a missing p");
                } else {
                    let mut trial = m.clone();
                    match trial.del_p(k, mode_c, mode_g) {
                        Ok(()) => {
                            assert!(matches!(got, Ok(true)), "{ctx}: delete p {k}: {got:?}");
                            m = trial;
                        }
                        Err(()) => assert!(
                            matches!(got, Err(Error::Conflict(_))),
                            "{ctx}: delete p {k} should be refused: {got:?}"
                        ),
                    }
                }
            }
            // delete c
            66..=75 => {
                let k = id(&mut r);
                let got = dbr.delete_row("c", &[i(k)]);
                if !m.c.contains_key(&k) {
                    assert!(matches!(got, Ok(false)), "{ctx}");
                } else {
                    let mut trial = m.clone();
                    match trial.del_c(k, mode_g) {
                        Ok(()) => {
                            assert!(matches!(got, Ok(true)), "{ctx}: delete c {k}: {got:?}");
                            m = trial;
                        }
                        Err(()) => assert!(
                            matches!(got, Err(Error::Conflict(_))),
                            "{ctx}: delete c {k} should be refused: {got:?}"
                        ),
                    }
                }
            }
            // delete g, n
            76..=80 => {
                let k = id(&mut r);
                assert_eq!(
                    dbr.delete_row("g", &[i(k)]).unwrap(),
                    m.g.remove(&k).is_some(),
                    "{ctx}"
                );
            }
            81..=88 => {
                let k = id(&mut r);
                let had = m.n.contains_key(&k);
                m.del_n(k);
                assert_eq!(
                    dbr.delete_row("n", &[i(k)]).unwrap(),
                    had,
                    "{ctx}: delete n {k}"
                );
            }
            89..=92 => {
                drop(db.take());
                db = Some(open(&d));
            }
            _ => {}
        }
        let dbr = db.as_ref().unwrap();
        agree(dbr, &m, &ctx);
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
fn random_operations_across_linked_tables_agree_with_the_model() {
    let n: u64 = std::env::var("POWERDB_MODEL_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(27);
    for seed in 1..=n {
        run(seed, 200);
    }
}
