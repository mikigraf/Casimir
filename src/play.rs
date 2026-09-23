//! Timed playback of a session in the terminal.
use std::io::Write;
use std::time::Duration;

use crate::model::Session;
use crate::render::{format_event, render_header, session_start_ms, RenderOpts};
use crate::util::{colors, ts_ms};

pub struct PlayOpts {
    pub speed: f64,
    pub max_delay_ms: u64,
    pub render: RenderOpts,
}

/// Replay a session with the original pacing (scaled by `speed`, capped by `max_delay_ms`).
pub fn play(session: &Session, o: &PlayOpts, out: &mut dyn Write) -> std::io::Result<()> {
    let c = colors();
    let speed = if o.speed > 0.0 { o.speed } else { 5.0 };
    writeln!(out, "{}", render_header(session))?;
    writeln!(
        out,
        "{}replaying at {}x (max pause {}ms) — Ctrl-C to stop{}",
        c.dim, speed, o.max_delay_ms, c.reset
    )?;
    let start = session_start_ms(session);
    let mut prev: Option<i64> = None;
    for ev in &session.events {
        if o.render.turn.is_some_and(|t| ev.turn != t) {
            continue;
        }
        let Some(line) = format_event(ev, &o.render, start) else {
            continue;
        };
        let t = ts_ms(&ev.ts);
        if let (Some(p), Some(t)) = (prev, t) {
            let delay = (((t - p).max(0) as f64) / speed) as u64;
            let delay = delay.min(o.max_delay_ms);
            if delay > 0 {
                out.flush()?;
                std::thread::sleep(Duration::from_millis(delay));
            }
        }
        if t.is_some() {
            prev = t;
        }
        writeln!(out, "{line}")?;
    }
    Ok(())
}
