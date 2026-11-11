//! `powerdb bucket ...` and `powerdb object ...` against a real server, driven through the real binary.

use powerdb::util::new_id;
use std::io::Write;
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
    let dir = std::env::temp_dir().join(format!("powerdb-cli-{}", new_id()));
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

fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("powerdb-clifiles-{}", new_id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn the_cli_stores_lists_and_retrieves_objects() {
    let s = start();
    let work = tmp();
    // buckets
    assert!(ok(&s, &["bucket", "create", "documents"]).contains("Created bucket documents"));
    ok(&s, &["bucket", "create", "history", "--versioning"]);
    let ls = ok(&s, &["bucket", "ls"]);
    assert!(
        ls.contains("documents") && ls.contains("history") && ls.contains("versioned"),
        "{ls}"
    );

    // put a multi-chunk file, get it back byte for byte
    let src = work.join("acme.pdf");
    let bytes = [b"%PDF-1.4\n".as_slice(), &data(3_500_000, 1)].concat();
    std::fs::write(&src, &bytes).unwrap();
    let put = ok(
        &s,
        &[
            "object",
            "put",
            "documents/contracts/acme.pdf",
            src.to_str().unwrap(),
            "--meta",
            "customer=acme",
        ],
    );
    assert!(put.contains("Stored documents/contracts/acme.pdf"), "{put}");
    let down = work.join("downloaded.pdf");
    ok(
        &s,
        &[
            "object",
            "get",
            "documents/contracts/acme.pdf",
            down.to_str().unwrap(),
        ],
    );
    assert!(
        std::fs::read(&down).unwrap() == bytes,
        "the downloaded file differs"
    );
    assert!(
        !work.join("downloaded.pdf.part").exists(),
        "no temporary file is left behind"
    );

    // default destination: the last part of the key, in the current folder
    let o = Command::new(BIN)
        .current_dir(&work)
        .args([
            "object",
            "get",
            "documents/contracts/acme.pdf",
            "--url",
            &s.url,
            "--key",
            &s.key,
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(std::fs::read(work.join("acme.pdf")).unwrap() == bytes);

    // standard input and output
    let mut c = Command::new(BIN)
        .args([
            "object",
            "put",
            "documents/notes/hello.txt",
            "-",
            "--type",
            "text/plain",
            "--url",
            &s.url,
            "--key",
            &s.key,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(b"hello from stdin")
        .unwrap();
    assert!(c.wait_with_output().unwrap().status.success());
    let out = run(
        &s,
        &["object", "get", "documents/notes/hello.txt", "-"],
        Some(&s.key),
    );
    assert!(out.ok && out.stdout == b"hello from stdin");

    // listing, with a prefix
    let all = ok(&s, &["object", "ls", "documents"]);
    assert!(
        all.contains("contracts/acme.pdf") && all.contains("notes/hello.txt"),
        "{all}"
    );
    let some = ok(&s, &["object", "ls", "documents", "notes/"]);
    assert!(
        some.contains("notes/hello.txt") && !some.contains("acme"),
        "{some}"
    );

    // unusual keys survive the round trip
    let odd = "weird name/\u{65e5}\u{672c} & more.txt";
    std::fs::write(work.join("odd.txt"), b"odd").unwrap();
    ok(
        &s,
        &[
            "object",
            "put",
            &format!("documents/{odd}"),
            work.join("odd.txt").to_str().unwrap(),
        ],
    );
    let r = run(
        &s,
        &["object", "get", &format!("documents/{odd}"), "-"],
        Some(&s.key),
    );
    assert!(r.ok && r.stdout == b"odd", "{}", r.stderr);

    // delete
    ok(&s, &["object", "rm", "documents/notes/hello.txt"]);
    let r = run(
        &s,
        &["object", "get", "documents/notes/hello.txt", "-"],
        Some(&s.key),
    );
    assert!(!r.ok && r.stderr.contains("404"), "{}", r.stderr);
    let r = run(&s, &["bucket", "rm", "documents"], Some(&s.key));
    assert!(!r.ok && r.stderr.contains("not empty"), "{}", r.stderr);
    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn the_cli_manages_versions() {
    let s = start();
    let work = tmp();
    ok(&s, &["bucket", "create", "history", "--versioning"]);
    for (i, body) in ["one", "two", "three"].iter().enumerate() {
        let f = work.join(format!("f{i}.txt"));
        std::fs::write(&f, body).unwrap();
        ok(
            &s,
            &["object", "put", "history/doc.txt", f.to_str().unwrap()],
        );
    }
    let v = ok(&s, &["object", "versions", "history/doc.txt"]);
    let ids: Vec<&str> = v
        .lines()
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(ids.len(), 3, "{v}");
    let old = run(
        &s,
        &["object", "get", "history/doc.txt", "-", "--version", ids[2]],
        Some(&s.key),
    );
    assert_eq!(old.stdout, b"one");
    ok(&s, &["object", "rm", "history/doc.txt"]);
    assert!(
        !run(&s, &["object", "get", "history/doc.txt", "-"], Some(&s.key)).ok,
        "a delete hides the key"
    );
    ok(
        &s,
        &["object", "restore", "history/doc.txt", "--version", ids[2]],
    );
    assert_eq!(
        run(&s, &["object", "get", "history/doc.txt", "-"], Some(&s.key)).stdout,
        b"one"
    );
    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn the_cli_says_what_went_wrong() {
    let s = start();
    let r = run(&s, &["bucket", "ls"], None);
    assert!(
        !r.ok && r.stderr.contains("--key") && r.stderr.contains("POWERDB_KEY"),
        "{}",
        r.stderr
    );
    let r = run(&s, &["bucket", "ls"], Some("pdb_live_wrong"));
    assert!(!r.ok && r.stderr.contains("401"), "{}", r.stderr);
    let r = run(
        &s,
        &["object", "put", "nobucket-file", "/nonexistent"],
        Some(&s.key),
    );
    assert!(!r.ok && r.stderr.contains("BUCKET/KEY"), "{}", r.stderr);
    let r = run(
        &s,
        &["object", "put", "b/k", "/nonexistent/file"],
        Some(&s.key),
    );
    assert!(!r.ok, "{}", r.stderr);
    let r = run(&s, &["bucket", "create", "Bad Name"], Some(&s.key));
    assert!(!r.ok && r.stderr.contains("400"), "{}", r.stderr);
    // a server that is not running
    let mut c = Command::new(BIN);
    c.args([
        "bucket",
        "ls",
        "--url",
        "http://127.0.0.1:1",
        "--key",
        "pdb_live_x",
    ]);
    let o = c.output().unwrap();
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("could not reach"));
}
