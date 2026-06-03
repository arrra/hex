#!/usr/bin/env bash
# iii-engine.sh — lifecycle manager for the hex iii engine launchd service.
#
# Subcommands: install | start | stop | restart | status
#
# This script installs and supervises a launchd-managed iii engine process
# under the label com.hex.iii-engine. The engine is started with the
# config at $HEX_DIR/.hex/iii/iii-config.yaml. The HTTP API is expected at
# http://127.0.0.1:3111/health.
#
# Standing Order S6: every failure is LOUD (stderr + non-zero exit).
# The install subcommand renders the plist template into
# ~/Library/LaunchAgents and then PRINTS the launchctl bootstrap command
# the user must run interactively — non-interactive shells cannot
# bootstrap services into the gui domain.

set -u

LABEL="com.hex.iii-engine"
PLIST_DEST="${HOME}/Library/LaunchAgents/${LABEL}.plist"
# The iii engine exposes no /health route; liveness = the HTTP API port accepts
# a TCP connection. Host pinned to 127.0.0.1 (not localhost) per the config.
HEALTH_HOST="127.0.0.1"
HEALTH_PORT="3111"

# engine_listening: 0 if a TCP connection to the HTTP API port succeeds, else 1.
# Uses bash /dev/tcp (no nc dependency); endpoint-agnostic (any listen = up).
engine_listening() {
  (exec 3<>"/dev/tcp/${HEALTH_HOST}/${HEALTH_PORT}") 2>/dev/null && { exec 3>&- 3<&-; return 0; }
  return 1
}

# --- LOUD failure helpers (S6) ----------------------------------------------
loud_err() {
  # Print a loud, attention-grabbing error to stderr.
  printf '\n!!! iii-engine: %s\n\n' "$*" >&2
}

die() {
  loud_err "$*"
  exit 1
}

# --- Path resolution --------------------------------------------------------
resolve_hex_dir() {
  if [ -n "${HEX_DIR:-}" ] && [ -d "${HEX_DIR}" ]; then
    printf '%s\n' "${HEX_DIR}"
    return 0
  fi
  local d="${PWD}"
  while [ "${d}" != "/" ] && [ -n "${d}" ]; do
    if [ -d "${d}/.hex" ]; then
      printf '%s\n' "${d}"
      return 0
    fi
    d="$(dirname "${d}")"
  done
  return 1
}

resolve_iii_bin() {
  local bin
  bin="$(command -v iii 2>/dev/null || true)"
  if [ -z "${bin}" ]; then
    return 1
  fi
  printf '%s\n' "${bin}"
}

uid_of_user() {
  id -u
}

service_target() {
  printf 'gui/%s/%s\n' "$(uid_of_user)" "${LABEL}"
}

