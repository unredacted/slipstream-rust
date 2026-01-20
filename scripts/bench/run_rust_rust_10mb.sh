#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CERT_DIR="${CERT_DIR:-"${ROOT_DIR}/fixtures/certs"}"

DNS_LISTEN_PORT="${DNS_LISTEN_PORT:-8853}"
PROXY_RECURSIVE_PORT="${PROXY_RECURSIVE_PORT:-5300}"
PROXY_AUTHORITATIVE_PORT="${PROXY_AUTHORITATIVE_PORT:-5301}"
USE_PROXY="${USE_PROXY:-0}"
RECURSIVE_ADDR="${RECURSIVE_ADDR:-}"
AUTHORITATIVE_ADDR="${AUTHORITATIVE_ADDR:-}"
TCP_TARGET_PORT="${TCP_TARGET_PORT:-5201}"
CLIENT_TCP_PORT="${CLIENT_TCP_PORT:-7000}"
DOMAIN="${DOMAIN:-test.com}"
SOCKET_TIMEOUT="${SOCKET_TIMEOUT:-}"
TRANSFER_BYTES="${TRANSFER_BYTES:-10485760}"
CHUNK_SIZE="${CHUNK_SIZE:-16384}"
PREFACE_BYTES="${PREFACE_BYTES:-1}"
RUNS="${RUNS:-1}"
RUN_EXFIL="${RUN_EXFIL:-1}"
RUN_DOWNLOAD="${RUN_DOWNLOAD:-1}"
MIN_AVG_MIB_S="${MIN_AVG_MIB_S:-}"
MIN_AVG_MIB_S_EXFIL="${MIN_AVG_MIB_S_EXFIL:-}"
MIN_AVG_MIB_S_DOWNLOAD="${MIN_AVG_MIB_S_DOWNLOAD:-}"
NETEM_IFACE="${NETEM_IFACE:-lo}"
NETEM_DELAY_MS="${NETEM_DELAY_MS:-}"
NETEM_JITTER_MS="${NETEM_JITTER_MS:-}"
NETEM_DIST="${NETEM_DIST:-normal}"
NETEM_SUDO="${NETEM_SUDO:-1}"
NETEM_ACTIVE=0
PROXY_DELAY_MS="${PROXY_DELAY_MS:-}"
PROXY_JITTER_MS="${PROXY_JITTER_MS:-}"
PROXY_DIST="${PROXY_DIST:-normal}"
PROXY_PORT="${PROXY_PORT:-}"
PROXY_REORDER_PROB="${PROXY_REORDER_PROB:-}"
PROXY_BURST_CORRELATION="${PROXY_BURST_CORRELATION:-}"
DEBUG_WAIT_SECS="${DEBUG_WAIT_SECS:-2}"
DEBUG_LOG_WAIT_SECS="${DEBUG_LOG_WAIT_SECS:-5}"
CLIENT_ARGS="${CLIENT_ARGS:-}"
RESOLVER_MODE="${RESOLVER_MODE:-resolver}"

client_extra_args=()
if [[ -n "${CLIENT_ARGS}" ]]; then
  read -r -a client_extra_args <<< "${CLIENT_ARGS}"
fi

case "${RESOLVER_MODE}" in
  resolver|authoritative|mixed) ;;
  *)
    echo "RESOLVER_MODE must be resolver, authoritative, or mixed (got: ${RESOLVER_MODE})" >&2
    exit 1
    ;;
esac

run_dir_prefix="bench-rust-rust"
default_timeout=30
default_download=10
if [[ "${RESOLVER_MODE}" == "mixed" ]]; then
  run_dir_prefix="bench-rust-rust-mixed"
fi
default_exfil=5

if [[ -z "${MIN_AVG_MIB_S_EXFIL}" ]]; then
  MIN_AVG_MIB_S_EXFIL="${default_exfil}"
fi
if [[ -z "${MIN_AVG_MIB_S_DOWNLOAD}" ]]; then
  MIN_AVG_MIB_S_DOWNLOAD="${default_download}"
fi
if [[ -n "${MIN_AVG_MIB_S}" ]]; then
  MIN_AVG_MIB_S_EXFIL="${MIN_AVG_MIB_S}"
  MIN_AVG_MIB_S_DOWNLOAD="${MIN_AVG_MIB_S}"
