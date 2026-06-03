// hex-backup — iii module: scheduled backup of the hex workspace to Google Drive.
//
// Single responsibility: run the personal backup script on a schedule.
//   hex::backup::gdrive — `$HEX_DIR/scripts/backup-to-gdrive.sh`  (daily 04:00)
//
// The MODULE is generic (schedule + invoke). The personal part — what gets
// backed up and to which Drive — lives in $HEX_DIR/scripts/backup-to-gdrive.sh
// (tarball → Drive Desktop folder, daily/weekly/monthly retention). Runs on the
// host (not sandboxed) so it can read the workspace + write the Drive mount.
// Standing Order S6: a nonzero exit is logged LOUD and rethrown.

const { registerWorker, Logger } = require("iii-sdk");
const { execFile } = require("node:child_process");
const { promisify } = require("node:util");
const fs = require("node:fs");

const pexec = promisify(execFile);
const HEX_DIR = process.env.HEX_DIR || `${process.env.HOME}/hex`;
const SCRIPT = `${HEX_DIR}/scripts/backup-to-gdrive.sh`;
const URL = process.env.III_URL || "ws://127.0.0.1:49134";

const iii = registerWorker(URL, { workerName: "hex-backup" });

async function runBackup() {
  const log = new Logger();
  if (!fs.existsSync(SCRIPT)) {
    log.error("hex-backup: script missing (S6)", { script: SCRIPT });
    throw new Error(`backup script not found: ${SCRIPT}`);
  }
  log.info("hex-backup: starting gdrive backup", { script: SCRIPT });
  try {
    const { stdout } = await pexec("bash", [SCRIPT], {
      timeout: 60 * 60 * 1000, // 60 min ceiling (full-workspace tarball)
      maxBuffer: 16 * 1024 * 1024,
      env: { ...process.env, HEX_DIR },
    });
    log.info("hex-backup: OK", { tail: String(stdout).slice(-400) });
    return { ok: true };
  } catch (e) {
    log.error("hex-backup: FAILED (S6)", {
      code: e.code,
      stderr: String(e.stderr || "").slice(-800),
    });
    throw e; // loud
  }
}

iii.registerFunction("hex::backup::gdrive", async () => runBackup(), {
  description: "Back up the hex workspace to Google Drive (daily tarball + retention)",
});

// 7-field cron: sec min hour day month weekday year — 04:00 daily (after the
// 03:00 nightly memory consolidate, so the backup captures the consolidated state).
iii.registerTrigger({
  type: "cron",
  function_id: "hex::backup::gdrive",
  config: { expression: "0 0 4 * * * *" },
});

new Logger().info("hex-backup registered: gdrive backup nightly@04:00");