# --- install ----------------------------------------------------------------
cmd_install() {
  local hex_dir iii_bin workdir log_path config_path template
  hex_dir="$(resolve_hex_dir)" \
    || die "could not resolve HEX_DIR — set \$HEX_DIR or run from inside a hex workspace"
  iii_bin="$(resolve_iii_bin)" \
    || die "iii binary not found on PATH — install iii (~/.local/bin/iii) first"

  workdir="${hex_dir}/.hex/iii"
  log_path="${hex_dir}/.hex/logs/iii-engine.log"
  config_path="${workdir}/iii-config.yaml"
  template="${hex_dir}/.hex/templates/launchd/${LABEL}.plist"
  # Fallback to in-repo template (when running from a source checkout).
  if [ ! -f "${template}" ]; then
    template="${hex_dir}/system/templates/launchd/${LABEL}.plist"
  fi

  [ -f "${template}" ] \
    || die "plist template not found at ${template}"
  [ -f "${config_path}" ] \
    || loud_err "WARNING: iii-config.yaml not found at ${config_path} (engine will fail at start)"

  mkdir -p "${workdir}/data" "${hex_dir}/.hex/logs" "$(dirname "${PLIST_DEST}")" \
    || die "failed to create iii engine directories"

  # Render the template, substituting placeholders.
  sed \
    -e "s|III_BIN_PLACEHOLDER|${iii_bin}|g" \
    -e "s|CONFIG_PLACEHOLDER|${config_path}|g" \
    -e "s|WORKDIR_PLACEHOLDER|${workdir}|g" \
    -e "s|LOG_PLACEHOLDER|${log_path}|g" \
    "${template}" > "${PLIST_DEST}" \
    || die "failed to render plist to ${PLIST_DEST}"

  cat <<EOF

iii engine plist installed:
  label:      ${LABEL}
  plist:      ${PLIST_DEST}
  iii bin:    ${iii_bin}
  config:     ${config_path}
  workdir:    ${workdir}
  log:        ${log_path}

MANUAL STEP REQUIRED — run this command yourself in an interactive shell
(non-interactive/agent shells cannot bootstrap gui-domain launchd services):

  launchctl bootstrap gui/$(uid_of_user) ${PLIST_DEST}

Then verify with:

  $(basename "$0") status

EOF

  # --- Render a launchd plist per worker config (workers/<name>.yaml) -------
  # Each worker is a declarative YAML config hosted by `hex iii worker run`.
  # We render the generic iii-worker.plist template into ~/Library/LaunchAgents/
  # com.hex.iii-<name>.plist, one per config. No node, no per-worker binary —
  # the hex binary is the host. New workers (incl. personal ones dropped into
  # .hex/iii/workers/) are picked up automatically. No user paths in the repo.
  local hex_bin runtime_path workers_root wtmpl
  hex_bin="${hex_dir}/.hex/bin/hex"
  runtime_path="${hex_dir}/.hex/bin:${HOME}/.local/bin:/opt/homebrew/bin:/usr/bin:/bin"
  workers_root="${hex_dir}/.hex/iii/workers"
  wtmpl="${hex_dir}/.hex/templates/launchd/iii-worker.plist"
  [ -f "${wtmpl}" ] || wtmpl="${hex_dir}/system/templates/launchd/iii-worker.plist"
  if [ ! -f "${wtmpl}" ]; then
    loud_err "iii-worker.plist template not found — worker plists NOT rendered"
  elif [ -d "${workers_root}" ]; then
    local wcfg wname wlabel wlog wdest
    for wcfg in "${workers_root}"/*.yaml; do
      [ -f "${wcfg}" ] || continue
      wname="$(basename "${wcfg}" .yaml)"
      wlabel="com.hex.iii-${wname}"
      wlog="${hex_dir}/.hex/logs/${wlabel}.log"
      wdest="${HOME}/Library/LaunchAgents/${wlabel}.plist"
      sed \
        -e "s|LABEL_PLACEHOLDER|${wlabel}|g" \
        -e "s|HEXBIN_PLACEHOLDER|${hex_bin}|g" \
        -e "s|CONFIG_PLACEHOLDER|${wcfg}|g" \
        -e "s|HEXDIR_PLACEHOLDER|${hex_dir}|g" \
        -e "s|PATH_PLACEHOLDER|${runtime_path}|g" \
        -e "s|LOG_PLACEHOLDER|${wlog}|g" \
        "${wtmpl}" > "${wdest}" \
        || { loud_err "failed to render worker plist ${wlabel}"; continue; }
      printf '  worker %s → %s\n    bootstrap: launchctl bootstrap gui/%s %s\n' \
        "${wname}" "${wlabel}" "$(uid_of_user)" "${wdest}"
    done
  fi
}

# --- start / stop / restart -------------------------------------------------
cmd_start() {
  local tgt
  tgt="$(service_target)"
  launchctl kickstart -k "${tgt}" \
    || die "launchctl kickstart ${tgt} failed — is the service bootstrapped? Run \`$(basename "$0") install\` and follow the printed launchctl bootstrap step."
}

cmd_stop() {
  local tgt
  tgt="$(service_target)"
  launchctl kill SIGTERM "${tgt}" \
    || die "launchctl kill SIGTERM ${tgt} failed — the service may not be bootstrapped."
}

cmd_restart() {
  cmd_stop || true
  cmd_start
}

# --- status -----------------------------------------------------------------
cmd_status() {
  local tgt
  tgt="$(service_target)"

  echo "=== launchctl print ${tgt} ==="
  launchctl print "${tgt}" 2>&1 | sed -n '1,30p' || true
  echo

  echo "=== liveness probe: tcp ${HEALTH_HOST}:${HEALTH_PORT} ==="
  if engine_listening; then
    echo "iii engine: UP (HTTP API accepting connections on :${HEALTH_PORT})"
  else
    loud_err "iii engine: DOWN — nothing listening on ${HEALTH_HOST}:${HEALTH_PORT}"
    echo "Fix: run \`$(basename "$0") install\` then the printed launchctl bootstrap command, or \`$(basename "$0") start\`."
  fi
  # status always exits 0 — it reports, it does not gate.
  return 0
}

# --- dispatch ---------------------------------------------------------------
usage() {
  cat >&2 <<EOF
usage: $(basename "$0") {install|start|stop|restart|status}

  install   render the launchd plist into ~/Library/LaunchAgents and print
            the manual launchctl bootstrap command (does not run it).
  start     launchctl kickstart the ${LABEL} service.
  stop      launchctl kill SIGTERM the service.
  restart   stop then start.
  status    show launchctl print summary and probe tcp ${HEALTH_HOST}:${HEALTH_PORT}.
EOF
}

main() {
  if [ "$#" -lt 1 ]; then
    usage
    exit 2
  fi
  local sub="$1"
  shift
  case "${sub}" in
    install)  cmd_install "$@" ;;
    start)    cmd_start "$@" ;;
    stop)     cmd_stop "$@" ;;
    restart)  cmd_restart "$@" ;;
    status)   cmd_status "$@" ;;
    -h|--help|help) usage; exit 0 ;;
    *) loud_err "unknown subcommand: ${sub}"; usage; exit 2 ;;
  esac
}

main "$@"
