#!/usr/bin/env bash
# Claude Code status line, two lines:
#
#   ⬢ sandbox (kits) · dir · model (effort) · ctx N/W (P%)
#   Σ total (new N · sub N) · $cost · left: 5h P% (in 2h14) · 7d P% (in 4d3h)
#
# What each number means: docs/addons.md, *Status line inside the sandbox*. Portable between macOS and
# an sbx sandbox (Linux + uutils coreutils): no `tail -r`, no `tac`, no `stat`, no GNU-only flags.
set -u

input=$(cat)

if ! command -v jq >/dev/null 2>&1; then
  printf '? · ? · ctx ?'
  exit 0
fi

# ── colour ─────────────────────────────────────────────────────────────────────
if [ -n "${NO_COLOR:-}" ]; then
  R=""; DIM=""; BOLD=""; GREEN=""; YELLOW=""; RED=""; CYAN=""
else
  R=$'\033[0m'; DIM=$'\033[2m'; BOLD=$'\033[1m'
  GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RED=$'\033[31m'; CYAN=$'\033[36m'
fi
SEP="${DIM} · ${R}"

# Traffic light where BIG is bad (context fill), with the thresholds of the official example.
hot() {
  if   [ "$1" -ge 90 ]; then printf '%s' "$RED"
  elif [ "$1" -ge 70 ]; then printf '%s' "$YELLOW"
  else                       printf '%s' "$GREEN"; fi
}

# … and where SMALL is bad (limit left).
cold() {
  if   [ "$1" -le 10 ]; then printf '%s' "$RED"
  elif [ "$1" -le 25 ]; then printf '%s' "$YELLOW"
  else                       printf '%s' "$GREEN"; fi
}

# short <n>: 1234 -> 1k, 1234567 -> 1.2M.
short() {
  if   [ "$1" -ge 1000000 ]; then printf '%d.%dM' "$(($1 / 1000000))" "$((($1 % 1000000) / 100000))"
  elif [ "$1" -ge 1000 ];    then printf '%dk' "$(($1 / 1000))"
  else                            printf '%d' "$1"; fi
}

