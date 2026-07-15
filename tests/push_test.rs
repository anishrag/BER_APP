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

/// Minimal HTTP/1.1 server: answers presign POSTs for known jobs with PUT
/// URLs pointing back at itself, 404s presigns for unknown jobs, registers a
/// job on POST /surveyor/jobs (like the real backend), and 200s the PUTs.
/// Runs until the process exits (detached thread).
fn spawn_mock_api(known_job: String) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::new(Mutex::new(HashMap::new()));
    let seen_thread = Arc::clone(&seen);
    let base_thread = base.clone();
    let known: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![known_job]));

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
            seen_thread
                .lock()
                .unwrap()
                .insert(path.clone(), body.clone());

            let response_body = if path == "/surveyor/jobs" {
                // Registration: remember the jobId like the real backend.
                let body_text = String::from_utf8_lossy(&body).to_string();
                if let Some(id) = body_text
                    .split("\"jobId\":\"")
                    .nth(1)
                    .and_then(|s| s.split('"').next())
                {
                    known.lock().unwrap().push(id.to_string());
                }
                r#"{"existed":false}"#.to_string()
            } else if path.contains("/ber/presign") {
                if known.lock().unwrap().iter().any(|j| path.contains(j)) {
                    format!(
                        r#"{{"prefix":"bers/x/","dataUrl":"{base_thread}/put/data-{}.json","photoUrls":{{}}}}"#,
                        path.split('/').nth(3).unwrap_or("x")
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
fn pushes_dirty_bers_registers_queued_ones_and_sets_aside_the_rest() {
    // --- Local DB with three BERs: one with a cloud job, one pre-feature
    // scratch (not queued), one fresh from-scratch (queued for registration).
    let db_path = tmp_path("db");
    let _ = std::fs::remove_file(&db_path);
    let conn = ber_app::engine::open_db(&db_path).unwrap();
    let repo = BerRepository::new(&conn);

    let with_job = ItemId::default();
    let scratch = ItemId::default();
    let newborn = ItemId::default();
    for (id, address) in [
        (with_job, "1 Cloud Rd"),
        (scratch, "2 Local Lane"),
        (newborn, "3 Fresh Ave"),
    ] {
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
    repo.enqueue_job_registration(&newborn).unwrap();

    // --- Mock API that only knows the first job (registration adds more). ---
    let (base, seen) = spawn_mock_api(with_job.to_string());
    let cfg_path = tmp_path("sync.json");
    std::fs::write(
        &cfg_path,
        format!(r#"{{"baseUrl":"{base}","apiKey":"test-key"}}"#),
    )
    .unwrap();

    // --- Spawn: the initial pass pushes the job BER, registers + pushes the
    // queued one, and 404-sets-aside the pre-feature scratch.
    let worker = PushWorker::spawn(db_path.clone(), cfg_path.clone()).expect("configured");
    assert!(
        wait_until(Duration::from_secs(10), || worker.status().pushed >= 2),
        "initial pass never pushed both: {:?}",
        worker.status()
    );

    let data_key = |id: &ItemId| format!("/put/data-{}.json", id.0);
    let data_put = seen
        .lock()
        .unwrap()
        .get(&data_key(&with_job))
        .cloned()
        .expect("job BER data.json was PUT");
    let pushed_ber: Ber = serde_json::from_slice(&data_put).unwrap();
    assert_eq!(pushed_ber.address, "1 Cloud Rd");

    // The queued BER registered (with its label) and then pushed.
    let reg_body = seen
        .lock()
        .unwrap()
        .get("/surveyor/jobs")
        .cloned()
        .expect("queued BER should have POSTed /surveyor/jobs");
    let reg_text = String::from_utf8_lossy(&reg_body).to_string();
    assert!(reg_text.contains(&newborn.0.to_string()), "{reg_text}");
    assert!(reg_text.contains("3 Fresh Ave"), "{reg_text}");
    assert!(
        seen.lock().unwrap().contains_key(&data_key(&newborn)),
        "registered BER data.json was PUT"
    );
    assert!(
        !repo.job_registration_queued(&newborn).unwrap(),
        "successful registration dequeues"
    );

    // The pre-feature scratch was attempted once and set aside, not pending.
    assert!(
        seen.lock()
            .unwrap()
            .contains_key(&format!("/surveyor/jobs/{}/ber/presign", scratch.0)),
        "scratch BER should have been presign-attempted once"
    );
    assert!(!seen.lock().unwrap().contains_key(&data_key(&scratch)));
    let status = worker.status();
    assert_eq!(status.pending, 0, "scratch 404 is set aside, not pending");

    // Bookkeeping recorded against the pushed BERs only.
    assert!(repo.push_state(&with_job).unwrap().is_some());
    assert!(repo.push_state(&newborn).unwrap().is_some());
    assert!(repo.push_state(&scratch).unwrap().is_none());

    // --- A content change re-pushes (trigger → debounce → pass). ------------
    seen.lock().unwrap().remove(&data_key(&with_job));
    let mut record = repo.load(&with_job).unwrap();
    record.data.eircode = "A98X7Y2".to_string();
    repo.upsert(&record).unwrap();
    worker.trigger();
    assert!(
        wait_until(Duration::from_secs(10), || worker.status().pushed >= 3),
        "dirty re-push never happened: {:?}",
        worker.status()
    );
    let data_put = seen
        .lock()
        .unwrap()
        .get(&data_key(&with_job))
        .cloned()
        .expect("updated data.json was PUT");
    let pushed_ber: Ber = serde_json::from_slice(&data_put).unwrap();
    assert_eq!(pushed_ber.eircode, "A98X7Y2");

    // --- A clean pass pushes nothing. ---------------------------------------
    seen.lock().unwrap().remove(&data_key(&with_job));
    seen.lock().unwrap().remove(&data_key(&newborn));
    worker.trigger();
    std::thread::sleep(Duration::from_secs(4)); // debounce + pass window
    assert!(
        !seen.lock().unwrap().contains_key(&data_key(&with_job)),
        "clean BER must not re-push"
    );
    assert!(
        !seen.lock().unwrap().contains_key(&data_key(&newborn)),
        "registered BER must not re-push when clean"
    );

    drop(worker);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&cfg_path);
}
