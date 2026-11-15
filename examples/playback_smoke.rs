use japanese_subtitle_ocr::{
    playback::{PlaybackEvent, Player},
    *,
};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .expect("Pass an audiovisual test video");
    let s = Settings::default();
    let v = probe(std::path::Path::new(&path), &s, &AtomicBool::new(false))?;
    let mut p = Player::new();
    p.load(v, s, 0., false, 0.);
    let now = Instant::now();
    let mut frames = 0;
    let mut audio = false;
    loop {
        let (f, events) = p.poll();
        if let Some(f) = f {
            frames += 1;
            if frames % 30 == 0 {
                println!("frame {frames} at {:.3}s", f.time);
            }
        }
        for event in events {
            match event {
                PlaybackEvent::Ready { audio: a } => {
                    audio = a;
                    println!("Ready, audio device active: {audio}");
                }
                PlaybackEvent::Notice(s) => println!("{s}"),
                PlaybackEvent::Error(s) => anyhow::bail!(s),
                PlaybackEvent::Ended => {
                    println!(
                        "Ended: {frames} displayed frames; audio={audio}; elapsed={:.2}s",
                        now.elapsed().as_secs_f64()
                    );
                    anyhow::ensure!(frames > 10, "Too few frames");
                    return Ok(());
                }
            }
        }
        anyhow::ensure!(now.elapsed() < Duration::from_secs(20), "Playback timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
}
