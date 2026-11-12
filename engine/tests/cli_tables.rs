//! `powerdb table ...` against a real server, driven through the real binary.

use powerdb::util::new_id;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_powerdb");

struct Server {
    child: Child,
    dir: PathBuf,
    url: String,
    key: String,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start() -> Server {
    let dir = std::env::temp_dir().join(format!("powerdb-clitab-{}", new_id()));
    let out = Command::new(BIN)
        .args(["init", dir.to_str().unwrap()])
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let key = text
        .split_whitespace()
        .find(|w| w.starts_with("pdb_live_"))
        .expect("init prints an administrator key")
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .to_string();
    let port = free_port();
    let child = Command::new(BIN)
        .args([
            "serve",
            "--data",
            dir.to_str().unwrap(),
            "--addr",
            &format!("127.0.0.1:{port}"),
            "--durability",
            "never",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Server {
        child,
        dir,
        url,
        key,
    }
}

struct Out {
    ok: bool,
    stdout: Vec<u8>,
    stderr: String,
}

fn run(s: &Server, args: &[&str], key: Option<&str>) -> Out {
    let mut c = Command::new(BIN);
    c.args(args).args(["--url", &s.url]);
    match key {
        Some(k) => {
            c.args(["--key", k]);
        }
        None => {
            c.env_remove("POWERDB_KEY");
        }
    }
    let o = c.output().unwrap();
    Out {
        ok: o.status.success(),
        stdout: o.stdout,
        stderr: String::from_utf8_lossy(&o.stderr).to_string(),
    }
}

fn ok(s: &Server, args: &[&str]) -> String {
    let o = run(s, args, Some(&s.key));
    assert!(o.ok, "powerdb {}: {}", args.join(" "), o.stderr);
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn the_cli_creates_tables_and_manages_rows() {
    let s = start();
    let spec = r#"{"name":"people","columns":[{"name":"id","type":"int64"},{"name":"name","type":"text","nullable":false}],"primary_key":["id"]}"#;
    assert!(ok(&s, &["table", "create", spec]).contains("Created table people"));
    assert!(ok(&s, &["table", "ls"]).contains("people"));
    assert!(ok(&s, &["table", "show", "people"]).contains("\"rows\": 0"));
    for i in 1..=5 {
        ok(
            &s,
            &[
                "table",
                "insert",
                "people",
                &format!(r#"{{"id":{i},"name":"p{i}"}}"#),
            ],
        );
    }
    let rows = ok(&s, &["table", "rows", "people", "--limit", "2"]);
    assert_eq!(rows.lines().count(), 5, "{rows}");
    let some = ok(&s, &["table", "rows", "people", "--max", "3"]);
    assert_eq!(some.lines().count(), 3);
    assert!(ok(&s, &["table", "get", "people", "3"]).contains("p3"));
    assert!(
        ok(
            &s,
            &["table", "update", "people", "3", r#"{"name":"three"}"#]
        )
        .contains("three")
    );
    ok(
        &s,
        &[
            "table",
            "alter",
            "people",
            r#"{"op":"add_column","name":"nick","type":"text"}"#,
        ],
    );
    // a spec from a file
    let f = s.dir.join("spec.json");
    std::fs::write(&f, r#"{"name":"pairs","columns":[{"name":"a","type":"text","nullable":false},{"name":"b","type":"int32","nullable":false}],"primary_key":["a","b"]}"#).unwrap();
    ok(&s, &["table", "create", &format!("@{}", f.display())]);
    ok(&s, &["table", "insert", "pairs", r#"{"a":"x","b":1}"#]);
    assert!(ok(&s, &["table", "get", "pairs", r#"["x",1]"#]).contains("\"a\": \"x\""));
    ok(&s, &["table", "delete", "pairs", r#"["x",1]"#]);
    // errors are readable and fail the command
    let o = run(&s, &["table", "get", "people", "99"], Some(&s.key));
    assert!(!o.ok && o.stderr.contains("404"), "{}", o.stderr);
    let o = run(
        &s,
        &["table", "insert", "people", "{not json"],
        Some(&s.key),
    );
    assert!(!o.ok && o.stderr.contains("not valid JSON"), "{}", o.stderr);
    let o = run(&s, &["table", "ls"], None);
    assert!(!o.ok && o.stderr.contains("no API key"), "{}", o.stderr);
    assert!(ok(&s, &["table", "drop", "people"]).contains("5 row"));
}