# bytes <file>: its size. `wc -c`, not `stat`: GNU and uutils read `stat -f` as --file-system and
# print to stdout before they fail. The digit filter drops the spaces macOS `wc` pads with.
bytes() {
  b=$(wc -c < "$1" 2>/dev/null)
  b=${b//[![:digit:]]/}
  printf '%s' "${b:-0}"
}

# ── everything Claude Code sends ───────────────────────────────────────────────
# One jq call: this runs on every assistant message. Fields are joined on US (\037), not a tab: a
# tab is IFS whitespace, so an empty field would shift every field after it. jq turns every float
# into an integer, so no locale with a decimal comma reaches a number on the bar.
sep=$(printf '\037')
IFS="$sep" read -r cwd model_id model_name effort transcript window tokens pct10 \
                   cents left5 reset5 left7 reset7 <<EOF
$(printf '%s' "$input" | jq -j '
    def tenths: if type == "number" then (. * 10 | round) else -1 end;
    # 100 minus used, floored: 99.6% used has to read as 0% left, not as a soothing 1%.
    def left:   if type == "number" then ((100 - .) | floor | if . < 0 then 0 else . end)
                else "" end;
    def pad2:   tostring | if length < 2 then "0" + . else . end;
    # Time to reset. Empty when the field is missing or already past — an expired counter
    # is a figure that no longer counts anything.
    def countdown:
      if type == "number" then ((. - now) | floor) as $d
        | if   $d <= 0     then ""
          elif $d >= 86400 then "\($d / 86400 | floor)d\(($d % 86400) / 3600 | floor)h"
          elif $d >= 3600  then "\($d / 3600 | floor)h\((($d % 3600) / 60 | floor) | pad2)"
          else                  "\(($d + 59) / 60 | floor)m" end
      else "" end;
    [ (.workspace.current_dir // .cwd // ""),
      (.model.id // ""),
      (.model.display_name // .model.id // "?"),
      (.effort.level // ""),
      (.transcript_path // ""),
      (.context_window.context_window_size // 0),
      (.context_window.total_input_tokens // 0),
      (.context_window.used_percentage | tenths),
      ((.cost.total_cost_usd // 0) * 100 | round),
      (.rate_limits.five_hour.used_percentage  | left),
      (.rate_limits.five_hour.resets_at        | countdown),
      (.rate_limits.seven_day.used_percentage  | left),
      (.rate_limits.seven_day.resets_at        | countdown)
    ] | map(tostring) | join("\u001f")' 2>/dev/null)
EOF

# A field that is not a plain digit string becomes 0, or "" where absent must differ from zero:
# `[ "" -gt 0 ]` would print an error into the bar.
case "${window:-}" in ''|*[!0-9]*) window=0 ;; esac
case "${tokens:-}" in ''|*[!0-9]*) tokens=0 ;; esac
case "${cents:-}"  in ''|*[!0-9]*) cents=0  ;; esac
case "${pct10:-}"  in ''|*[!0-9]*) pct10=-1 ;; esac
case "${left5:-}"  in ''|*[!0-9]*) left5="" ;; esac
case "${left7:-}"  in ''|*[!0-9]*) left7="" ;; esac
[ -n "${model_name:-}" ] || model_name="?"

[ -n "${effort:-}" ] && model_name="$model_name ($effort)"

# ── sandbox ────────────────────────────────────────────────────────────────────
# The kits come from their SBX_KIT_<NAME>=1 markers (kits/statusline/statusline.dockerfile).
sandbox=""
if [ -n "${SANDBOX_NAME:-}" ] || [ "${IS_SANDBOX:-0}" = "1" ]; then
  sbx_name="${SANDBOX_NAME:-$(hostname 2>/dev/null || echo sandbox)}"
  # The label that the dashboard's pre-start step writes, else the sbx name.
  shown="$sbx_name"
  if [ -r "${HOME:-}/.claude/sandbox-label" ]; then
    label=$(cat "${HOME:-}/.claude/sandbox-label" 2>/dev/null)
    [ -n "$label" ] && shown="$label"
  fi
  kits=""
  for k in $(env | sed -n 's/^SBX_KIT_\([A-Za-z0-9_]*\)=1$/\1/p' \
    | tr '[:upper:]_' '[:lower:]-' | sort); do
    # Skip this kit and the backend kit, which the sandbox name already spells out.
    [ "$k" = "statusline" ] && continue
    case "$sbx_name" in "$k" | "$k"-*) continue ;; esac
    kits="${kits}${kits:+ }$k"
  done
  if [ -n "$kits" ]; then
    sandbox="${BOLD}⬢ ${shown}${R} ${DIM}(${kits})${R}${SEP}"
  else
    sandbox="${BOLD}⬢ ${shown}${R}${SEP}"
  fi
fi

# ── directory ──────────────────────────────────────────────────────────────────
home="${HOME:-}"; home="${home%/}"
if [ -z "$cwd" ]; then
  dir="?"
elif [ "$cwd" = "$home" ]; then
  dir="~"
else
  dir=${cwd##*/}
fi

# ── context window ─────────────────────────────────────────────────────────────
# context_window_size is authoritative (1000000 for [1m] models); fall back to the
# model id only if the field is missing.
if [ "$window" -le 0 ]; then
  case "$model_id" in
    *"[1m]"*) window=1000000 ;;
    *)        window=200000  ;;
  esac
fi
case "$window" in
  1000000) window_label="1M" ;;
  *)       window_label="$((window / 1000))k" ;;
esac

# Fallback for the first render of a resumed session: newest usage record in the
# transcript tail. Reversed with awk, since `tail -r` is BSD-only and `tac` is GNU-only.
if [ "$tokens" -le 0 ] && [ -n "$transcript" ] && [ -f "$transcript" ]; then
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    t=$(printf '%s' "$line" | jq -r '
      (.message.usage // .usage // null) as $u
      | if $u == null then empty
        else (($u.input_tokens // 0)
              + ($u.cache_read_input_tokens // 0)
              + ($u.cache_creation_input_tokens // 0))
        end
    ' 2>/dev/null)
    if [ -n "$t" ] && [ "$t" -gt 0 ] 2>/dev/null; then
      tokens="$t"
      pct10=-1
      break
    fi
  done <<EOF
$(tail -c 1048576 "$transcript" 2>/dev/null | grep -F '"usage"' \
  | awk '{ a[NR] = $0 } END { for (i = NR; i >= 1 && i > NR - 5; i--) print a[i] }')
EOF
fi

# ── session total ──────────────────────────────────────────────────────────────
# Subagents write beside the main transcript, under <session>/subagents/*.jsonl, so all files are
# summed. The result is cached against a fingerprint of their sizes.
total=0      # Σ, subagents included
total_new=0  # of it, genuinely computed
total_sub=0  # of it, the subagents' own Σ
cache_dir="${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}/claude-statusline"

# sum_file <file>: "new cache_read". jq emits a row per usage record and awk adds them up, not one
# `reduce`: a torn last line of the live file then costs only the rows after it.
sum_file() {
  jq -r 'select(.message? | type == "object") | .message.usage // empty
         | [ ((.input_tokens // 0) + (.cache_creation_input_tokens // 0) + (.output_tokens // 0)),
             (.cache_read_input_tokens // 0) ]
         | @tsv' "$1" 2>/dev/null \
    | awk '{ n += $1; c += $2 } END { print (n + 0), (c + 0) }'
}

if [ -n "$transcript" ] && [ -f "$transcript" ]; then
  session=${transcript##*/}; session=${session%.jsonl}
  subdir="${transcript%/*}/$session/subagents"

  fp=$(bytes "$transcript")
  if [ -d "$subdir" ]; then
    for s in "$subdir"/*.jsonl; do
      [ -f "$s" ] || continue
      fp=$((fp + $(bytes "$s")))
    done
  fi

  cf="$cache_dir/$session"
  fp_old=""
  if [ -f "$cf" ]; then
    read -r fp_old total total_new total_sub < "$cf" 2>/dev/null || true
    # Each field on its own: a cache file of an older version has three fields, and an empty
    # fourth would print an error into the bar.
    for v in "${fp_old:-}" "${total:-}" "${total_new:-}" "${total_sub:-}"; do
      case "$v" in
        ''|*[!0-9]*) fp_old=""; total=0; total_new=0; total_sub=0; break ;;
      esac
    done
  fi

  if [ "$fp_old" != "$fp" ]; then
    v=$(sum_file "$transcript"); main_new=${v%% *}; main_read=${v##* }
    sub_new=0; sub_read=0
    if [ -d "$subdir" ]; then
      for s in "$subdir"/*.jsonl; do
        [ -f "$s" ] || continue
        v=$(sum_file "$s")
        sub_new=$((sub_new + ${v%% *}))
        sub_read=$((sub_read + ${v##* }))
      done
    fi
    total=$((main_new + main_read + sub_new + sub_read))
    total_new=$((main_new + sub_new))
    total_sub=$((sub_new + sub_read))
    if mkdir -p "$cache_dir" 2>/dev/null; then
      # The redirection is inside the group: the shell, not printf, reports a read-only cache dir.
      { printf '%s %s %s %s\n' "$fp" "$total" "$total_new" "$total_sub" > "$cf"; } 2>/dev/null || true
      # Only on a rescan, not on every redraw: last month's sessions are gone anyway.
      find "$cache_dir" -type f -mtime +30 -delete 2>/dev/null || true
    fi
  fi
fi

# ── line 1: where you are ──────────────────────────────────────────────────────
if [ "$tokens" -gt 0 ]; then
  [ "$pct10" -ge 0 ] || pct10=$(( (tokens * 1000 + window / 2) / window ))
  ctx_disp="$(hot $((pct10 / 10)))$(short "$tokens")/${window_label} ($((pct10 / 10)).$((pct10 % 10))%)${R}"
else
  ctx_disp="${DIM}0/${window_label}${R}"
fi

line1="${sandbox}${BOLD}${CYAN}${dir}${R}${SEP}${DIM}${model_name}${R}${SEP}${DIM}ctx${R} ${ctx_disp}"

# ── line 2: what it has cost ───────────────────────────────────────────────────
line2=""
add() {
  if [ -z "$line2" ]; then line2="$1"; else line2="${line2}${SEP}$1"; fi
}

if [ "$total" -gt 0 ]; then
  piece="${DIM}Σ${R} $(short "$total")"
  shares=""
  [ "$total_new" -gt 0 ] && shares="${DIM}new${R} $(short "$total_new")"
  if [ "$total_sub" -gt 0 ]; then
    [ -n "$shares" ] && shares="${shares}${DIM} · ${R}"
    shares="${shares}${DIM}sub${R} $(short "$total_sub")"
  fi
  [ -n "$shares" ] && piece="$piece ${DIM}(${R}${shares}${DIM})${R}"
  add "$piece"
fi

[ "$cents" -gt 0 ] && add "$(printf '$%d.%02d' "$((cents / 100))" "$((cents % 100))")"

limits=""
if [ -n "$left5" ]; then
  limits="${DIM}5h${R} $(cold "$left5")${left5}%${R}"
  [ -n "$reset5" ] && limits="${limits} ${DIM}(in ${reset5})${R}"
fi
if [ -n "$left7" ]; then
  [ -n "$limits" ] && limits="${limits}${SEP}"
  limits="${limits}${DIM}7d${R} $(cold "$left7")${left7}%${R}"
  [ -n "$reset7" ] && limits="${limits} ${DIM}(in ${reset7})${R}"
fi
# rate_limits exists only on a subscription, and each window can be absent on its own.
[ -n "$limits" ] && add "${DIM}left:${R} $limits"

if [ -n "$line2" ]; then
  printf '%s\n%s' "$line1" "$line2"
else
  printf '%s' "$line1"
fi
