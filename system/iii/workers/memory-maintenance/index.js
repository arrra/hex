// hex-memory-maintenance — hex's first iii module.
//
// Single responsibility: run hex memory maintenance on schedule.
//   hex::memory::index            — `hex memory index`            (every 15 min)
//   hex::memory::consolidate_full — `hex memory consolidate full` (nightly 03:00)
//
// Runs on the HOST (not a sandboxed iii worker) so it can exec the host `hex`
// binary against the live workspace. Connects to the engine WS on :49134.
// Standing Order S6: a nonzero `hex` exit is logged LOUD and rethrown.

const { registerWorker, Logger } = require("iii-sdk");
const { execFile } = require("node:child_process");
const { promisify } = require("node:util");

const pexec = promisify(execFile);
const HEX = process.env.HEX_BIN || `${process.env.HOME}/hex/.hex/bin/hex`;
const URL = process.env.III_URL || "ws://127.0.0.1:49134";

const iii = registerWorker(URL, { workerName: "hex-memory-maintenance" });

async function runHex(args) {
  const log = new Logger();
  log.info("memory-maintenance: running", { hex: HEX, args });
  try {
    const { stdout } = await pexec(HEX, args, {
      timeout: 20 * 60 * 1000, // 20 min ceiling
      maxBuffer: 16 * 1024 * 1024,
    });
    log.info("memory-maintenance: OK", { args, tail: String(stdout).slice(-300) });
    return { ok: true, args };
  } catch (e) {
    log.error("memory-maintenance: hex FAILED (S6)", {
      args,
      code: e.code,
      stderr: String(e.stderr || "").slice(-800),
    });
    throw e; // loud — let the engine record the failure
  }
}

iii.registerFunction(
  "hex::memory::index",
  async () => runHex(["memory", "index"]),
  { description: "Rebuild the hex memory index (scheduled every 15 minutes)" },
);

iii.registerFunction(
  "hex::memory::consolidate_full",
  async () => runHex(["memory", "consolidate", "full"]),
  { description: "Full nightly hex memory consolidation (deterministic layers + LLM audit)" },
);

// 7-field cron: sec min hour day month weekday year
iii.registerTrigger({
  type: "cron",
  function_id: "hex::memory::index",
  config: { expression: "0 */15 * * * * *" }, // every 15 minutes
});

iii.registerTrigger({
  type: "cron",
  function_id: "hex::memory::consolidate_full",
  config: { expression: "0 0 3 * * * *" }, // 03:00 daily
});

new Logger().info(
  "hex-memory-maintenance registered: index q15m + consolidate_full nightly@03:00",
);