fi
if [[ -z "${SOCKET_TIMEOUT}" ]]; then
  SOCKET_TIMEOUT="${default_timeout}"
fi

RUN_DIR="${RUN_DIR:-"${ROOT_DIR}/.interop/${run_dir_prefix}-$(date +%Y%m%d_%H%M%S)"}"

if [[ ! -f "${CERT_DIR}/cert.pem" || ! -f "${CERT_DIR}/key.pem" ]]; then
  echo "Missing test certs in ${CERT_DIR}. Set CERT_DIR to override." >&2
  exit 1
fi

mkdir -p "${RUN_DIR}" "${ROOT_DIR}/.interop"

cleanup_pids() {
  for pid in "${CLIENT_PID:-}" "${SERVER_PID:-}" "${TARGET_PID:-}" "${PROXY_PID:-}" \
    "${PROXY_RECURSIVE_PID:-}" "${PROXY_AUTHORITATIVE_PID:-}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      wait "${pid}" 2>/dev/null || true
    fi
  done
  CLIENT_PID=""
  SERVER_PID=""
  TARGET_PID=""
  PROXY_PID=""
  PROXY_RECURSIVE_PID=""
  PROXY_AUTHORITATIVE_PID=""
}

setup_netem() {
  if [[ -z "${NETEM_DELAY_MS}" ]]; then
    return
  fi
  if ! command -v tc >/dev/null 2>&1; then
    echo "tc not found; install iproute2 or unset NETEM_DELAY_MS." >&2
    exit 1
  fi

  local args=(qdisc replace dev "${NETEM_IFACE}" root netem delay "${NETEM_DELAY_MS}ms")
  if [[ -n "${NETEM_JITTER_MS}" ]]; then
    args+=("${NETEM_JITTER_MS}ms" distribution "${NETEM_DIST}")
  fi

  echo "Applying netem on ${NETEM_IFACE}: delay ${NETEM_DELAY_MS}ms${NETEM_JITTER_MS:+ jitter ${NETEM_JITTER_MS}ms}." >&2
  if [[ "$(id -u)" -eq 0 ]]; then
    tc "${args[@]}"
  else
    if [[ "${NETEM_SUDO}" == "1" ]]; then
      sudo -n tc "${args[@]}" || {
        echo "Failed to apply netem; re-run with sudo or set NETEM_SUDO=0." >&2
        exit 1
      }
    else
      echo "NETEM requires root; re-run with sudo or set NETEM_SUDO=1." >&2
      exit 1
    fi
  fi
  NETEM_ACTIVE=1
}

cleanup_netem() {
  if [[ "${NETEM_ACTIVE}" != "1" ]]; then
    return
  fi
  local args=(qdisc del dev "${NETEM_IFACE}" root)
  if [[ "$(id -u)" -eq 0 ]]; then
    tc "${args[@]}" || true
  else
    if [[ "${NETEM_SUDO}" == "1" ]]; then
      sudo -n tc "${args[@]}" || true
    fi
  fi
}

cleanup() {
  cleanup_pids
  cleanup_netem
}
trap cleanup EXIT INT TERM HUP

e2e_report() {
  local label="$1"
  local start_log="$2"
  local end_log="$3"
  local bytes="$4"
  "${ROOT_DIR}/target/release/slipstream-bench" e2e-report \
    --label "${label}" \
    --start-log "${start_log}" \
    --end-log "${end_log}" \
    --bytes "${bytes}" || true
}

enforce_min_avg() {
  local args=(--run-dir "${RUN_DIR}" --bytes "${TRANSFER_BYTES}")
  if [[ -n "${MIN_AVG_MIB_S}" ]]; then
    args+=(--min-avg "${MIN_AVG_MIB_S}")
  fi
  if [[ -n "${MIN_AVG_MIB_S_EXFIL}" ]]; then
    args+=(--min-avg-exfil "${MIN_AVG_MIB_S_EXFIL}")
  fi
  if [[ -n "${MIN_AVG_MIB_S_DOWNLOAD}" ]]; then
    args+=(--min-avg-download "${MIN_AVG_MIB_S_DOWNLOAD}")
  fi
  if [[ "${RUN_EXFIL}" == "0" ]]; then
    args+=(--run-exfil false)
  fi
  if [[ "${RUN_DOWNLOAD}" == "0" ]]; then
    args+=(--run-download false)
  fi
  "${ROOT_DIR}/target/release/slipstream-bench" enforce-min-avg "${args[@]}"
}

