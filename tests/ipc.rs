use japanese_subtitle_ocr::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
fn settings() -> Settings {
    Settings {
        worker: "tests/fake_worker.py".into(),
        ..Default::default()
    }
}
#[test]
fn worker_reuses_process_and_reports_errors() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let p = tmp.path().join("image.png");
    std::fs::write(&p, b"fixture")?;
    let c = AtomicBool::new(false);
    let mut worker = Worker::start(&settings(), &c)?;
    assert_eq!(worker.capabilities().max_concurrency, 1);
    assert_eq!(worker.recognize(&p, &c)?.text, "1");
    assert_eq!(worker.recognize(&p, &c)?.text, "2");
    let e = tmp.path().join("error.png");
    std::fs::write(&e, b"fixture")?;
    assert!(worker
        .recognize(&e, &c)
        .unwrap_err()
        .to_string()
        .contains("controlled model failure"));
    Ok(())
}
#[test]
fn worker_exit_and_bad_id_are_detected() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let c = AtomicBool::new(false);
    for name in ["exit", "bad_id"] {
        let p = tmp.path().join(format!("{name}.png"));
        std::fs::write(&p, b"fixture")?;
        let mut w = Worker::start(&settings(), &c)?;
        assert!(w.recognize(&p, &c).is_err());
    }
    Ok(())
}
#[test]
fn cancellation_interrupts_inflight_worker() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let p = tmp.path().join("cancel.png");
    std::fs::write(&p, b"fixture")?;
    let c = Arc::new(AtomicBool::new(false));
    let mut w = Worker::start(&settings(), &c)?;
    let worker_pid = w.info["pid"].as_u64().unwrap();
    let flag = c.clone();
    let signal = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        flag.store(true, Ordering::Relaxed);
    });
    let now = std::time::Instant::now();
    assert!(w.recognize(&p, &c).is_err());
    assert!(now.elapsed() < std::time::Duration::from_secs(3));
    signal.join().unwrap();
    drop(w);
    #[cfg(windows)]
    {
        let output = command("tasklist")
            .args(["/FI", &format!("PID eq {worker_pid}"), "/FO", "CSV", "/NH"])
            .output()?;
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(&format!("\"{worker_pid}\"")),
            "Python interpreter survived cancellation"
        );
    }
    #[cfg(not(windows))]
    let _ = worker_pid;
    Ok(())
}

#[test]
fn model_runtime_and_cache_identity_are_isolated() -> anyhow::Result<()> {
    let c = AtomicBool::new(false);
    let old: Settings = serde_json::from_str(r#"{"fps":2,"python":"custom-python.exe"}"#)?;
    assert_eq!(old.model, MODEL);
    assert_eq!(old.python_executable(), "custom-python.exe");
    let manga = settings();
    let w = Worker::start(&manga, &c)?;
    let manga_id = w.identity();
    drop(w);
    let paddle = Settings {
        model: PADDLE_MODEL.into(),
        vl_python: manga.python.clone(),
        ..manga.clone()
    };
    assert_ne!(paddle.worker_signature(), manga.worker_signature());
    let w = Worker::start(&paddle, &c)?;
    assert!(w.identity().contains(PADDLE_MODEL));
    assert_ne!(w.identity(), manga_id);
    let paddle_id = w.identity();
    drop(w);
    let changed = Settings {
        vl_max_pixels: 501760,
        ..paddle
    };
    let w = Worker::start(&changed, &c)?;
    assert_ne!(paddle_id, w.identity());
    Ok(())
}

#[test]
fn windows_utf8_bom_settings_preserve_selected_model() -> anyhow::Result<()> {
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(br#"{"model":"PaddlePaddle/PaddleOCR-VL-1.6"}"#);
    let s = Settings::from_json_bytes(&bytes)?;
    assert_eq!(s.model, PADDLE_MODEL);
    assert_eq!(s.python_executable(), ".venv-vl/Scripts/python.exe");
    Ok(())
}
