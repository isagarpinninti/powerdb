//! Tables, columns and schema changes: the individual rules, and a model test that rows written under old schema versions stay readable, without
//! being rewritten, after any sequence of changes.

use powerdb::Error;
use powerdb::types::row::{decode_row, encode_row};
use powerdb::types::schema::{Change, Effect, TableDef, widen_value};
use powerdb::types::{Type, Value};
use std::collections::BTreeMap;

fn add(name: &str, ty: Type, nullable: bool, default: Option<Value>) -> Change {
    Change::AddColumn {
        name: name.into(),
        ty,
        nullable,
        default,
    }
}

fn table_with(cols: &[(&str, Type, bool, Option<Value>)]) -> TableDef {
    let mut t = TableDef::new("people").unwrap();
    for (n, ty, nullable, d) in cols {
        t = t
            .apply(&add(n, ty.clone(), *nullable, d.clone()), true)
            .unwrap()
            .0;
    }
    t
}

fn get<'a>(row: &'a [(String, Option<Value>)], name: &str) -> &'a Option<Value> {
    &row.iter().find(|(n, _)| n == name).unwrap().1
}

#[test]
fn names_are_checked_and_a_table_starts_at_version_one() {
    for bad in ["", "Upper", "1abc", "has space", "a-b", &"x".repeat(64)] {
        assert!(TableDef::new(bad).is_err(), "{bad:?}");
    }
    let t = TableDef::new("people").unwrap();
    assert_eq!((t.version, t.columns.len(), t.next_column_id), (1, 0, 1));
    for bad in ["", "Upper", "a-b", "id "] {
        assert!(
            matches!(
                t.apply(&add(bad, Type::I32, true, None), true),
                Err(Error::Invalid(_))
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn every_change_raises_the_version_by_one() {
    let mut t = TableDef::new("t").unwrap();
    for (i, c) in [
        add("a", Type::I32, true, None),
        add("b", Type::Text, true, None),
        Change::RenameColumn {
            from: "a".into(),
            to: "c".into(),
        },
        Change::WidenColumn {
            name: "c".into(),
            to: Type::I64,
        },
        Change::DropColumn { name: "b".into() },
    ]
    .iter()
    .enumerate()
    {
        t = t.apply(c, true).unwrap().0;
        assert_eq!(t.version, i as u32 + 2);
    }
}

#[test]
fn adding_a_column_to_a_table_with_rows() {
    let t = table_with(&[("name", Type::Text, false, None)]);
    // nullable, no default: old rows read as null
    let (t1, e) = t
        .apply(&add("nickname", Type::Text, true, None), false)
        .unwrap();
    assert_eq!(e, Effect::CatalogOnly);
    // NOT NULL with no default cannot work: the existing rows have nothing to show
    let err = t
        .apply(&add("age", Type::I32, false, None), false)
        .unwrap_err();
    assert!(
        matches!(err, Error::Invalid(_)) && err.to_string().contains("default"),
        "{err}"
    );
    // ... but is fine on an empty table, and with a default on a full one
    assert!(t.apply(&add("age", Type::I32, false, None), true).is_ok());
    let (t2, _) = t
        .apply(&add("age", Type::I32, false, Some(Value::I32(18))), false)
        .unwrap();
    // the old row (written before either column existed) reads with the defaults
    let old = encode_row(&t.build_row(&[("name", Value::Text("Ann".into()))]).unwrap()).unwrap();
    let row = t2.project(&decode_row(&old).unwrap()).unwrap();
    assert_eq!(get(&row, "age"), &Some(Value::I32(18)));
    let row = t1.project(&decode_row(&old).unwrap()).unwrap();
    assert_eq!(get(&row, "nickname"), &None);
    // a default of the wrong type, and a duplicate name, are refused
    assert!(matches!(
        t.apply(
            &add("x", Type::I32, true, Some(Value::Text("no".into()))),
            false
        ),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        t.apply(&add("name", Type::Text, true, None), false),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn a_dropped_column_is_ignored_and_its_id_is_never_given_to_another() {
    let t = table_with(&[("a", Type::Text, true, None), ("b", Type::I32, true, None)]);
    let stored = encode_row(
        &t.build_row(&[("a", Value::Text("secret".into())), ("b", Value::I32(7))])
            .unwrap(),
    )
    .unwrap();
    let (t2, e) = t
        .apply(&Change::DropColumn { name: "a".into() }, false)
        .unwrap();
    assert_eq!(e, Effect::CatalogOnly);
    assert!(t2.column("a").is_none());
    // a column added afterwards must not pick up the dropped column's old values
    let (t3, _) = t2.apply(&add("c", Type::Text, true, None), false).unwrap();
    assert!(
        t3.columns.iter().all(|c| c.id != t.column("a").unwrap().id),
        "the id of a dropped column was reused"
    );
    let row = t3.project(&decode_row(&stored).unwrap()).unwrap();
    assert_eq!(
        get(&row, "c"),
        &None,
        "the new column read the old column's value"
    );
    assert_eq!(get(&row, "b"), &Some(Value::I32(7)));
    assert!(matches!(
        t.apply(&Change::DropColumn { name: "zz".into() }, false),
        Err(Error::NotFound(_))
    ));
    // the same when the column dropped is the one with the highest id (the case a "highest id plus one" scheme would get wrong)
    let (t4, _) = t
        .apply(&Change::DropColumn { name: "b".into() }, false)
        .unwrap();
    let (t5, _) = t4.apply(&add("d", Type::I32, true, None), false).unwrap();
    assert_ne!(
        t5.column("d").unwrap().id,
        t.column("b").unwrap().id,
        "a new column was given the id of the highest column that was dropped"
    );
    let row = t5.project(&decode_row(&stored).unwrap()).unwrap();
    assert_eq!(
        get(&row, "d"),
        &None,
        "the new column read a dropped column's value"
    );
}

#[test]
fn renaming_follows_the_data() {
    let t = table_with(&[("email", Type::Text, true, None)]);
    let stored = encode_row(
        &t.build_row(&[("email", Value::Text("a@b.c".into()))])
            .unwrap(),
    )
    .unwrap();
    let (t2, e) = t
        .apply(
            &Change::RenameColumn {
                from: "email".into(),
                to: "mail".into(),
            },
            false,
        )
        .unwrap();
    assert_eq!(e, Effect::CatalogOnly);
    let row = t2.project(&decode_row(&stored).unwrap()).unwrap();
    assert_eq!(get(&row, "mail"), &Some(Value::Text("a@b.c".into())));
    assert!(t2.column("email").is_none());
    // onto an existing name is refused; onto itself is a no-op
    let t3 = t2
        .apply(&add("other", Type::Text, true, None), false)
        .unwrap()
        .0;
    assert!(matches!(
        t3.apply(
            &Change::RenameColumn {
                from: "mail".into(),
                to: "other".into()
            },
            false
        ),
        Err(Error::Conflict(_))
    ));
    assert!(
        t3.apply(
            &Change::RenameColumn {
                from: "mail".into(),
                to: "mail".into()
            },
            false
        )
        .is_ok()
    );
}

#[test]
fn widening_reads_old_values_as_the_wider_type_and_narrowing_is_refused() {
    let t = table_with(&[
        ("n", Type::I32, false, Some(Value::I32(5))),
        ("f", Type::F32, true, None),
        ("u", Type::U8, true, None),
    ]);
    let stored = encode_row(
        &t.build_row(&[
            ("n", Value::I32(-9)),
            ("f", Value::F32(1.5)),
            ("u", Value::U8(200)),
        ])
        .unwrap(),
    )
    .unwrap();
    let widen = |t: &TableDef, name: &str, to: Type| {
        t.apply(
            &Change::WidenColumn {
                name: name.into(),
                to,
            },
            false,
        )
    };
    let (t2, e) = widen(&t, "n", Type::I64).unwrap();
    assert_eq!(e, Effect::CatalogOnly);
    let t2 = widen(&t2, "f", Type::F64).unwrap().0;
    let t2 = widen(&t2, "u", Type::U64).unwrap().0;
    let row = t2.project(&decode_row(&stored).unwrap()).unwrap();
    assert_eq!(get(&row, "n"), &Some(Value::I64(-9)));
    assert_eq!(get(&row, "f"), &Some(Value::F64(1.5)));
    assert_eq!(get(&row, "u"), &Some(Value::U64(200)));
    assert_eq!(
        t2.column("n").unwrap().default,
        Some(Value::I64(5)),
        "the default is widened with the column"
    );
    for (name, to) in [
        ("n", Type::I32),
        ("n", Type::U64),
        ("n", Type::F64),
        ("u", Type::I64),
        ("f", Type::F32),
    ] {
        assert!(
            widen(&t2, name, to.clone()).is_err(),
            "{name} to {to:?} should be refused"
        );
    }
    // a value that cannot be widened to the column's type is damage, not a silent conversion
    assert!(widen_value(Value::Text("x".into()), &Type::I64).is_err());
    assert!(widen_value(Value::I64(1), &Type::I32).is_err());
}

#[test]
fn not_null_and_nullable() {
    let t = table_with(&[("x", Type::I32, true, None)]);
    let (_, e) = t
        .apply(&Change::SetNotNull { name: "x".into() }, false)
        .unwrap();
    assert!(
        matches!(e, Effect::NeedsScan(_)),
        "existing rows must be checked first"
    );
    let (t2, e) = t
        .apply(&Change::SetNotNull { name: "x".into() }, true)
        .unwrap();
    assert_eq!(e, Effect::CatalogOnly);
    // reading a row that lacks a NOT NULL value is reported as damage
    let empty = encode_row(&[]).unwrap();
    assert!(matches!(
        t2.project(&decode_row(&empty).unwrap()),
        Err(Error::Corrupt(_))
    ));
    let (t3, e) = t2
        .apply(&Change::SetNullable { name: "x".into() }, false)
        .unwrap();
    assert_eq!(e, Effect::CatalogOnly);
    assert!(t3.project(&[]).is_ok());
}

#[test]
fn changing_a_type_outright_needs_an_empty_table() {
    let t = table_with(&[("x", Type::Text, true, None)]);
    let err = t
        .apply(
            &Change::ChangeType {
                name: "x".into(),
                to: Type::I32,
            },
            false,
        )
        .unwrap_err();
    assert!(err.to_string().contains("no rows"), "{err}");
    let (t2, _) = t
        .apply(
            &Change::ChangeType {
                name: "x".into(),
                to: Type::I32,
            },
            true,
        )
        .unwrap();
    assert_eq!(t2.column("x").unwrap().ty, Type::I32);
    // a widening through ChangeType is allowed on a full table too
    let t = table_with(&[("n", Type::I8, true, None)]);
    assert!(
        t.apply(
            &Change::ChangeType {
                name: "n".into(),
                to: Type::I64
            },
            false
        )
        .is_ok()
    );
}

#[test]
fn a_refused_change_leaves_the_table_exactly_as_it_was() {
    let t = table_with(&[("a", Type::I32, true, None)]);
    let before = t.clone();
    let _ = t.apply(&add("a", Type::I32, true, None), false);
    let _ = t.apply(
        &Change::WidenColumn {
            name: "a".into(),
            to: Type::Text,
        },
        false,
    );
    let _ = t.apply(
        &Change::DropColumn {
            name: "nope".into(),
        },
        false,
    );
    assert_eq!(t, before);
}

#[test]
fn building_a_row_applies_types_defaults_and_not_null() {
    let t = table_with(&[
        ("name", Type::Text, false, None),
        ("age", Type::I32, false, Some(Value::I32(18))),
        ("note", Type::Text, true, None),
    ]);
    let row = t.build_row(&[("name", Value::Text("Ann".into()))]).unwrap();
    assert_eq!(
        row.len(),
        2,
        "the default is written, the null is left out: {row:?}"
    );
    assert!(
        t.build_row(&[("name", Value::I32(1))]).is_err(),
        "wrong type"
    );
    assert!(
        t.build_row(&[("age", Value::I32(1))]).is_err(),
        "NOT NULL with no default"
    );
    assert!(
        t.build_row(&[("name", Value::Text("x".into())), ("zip", Value::I32(1))])
            .is_err(),
        "unknown column"
    );
    let ids: Vec<u32> = row.iter().map(|c| c.0).collect();
    assert_eq!(
        ids,
        vec![t.column("name").unwrap().id, t.column("age").unwrap().id]
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
        Type::U8,
        Type::U32,
        Type::F32,
        Type::F64,
        Type::Text,
        Type::Bool,
        Type::TimestampTz,
    ][r.below(11)]
    .clone()
}

fn random_value(r: &mut Rng, t: &Type) -> Value {
    let n = r.next();
    match t {
        Type::I8 => Value::I8(n as i8),
        Type::I16 => Value::I16(n as i16),
        Type::I32 => Value::I32(n as i32),
        Type::I64 => Value::I64(n as i64),
        Type::U8 => Value::U8(n as u8),
        Type::U32 => Value::U32(n as u32),
        Type::F32 => Value::F32((n % 1000) as f32 / 4.0),
        Type::F64 => Value::F64((n % 1000) as f64 / 4.0),
        Type::Text => Value::Text(format!("t{}", n % 100)),
        Type::Bool => Value::Bool(n.is_multiple_of(2)),
        Type::TimestampTz => Value::TimestampTz(n as i64 >> 8),
        _ => unreachable!(),
    }
}

type ModelRow = BTreeMap<String, Option<Value>>;

fn run(seed: u64, steps: usize) {
    let mut r = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
    let mut def = TableDef::new("t").unwrap();
    // every row as it was written (bytes, never rewritten) beside what the model says it holds now
    let mut stored: Vec<(Vec<u8>, ModelRow)> = Vec::new();
    for step in 0..steps {
        let names: Vec<String> = def.columns.iter().map(|c| c.name.clone()).collect();
        match r.below(10) {
            // insert a row under the current schema
            0..=3 => {
                let mut values: Vec<(String, Value)> = Vec::new();
                for c in def.columns.clone() {
                    if !c.nullable || r.below(3) > 0 {
                        values.push((c.name.clone(), random_value(&mut r, &c.ty)));
                    }
                }
                let refs: Vec<(&str, Value)> = values
                    .iter()
                    .map(|(n, v)| (n.as_str(), v.clone()))
                    .collect();
                let cols = def
                    .build_row(&refs)
                    .unwrap_or_else(|e| panic!("seed {seed} step {step}: {e}"));
                let mut model = ModelRow::new();
                for c in &def.columns {
                    let v = cols
                        .iter()
                        .find(|(id, _)| *id == c.id)
                        .map(|(_, v)| v.clone());
                    model.insert(c.name.clone(), v);
                }
                stored.push((encode_row(&cols).unwrap(), model));
            }
            4 | 5 => {
                let name = format!("c{}", r.below(8));
                let ty = random_type(&mut r);
                let nullable = r.below(2) == 0;
                let default = if r.below(2) == 0 {
                    Some(random_value(&mut r, &ty))
                } else {
                    None
                };
                let empty = stored.is_empty();
                let should_fail =
                    names.contains(&name) || (!nullable && default.is_none() && !empty);
                match def.apply(&add(&name, ty, nullable, default.clone()), empty) {
                    Ok((next, e)) => {
                        assert!(
                            !should_fail,
                            "seed {seed} step {step}: a change that must be refused was accepted"
                        );
                        assert_eq!(e, Effect::CatalogOnly);
                        def = next;
                        for (_, m) in &mut stored {
                            m.insert(name.clone(), default.clone());
                        }
                    }
                    Err(_) => assert!(
                        should_fail,
                        "seed {seed} step {step}: a valid change was refused"
                    ),
                }
            }
            6 if !names.is_empty() => {
                let name = names[r.below(names.len())].clone();
                def = def
                    .apply(
                        &Change::DropColumn { name: name.clone() },
                        stored.is_empty(),
                    )
                    .unwrap()
                    .0;
                for (_, m) in &mut stored {
                    m.remove(&name);
                }
            }
            7 if !names.is_empty() => {
                let from = names[r.below(names.len())].clone();
                let to = format!("c{}", r.below(8));
                let clash = to != from && names.contains(&to);
                match def.apply(
                    &Change::RenameColumn {
                        from: from.clone(),
                        to: to.clone(),
                    },
                    stored.is_empty(),
                ) {
                    Ok((next, _)) => {
                        assert!(!clash);
                        def = next;
                        if from != to {
                            for (_, m) in &mut stored {
                                let v = m.remove(&from).unwrap();
                                m.insert(to.clone(), v);
                            }
                        }
                    }
                    Err(_) => assert!(clash),
                }
            }
            8 if !names.is_empty() => {
                let c = def.columns[r.below(def.columns.len())].clone();
                let to = random_type(&mut r);
                if let Ok((next, _)) = def.apply(
                    &Change::WidenColumn {
                        name: c.name.clone(),
                        to: to.clone(),
                    },
                    stored.is_empty(),
                ) {
                    def = next;
                    for (_, m) in &mut stored {
                        if let Some(Some(v)) = m.get(&c.name).cloned() {
                            m.insert(c.name.clone(), Some(widen_value(v, &to).unwrap()));
                        }
                    }
                }
            }
            _ if !names.is_empty() => {
                // NOT NULL on a column: the caller does the scan the change asks for, and refuses if a row has no value
                let name = names[r.below(names.len())].clone();
                if r.below(2) == 0 {
                    def = def
                        .apply(&Change::SetNullable { name }, stored.is_empty())
                        .unwrap()
                        .0;
                } else {
                    let (next, e) = def
                        .apply(
                            &Change::SetNotNull { name: name.clone() },
                            stored.is_empty(),
                        )
                        .unwrap();
                    let ok = match e {
                        Effect::CatalogOnly => true,
                        Effect::NeedsScan(_) => stored
                            .iter()
                            .all(|(_, m)| matches!(m.get(&name), Some(Some(_)))),
                    };
                    if ok {
                        def = next;
                    }
                }
            }
            _ => {}
        }
        // after every step: every row ever written still reads, as the model says, under the current schema
        for (i, (bytes, model)) in stored.iter().enumerate() {
            let row = def
                .project(&decode_row(bytes).unwrap())
                .unwrap_or_else(|e| {
                    panic!("seed {seed} step {step}: row {i} no longer reads: {e}")
                });
            let got: ModelRow = row.into_iter().collect();
            assert_eq!(
                got.len(),
                model.len(),
                "seed {seed} step {step}: row {i}: columns {:?} vs {:?}",
                got.keys().collect::<Vec<_>>(),
                model.keys().collect::<Vec<_>>()
            );
            for (k, mv) in model {
                let gv = got
                    .get(k)
                    .unwrap_or_else(|| panic!("seed {seed} step {step}: row {i} lacks {k}"));
                let same = match (gv, mv) {
                    (Some(a), Some(b)) => a.same(b),
                    (None, None) => true,
                    _ => false,
                };
                assert!(
                    same,
                    "seed {seed} step {step}: row {i} column {k}: read {gv:?}, the model says {mv:?}"
                );
            }
        }
    }
}

#[test]
fn rows_written_under_old_schemas_stay_readable_through_any_sequence_of_changes() {
    let n: u64 = std::env::var("POWERDB_SCHEMA_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    for seed in 1..=n {
        run(seed, 120);
    }
}