wait_for_log() {
  local label="$1"
  local log_path="$2"
  local pattern="$3"
  local attempts="${4:-10}"
  for _ in $(seq 1 "${attempts}"); do
    if [[ -s "${log_path}" ]] && grep -Eq "${pattern}" "${log_path}"; then
      return 0
    fi
    sleep 1
  done
  echo "Timed out waiting for ${label}; see ${log_path}." >&2
  return 1
}

client_has_arg() {
  local needle="$1"
  shift
  for arg in "$@"; do
    if [[ "${arg}" == "${needle}" ]]; then
      return 0
    fi
  done
  return 1
}

client_debug_poll_enabled() {
  if client_has_arg "--debug-poll" "$@"; then
    return 0
  fi
  if client_has_arg "--debug-poll" "${client_extra_args[@]}"; then
    return 0
  fi
  return 1
}

wait_for_log_patterns() {
  local label="$1"
  local log_path="$2"
  local attempts="$3"
  shift 3
  local missing=()
  for _ in $(seq 1 "${attempts}"); do
    missing=()
    for pattern in "$@"; do
      if [[ ! -s "${log_path}" ]] || ! grep -Eq "${pattern}" "${log_path}"; then
        missing+=("${pattern}")
      fi
    done
    if [[ ${#missing[@]} -eq 0 ]]; then
      return 0
    fi
    sleep 1
  done
  echo "Timed out waiting for ${label} (${log_path}); missing patterns: ${missing[*]}." >&2
  return 1
}

start_client() {
  local log_path="$1"
  shift
  local rust_log=""
  if client_debug_poll_enabled "$@"; then
    rust_log="debug"
  fi
  if [[ -n "${rust_log}" ]]; then
    RUST_LOG="${rust_log}" "${ROOT_DIR}/target/release/slipstream-client" \
      --tcp-listen-port "${CLIENT_TCP_PORT}" \
      --domain "${DOMAIN}" \
      "$@" \
      "${client_extra_args[@]}" \
      >"${log_path}" 2>&1 &
  else
    "${ROOT_DIR}/target/release/slipstream-client" \
      --tcp-listen-port "${CLIENT_TCP_PORT}" \
      --domain "${DOMAIN}" \
      "$@" \
      "${client_extra_args[@]}" \
      >"${log_path}" 2>&1 &
  fi
  CLIENT_PID=$!
}

stop_client() {
  if [[ -n "${CLIENT_PID:-}" ]] && kill -0 "${CLIENT_PID}" 2>/dev/null; then
    kill "${CLIENT_PID}" 2>/dev/null || true
    wait "${CLIENT_PID}" 2>/dev/null || true
  fi
  CLIENT_PID=""
}

start_target() {
  local label="$1"
  local target_mode="$2"
  local preface_bytes="$3"
  local target_json="${RUN_DIR}/target_${label}.jsonl"
  local target_log="${RUN_DIR}/target_${label}.log"
  
  # Map tcp_bench.py modes to slipstream-bench subcommands
  local bench_cmd
  local bench_args=(--listen "127.0.0.1:${TCP_TARGET_PORT}" --bytes "${TRANSFER_BYTES}" --chunk-size "${CHUNK_SIZE}" --timeout "${SOCKET_TIMEOUT}" --log "${target_json}")
  if [[ "${target_mode}" == "sink" ]]; then
    bench_cmd="sink"
  else
    bench_cmd="source"
    if [[ "${preface_bytes}" -gt 0 ]]; then
      bench_args+=(--preface-bytes "${preface_bytes}")
    fi
  fi
  
  "${ROOT_DIR}/target/release/slipstream-bench" "${bench_cmd}" "${bench_args[@]}" >"${target_log}" 2>&1 &
  TARGET_PID=$!
  if ! wait_for_log "bench target (${label})" "${target_json}" '"event":"listening"'; then
    return 1
  fi
}

stop_target() {
  if [[ -n "${TARGET_PID:-}" ]] && kill -0 "${TARGET_PID}" 2>/dev/null; then
    kill "${TARGET_PID}" 2>/dev/null || true
    wait "${TARGET_PID}" 2>/dev/null || true
  fi
  TARGET_PID=""
}

run_bench_client() {
  local label="$1"
  local client_mode="$2"
  local preface_bytes="$3"
  local bench_json="${RUN_DIR}/bench_${label}.jsonl"
  local bench_log="${RUN_DIR}/bench_${label}.log"
  
  # Map tcp_bench.py modes to slipstream-bench subcommands
  local bench_cmd
  local bench_args=(--connect "127.0.0.1:${CLIENT_TCP_PORT}" --bytes "${TRANSFER_BYTES}" --chunk-size "${CHUNK_SIZE}" --timeout "${SOCKET_TIMEOUT}" --log "${bench_json}")
  if [[ "${client_mode}" == "send" ]]; then
    bench_cmd="send"
  else
    bench_cmd="recv"
    if [[ "${preface_bytes}" -gt 0 ]]; then
      bench_args+=(--preface-bytes "${preface_bytes}")
    fi
  fi
  
  if ! "${ROOT_DIR}/target/release/slipstream-bench" "${bench_cmd}" "${bench_args[@]}" >"${bench_log}" 2>&1; then
    echo "Bench transfer failed (${label}); see logs in ${RUN_DIR}." >&2
    return 1
  fi
}

extract_e2e_mib_s() {
  "${ROOT_DIR}/target/release/slipstream-bench" extract-mib-s \
    --start-log "$1" \
    --end-log "$2" \
    --bytes "$3"
}

report_throughput() {
  local label="$1"
  local mixed="$2"
  if [[ -n "${mixed}" ]]; then
    printf "throughput %s mixed MiB/s=%s\n" "${label}" "${mixed}"
  fi
}

enforce_min_throughput() {
  local label="$1"
  local value="$2"
  local threshold="$3"
  if [[ -z "${threshold}" ]]; then
    return 0
  fi
  "${ROOT_DIR}/target/release/slipstream-bench" enforce-min-throughput \
    --label "${label}" \
    --value "${value}" \
    --threshold "${threshold}"
}

run_client_bench() {
  local label="$1"
  local target_mode="$2"
  local client_mode="$3"
  shift 3
  local client_log="${RUN_DIR}/client_${label}.log"
  local target_json="${RUN_DIR}/target_${label}.jsonl"
  local bench_json="${RUN_DIR}/bench_${label}.jsonl"
  local preface_bytes=0
  local start_path="${bench_json}"
  local end_path="${target_json}"
  local debug_poll=0

  if [[ "${client_mode}" == "recv" ]]; then
    preface_bytes="${PREFACE_BYTES}"
    start_path="${target_json}"
    end_path="${bench_json}"
  fi
  if client_debug_poll_enabled "$@"; then
    debug_poll=1
  fi

  if ! start_target "${label}" "${target_mode}" "${preface_bytes}"; then
    stop_target
    return 1
  fi
  start_client "${client_log}" "$@"
  echo "Waiting for Rust client (${label}) to accept connections..." >&2
  if ! wait_for_log "Rust client (${label})" "${client_log}" "Listening on TCP port"; then
    stop_client
    stop_target
    return 1
  fi
  if ! run_bench_client "${label}" "${client_mode}" "${preface_bytes}"; then
    stop_client
    stop_target
    return 1
  fi
  if ! wait "${TARGET_PID}"; then
    echo "Target server failed (${label}); see logs in ${RUN_DIR}." >&2
    stop_client
    stop_target
    return 1
  fi
  if [[ "${label}" == mixed_* && "${debug_poll}" == "1" ]]; then
    if ! wait_for_log_patterns \
      "mixed debug output (${label})" \
      "${client_log}" \
      "${DEBUG_LOG_WAIT_SECS}" \
      "mode=Recursive" \
      "mode=Authoritative" \
      "mode=Authoritative.*pacing_rate="; then
      stop_client
      stop_target
      return 1
    fi
    sleep "${DEBUG_WAIT_SECS}"
  fi
  stop_client
  stop_target

  extract_e2e_mib_s "${start_path}" "${end_path}" "${TRANSFER_BYTES}"
}

cargo build -p slipstream-server -p slipstream-client -p slipstream-bench --release

run_case() {
  local case_name="$1"
  local target_mode="$2"
  local client_mode="$3"
  local run_id="${4:-}"
  local case_base="${case_name##*/}"
  local case_dir="${RUN_DIR}/${case_name}"
  local preface_args=()
  local target_preface_args=()
  local resolver_port="${DNS_LISTEN_PORT}"
  mkdir -p "${case_dir}"

  if [[ "${client_mode}" == "recv" && "${PREFACE_BYTES}" -gt 0 ]]; then
    preface_args=(--preface-bytes "${PREFACE_BYTES}")
    target_preface_args=(--preface-bytes "${PREFACE_BYTES}")
  fi

  if [[ -n "${PROXY_DELAY_MS}" ]]; then
    local proxy_port="${PROXY_PORT:-$((DNS_LISTEN_PORT + 1))}"
    if [[ "${proxy_port}" -eq "${DNS_LISTEN_PORT}" ]]; then
      echo "Proxy port ${proxy_port} conflicts with DNS_LISTEN_PORT." >&2
      return 1
    fi
    local proxy_args=(
      --listen "127.0.0.1:${proxy_port}"
      --upstream "127.0.0.1:${DNS_LISTEN_PORT}"
      --delay-ms "${PROXY_DELAY_MS}"
      --dist "${PROXY_DIST}"
      --log "${case_dir}/dns_proxy.jsonl"
    )
    if [[ -n "${PROXY_JITTER_MS}" ]]; then
      proxy_args+=(--jitter-ms "${PROXY_JITTER_MS}")
    fi
    if [[ -n "${PROXY_REORDER_PROB}" ]]; then
      proxy_args+=(--reorder-rate "${PROXY_REORDER_PROB}")
    fi
    "${ROOT_DIR}/target/release/slipstream-bench" udp-proxy \
      "${proxy_args[@]}" \
      >"${case_dir}/dns_proxy.log" 2>&1 &
    PROXY_PID=$!
    resolver_port="${proxy_port}"
  fi

  # Start target using slipstream-bench
  local bench_cmd
  local bench_args=(--listen "127.0.0.1:${TCP_TARGET_PORT}" --bytes "${TRANSFER_BYTES}" --chunk-size "${CHUNK_SIZE}" --timeout "${SOCKET_TIMEOUT}" --log "${case_dir}/target.jsonl")
  if [[ "${target_mode}" == "sink" ]]; then
    bench_cmd="sink"
  else
    bench_cmd="source"
    if [[ -n "${target_preface_args[*]:-}" ]]; then
      bench_args+=(${target_preface_args[@]})
    fi
  fi
  "${ROOT_DIR}/target/release/slipstream-bench" "${bench_cmd}" "${bench_args[@]}" >"${case_dir}/target.log" 2>&1 &
  TARGET_PID=$!

  "${ROOT_DIR}/target/release/slipstream-server" \
    --dns-listen-port "${DNS_LISTEN_PORT}" \
    --target-address "127.0.0.1:${TCP_TARGET_PORT}" \
    --domain "${DOMAIN}" \
    --cert "${CERT_DIR}/cert.pem" \
    --key "${CERT_DIR}/key.pem" \
    >"${case_dir}/server.log" 2>&1 &
  SERVER_PID=$!

  "${ROOT_DIR}/target/release/slipstream-client" \
    --tcp-listen-port "${CLIENT_TCP_PORT}" \
    --"${RESOLVER_MODE}" "127.0.0.1:${resolver_port}" \
    --domain "${DOMAIN}" \
    ${client_extra_args[@]+"${client_extra_args[@]}"} \
    >"${case_dir}/client.log" 2>&1 &
  CLIENT_PID=$!

  if [[ -n "${run_id}" ]]; then
    echo "Running ${case_name} benchmark (run ${run_id})..."
  else
    echo "Running ${case_name} benchmark..."
  fi
  sleep 2

  # Run client bench using slipstream-bench
  local client_bench_cmd
  local client_bench_args=(--connect "127.0.0.1:${CLIENT_TCP_PORT}" --bytes "${TRANSFER_BYTES}" --chunk-size "${CHUNK_SIZE}" --timeout "${SOCKET_TIMEOUT}" --log "${case_dir}/bench.jsonl")
  if [[ "${client_mode}" == "send" ]]; then
    client_bench_cmd="send"
  else
    client_bench_cmd="recv"
    if [[ -n "${preface_args[*]:-}" ]]; then
      client_bench_args+=(${preface_args[@]})
    fi
  fi
  "${ROOT_DIR}/target/release/slipstream-bench" "${client_bench_cmd}" "${client_bench_args[@]}" 2>&1 | tee "${case_dir}/bench.log"

  wait "${TARGET_PID}" || {
    echo "Target ${case_name} server failed." >&2
    return 1
  }
  local label="end-to-end ${case_base}"
  if [[ -n "${run_id}" ]]; then
    label="${label} (run ${run_id})"
  fi
  if [[ "${case_base}" == "exfil" ]]; then
    e2e_report "${label}" "${case_dir}/bench.jsonl" "${case_dir}/target.jsonl" "${TRANSFER_BYTES}"
  else
    e2e_report "${label}" "${case_dir}/target.jsonl" "${case_dir}/bench.jsonl" "${TRANSFER_BYTES}"
  fi
  cleanup_pids
}

run_mixed() {
  local use_proxy="${USE_PROXY}"
  if [[ -n "${PROXY_DELAY_MS}" ]]; then
    use_proxy=1
  fi

  if [[ "${use_proxy}" == "1" ]]; then
    RECURSIVE_ADDR="${RECURSIVE_ADDR:-127.0.0.1:${PROXY_RECURSIVE_PORT}}"
    AUTHORITATIVE_ADDR="${AUTHORITATIVE_ADDR:-127.0.0.1:${PROXY_AUTHORITATIVE_PORT}}"
  else
    RECURSIVE_ADDR="${RECURSIVE_ADDR:-127.0.0.1:${DNS_LISTEN_PORT}}"
    AUTHORITATIVE_ADDR="${AUTHORITATIVE_ADDR:-[::1]:${DNS_LISTEN_PORT}}"
  fi

  if [[ "${RECURSIVE_ADDR}" == "${AUTHORITATIVE_ADDR}" ]]; then
    echo "Recursive and authoritative resolver addresses must differ; set RECURSIVE_ADDR/AUTHORITATIVE_ADDR or USE_PROXY=1." >&2
    return 1
  fi

  if [[ -z "${PROXY_DELAY_MS}" ]]; then
    setup_netem
  fi

  "${ROOT_DIR}/target/release/slipstream-server" \
    --dns-listen-port "${DNS_LISTEN_PORT}" \
    --target-address "127.0.0.1:${TCP_TARGET_PORT}" \
    --domain "${DOMAIN}" \
    --cert "${CERT_DIR}/cert.pem" \
    --key "${CERT_DIR}/key.pem" \
    >"${RUN_DIR}/server.log" 2>&1 &
  SERVER_PID=$!

  if [[ "${use_proxy}" == "1" ]]; then
    local proxy_recursive_args=(
      --listen "127.0.0.1:${PROXY_RECURSIVE_PORT}"
      --upstream "127.0.0.1:${DNS_LISTEN_PORT}"
      --log "${RUN_DIR}/dns_recursive.jsonl"
    )
    local proxy_authoritative_args=(
      --listen "127.0.0.1:${PROXY_AUTHORITATIVE_PORT}"
      --upstream "127.0.0.1:${DNS_LISTEN_PORT}"
      --log "${RUN_DIR}/dns_authoritative.jsonl"
    )
    if [[ -n "${PROXY_DELAY_MS}" ]]; then
      proxy_recursive_args+=(--delay-ms "${PROXY_DELAY_MS}" --dist "${PROXY_DIST}")
      proxy_authoritative_args+=(--delay-ms "${PROXY_DELAY_MS}" --dist "${PROXY_DIST}")
      if [[ -n "${PROXY_JITTER_MS}" ]]; then
        proxy_recursive_args+=(--jitter-ms "${PROXY_JITTER_MS}")
        proxy_authoritative_args+=(--jitter-ms "${PROXY_JITTER_MS}")
      fi
      if [[ -n "${PROXY_REORDER_PROB}" ]]; then
        proxy_recursive_args+=(--reorder-rate "${PROXY_REORDER_PROB}")
        proxy_authoritative_args+=(--reorder-rate "${PROXY_REORDER_PROB}")
      fi
    fi
    "${ROOT_DIR}/target/release/slipstream-bench" udp-proxy \
      "${proxy_recursive_args[@]}" \
      >"${RUN_DIR}/udp_proxy_recursive.log" 2>&1 &
    PROXY_RECURSIVE_PID=$!

    "${ROOT_DIR}/target/release/slipstream-bench" udp-proxy \
      "${proxy_authoritative_args[@]}" \
      >"${RUN_DIR}/udp_proxy_authoritative.log" 2>&1 &
    PROXY_AUTHORITATIVE_PID=$!
  fi

  local mixed_download_mib_s=""
  local mixed_exfil_mib_s=""

  if [[ "${RUN_DOWNLOAD}" != "0" ]]; then
    if ! mixed_download_mib_s=$(run_client_bench \
      mixed_download \
      source \
      recv \
      --authoritative "${AUTHORITATIVE_ADDR}" \
      --resolver "${RECURSIVE_ADDR}"); then
      return 1
    fi
    if ! enforce_min_throughput "download" "${mixed_download_mib_s}" "${MIN_AVG_MIB_S_DOWNLOAD}"; then
      return 1
    fi
  fi

  if [[ "${RUN_EXFIL}" != "0" ]]; then
    if ! mixed_exfil_mib_s=$(run_client_bench \
      mixed_exfil \
      sink \
      send \
      --authoritative "${AUTHORITATIVE_ADDR}" \
      --resolver "${RECURSIVE_ADDR}"); then
      return 1
    fi
    if ! enforce_min_throughput "exfil" "${mixed_exfil_mib_s}" "${MIN_AVG_MIB_S_EXFIL}"; then
      return 1
    fi
  fi

  if [[ "${RUN_DOWNLOAD}" == "0" && "${RUN_EXFIL}" == "0" ]]; then
    echo "RUN_DOWNLOAD and RUN_EXFIL are both disabled; nothing to run." >&2
    return 1
  fi

  if [[ "${use_proxy}" == "1" ]]; then
    "${ROOT_DIR}/target/release/slipstream-bench" check-capture \
      --recursive-log "${RUN_DIR}/dns_recursive.jsonl" \
      --authoritative-log "${RUN_DIR}/dns_authoritative.jsonl"
  fi

  if [[ "${RUN_DOWNLOAD}" != "0" ]]; then
    report_throughput "download" "${mixed_download_mib_s}"
  fi
  if [[ "${RUN_EXFIL}" != "0" ]]; then
    report_throughput "exfil" "${mixed_exfil_mib_s}"
  fi

  echo "Interop mixed OK; logs in ${RUN_DIR}."
}

if [[ "${RESOLVER_MODE}" == "mixed" ]]; then
  run_mixed
  exit $?
fi

if [[ -z "${PROXY_DELAY_MS}" ]]; then
  setup_netem
fi

if [[ "${RUNS}" -le 1 ]]; then
  if [[ "${RUN_EXFIL}" -ne 0 ]]; then
    run_case "exfil" "sink" "send"
  fi
  if [[ "${RUN_DOWNLOAD}" -ne 0 ]]; then
    run_case "download" "source" "recv"
  fi
else
  for run_id in $(seq 1 "${RUNS}"); do
    if [[ "${RUN_EXFIL}" -ne 0 ]]; then
      run_case "run-${run_id}/exfil" "sink" "send" "${run_id}"
    fi
    if [[ "${RUN_DOWNLOAD}" -ne 0 ]]; then
      run_case "run-${run_id}/download" "source" "recv" "${run_id}"
    fi
  done
fi

enforce_min_avg
echo "Benchmarks OK; logs in ${RUN_DIR}."
