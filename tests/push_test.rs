//! End-to-end test of the cloud push worker against a mock surveyor API — the
//! full loop (spawn pass → presign → PUT data.json → bookkeeping → dirty
//! re-push → scratch-BER 404 handling) without a network or a backend.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDate;

use ber_app::engine::ber_repository::{BerRecord, BerRepository};
use ber_app::engine::PushWorker;
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;

/// Everything the mock server saw: request path → body bytes (last one wins).
type Seen = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Minimal HTTP/1.1 server: answers the presign POST for `known_job` with PUT
/// URLs pointing back at itself, 404s presigns for any other job, and 200s the
/// PUTs. Runs until the process exits (detached thread).
fn spawn_mock_api(known_job: String) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::new(Mutex::new(HashMap::new()));
    let seen_thread = Arc::clone(&seen);
    let base_thread = base.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream);

            // Request line + headers.
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() || line.is_empty() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let mut content_length = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                let _ = reader.read_exact(&mut body);
            }
            seen_thread.lock().unwrap().insert(path.clone(), body);

            let response_body = if path.contains("/ber/presign") {
                if path.contains(&known_job) {
                    format!(
                        r#"{{"prefix":"bers/{known_job}/","dataUrl":"{base_thread}/put/data.json","photoUrls":{{}}}}"#
                    )
                } else {
                    let mut s = reader.into_inner();
                    let resp =
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                    let _ = s.write_all(resp.as_bytes());
                    continue;
                }
            } else {
                "{}".to_string()
            };
            let mut s = reader.into_inner();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });

    (base, seen)
}

fn wait_until(deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn tmp_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ber_push_test_{tag}_{}", std::process::id()))
}

#[test]
fn pushes_dirty_bers_and_sets_aside_scratch_ones() {
    // --- Local DB with two BERs: one with a cloud job, one scratch. ---------
    let db_path = tmp_path("db");
    let _ = std::fs::remove_file(&db_path);
    let conn = ber_app::engine::open_db(&db_path).unwrap();
    let repo = BerRepository::new(&conn);

    let with_job = ItemId::default();
    let scratch = ItemId::default();
    for (id, address) in [(with_job, "1 Cloud Rd"), (scratch, "2 Local Lane")] {
        let mut ber = Ber::new();
        ber.id = id;
        ber.address = address.to_string();
        repo.upsert(&BerRecord {
            id,
            name: address.to_string(),
            date_assessed: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            data: ber,
        })
        .unwrap();
    }

    // --- Mock API that only knows the first job. ----------------------------
    let (base, seen) = spawn_mock_api(with_job.to_string());
    let cfg_path = tmp_path("sync.json");
    std::fs::write(
        &cfg_path,
        format!(r#"{{"baseUrl":"{base}","apiKey":"test-key"}}"#),
    )
    .unwrap();

    // --- Spawn: the initial pass should push the job BER, 404 the scratch. --
    let worker = PushWorker::spawn(db_path.clone(), cfg_path.clone()).expect("configured");
    assert!(
        wait_until(Duration::from_secs(10), || worker.status().pushed >= 1),
        "initial pass never pushed: {:?}",
        worker.status()
    );

    let data_put = seen
        .lock()
        .unwrap()
        .get("/put/data.json")
        .cloned()
        .expect("data.json was PUT");
    let pushed_ber: Ber = serde_json::from_slice(&data_put).unwrap();
    assert_eq!(pushed_ber.address, "1 Cloud Rd");
    assert!(
        seen.lock()
            .unwrap()
            .contains_key(&format!("/surveyor/jobs/{}/ber/presign", scratch.0)),
        "scratch BER should have been presign-attempted once"
    );

    let status = worker.status();
    assert_eq!(status.pending, 0, "scratch 404 is set aside, not pending");

    // Bookkeeping recorded against the pushed BER.
    assert!(repo.push_state(&with_job).unwrap().is_some());
    assert!(repo.push_state(&scratch).unwrap().is_none());

    // --- A content change re-pushes (trigger → debounce → pass). ------------
    seen.lock().unwrap().remove("/put/data.json");
    let mut record = repo.load(&with_job).unwrap();
    record.data.eircode = "A98X7Y2".to_string();
    repo.upsert(&record).unwrap();
    worker.trigger();
    assert!(
        wait_until(Duration::from_secs(10), || worker.status().pushed >= 2),
        "dirty re-push never happened: {:?}",
        worker.status()
    );
    let data_put = seen
        .lock()
        .unwrap()
        .get("/put/data.json")
        .cloned()
        .expect("updated data.json was PUT");
    let pushed_ber: Ber = serde_json::from_slice(&data_put).unwrap();
    assert_eq!(pushed_ber.eircode, "A98X7Y2");

    // --- A clean pass pushes nothing. ---------------------------------------
    seen.lock().unwrap().remove("/put/data.json");
    worker.trigger();
    std::thread::sleep(Duration::from_secs(4)); // debounce + pass window
    assert!(
        !seen.lock().unwrap().contains_key("/put/data.json"),
        "clean BER must not re-push"
    );

    drop(worker);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&cfg_path);
}
