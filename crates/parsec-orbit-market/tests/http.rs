//! The market over a real HTTP connection: index, signature and package are
//! served by a tiny server on the loopback interface.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use parsec_orbit_market::cli::{self, Environment};
use parsec_orbit_market::source::Real;
use parsec_orbit_market::{trust, Error};

struct NoEnv;
impl Environment for NoEnv {
    fn var(&self, _: &str) -> Option<String> {
        None
    }
}

/// Serves `files` (path -> bytes) and records the paths asked for.
fn serve(files: HashMap<String, Vec<u8>>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let record = asked.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read_exact(&mut byte).is_err() {
                    break;
                }
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            record.lock().unwrap().push(path.clone());
            let response = match files.get(&path) {
                Some(body) => {
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    response.extend_from_slice(body);
                    response
                }
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };
            let _ = stream.write_all(&response);
        }
    });
    (port, asked)
}

fn zip(id: &str, version: &str) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer
        .start_file(format!("{id}/plugin.toml"), options)
        .unwrap();
    writer
        .write_all(
            format!("id = \"{id}\"\nversion = \"{version}\"\nentry = \"p.dll\"\n").as_bytes(),
        )
        .unwrap();
    writer.start_file(format!("{id}/p.dll"), options).unwrap();
    writer.write_all(b"MZ").unwrap();
    writer.finish().unwrap().into_inner()
}

fn run(args: &[String]) -> Result<String, Error> {
    let mut out = Vec::new();
    cli::run(args, &NoEnv, &Real, &mut out)?;
    Ok(String::from_utf8(out).unwrap())
}

#[test]
fn a_plugin_is_installed_from_a_signed_index_over_http() {
    let id = "com.example.http";
    let package = zip(id, "1.0.0");
    let (secret, public) = trust::generate_keypair().unwrap();
    let index = format!(
        "{{\"format\":1,\"name\":\"http\",\"plugins\":[{{\"id\":\"{id}\",\"versions\":[{{\"version\":\"1.0.0\",\"url\":\"files/p.zip\",\"sha256\":\"{}\",\"size\":{}}}]}}]}}",
        trust::sha256_hex(&package),
        package.len()
    );
    let signature = trust::sign(index.as_bytes(), &trust::signing_key(&secret).unwrap());
    let (port, asked) = serve(HashMap::from([
        ("/market/index.json".to_string(), index.clone().into_bytes()),
        ("/market/index.json.sig".to_string(), signature.into_bytes()),
        ("/market/files/p.zip".to_string(), package),
    ]));

    let dir = std::env::temp_dir().join(format!("parsec-orbit-http-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let keys = dir.join("keys.txt");
    std::fs::write(&keys, format!("ed25519 {public} test\n")).unwrap();
    let plugins = dir.join("plugins");
    let args = |first: &[&str]| -> Vec<String> {
        let mut all: Vec<String> = first.iter().map(|s| s.to_string()).collect();
        all.extend([
            "--index".to_string(),
            format!("http://127.0.0.1:{port}/market/index.json"),
            "--plugins-dir".to_string(),
            plugins.to_string_lossy().into_owned(),
            "--keys".to_string(),
            keys.to_string_lossy().into_owned(),
        ]);
        all
    };

    let output = run(&args(&["install", id])).unwrap();
    assert!(
        output.contains("installed com.example.http 1.0.0"),
        "{output}"
    );
    assert!(plugins.join(id).join("p.dll").is_file());
    // The index, its signature and the package, nothing else.
    assert_eq!(
        *asked.lock().unwrap(),
        [
            "/market/index.json",
            "/market/index.json.sig",
            "/market/files/p.zip"
        ]
    );

    // A missing package is a clear error and installs nothing.
    let _ = std::fs::remove_dir_all(&plugins);
    let (port2, _) = serve(HashMap::from([
        ("/index.json".to_string(), index.clone().into_bytes()),
        (
            "/index.json.sig".to_string(),
            trust::sign(index.as_bytes(), &trust::signing_key(&secret).unwrap()).into_bytes(),
        ),
    ]));
    let mut broken = args(&["install", id]);
    let at = broken
        .iter()
        .position(|a| a.starts_with("http://"))
        .unwrap();
    broken[at] = format!("http://127.0.0.1:{port2}/index.json");
    let error = run(&broken).unwrap_err();
    assert!(matches!(error, Error::Network(_)), "{error}");
    assert!(!plugins.join(id).exists());
    let _ = std::fs::remove_dir_all(&dir);
}
