import { formatEvent, renderHeader } from "./render.js";
import { c } from "./util.js";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Replay a session in the terminal with the original pacing.
 * opts: {speed (default 5x), maxDelayMs (default 2000), thinking, full, sidechains, turn}
 */
export async function play(session, opts = {}) {
  const speed = opts.speed > 0 ? opts.speed : 5;
  const maxDelay = opts.maxDelayMs ?? 2000;
  const write = opts.write || ((s) => process.stdout.write(s));
  write(renderHeader(session) + "\n");
  write(`${c.dim}replaying at ${speed}x (max pause ${maxDelay}ms) — Ctrl-C to stop${c.reset}\n`);
  const start = Date.parse(session.startedAt || session.events[0]?.ts);
  let prev = null;
  for (const ev of session.events) {
    if (opts.turn && ev.turn !== opts.turn) continue;
    const line = formatEvent(ev, opts, { start });
    if (line == null) continue;
    const t = Date.parse(ev.ts);
    if (prev != null && !Number.isNaN(t) && !Number.isNaN(prev)) {
      const delay = Math.min(maxDelay, Math.max(0, (t - prev) / speed));
      if (delay > 0) await sleep(delay);
    }
    prev = t;
    write(line + "\n");
  }
}
