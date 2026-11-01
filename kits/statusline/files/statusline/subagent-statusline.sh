#!/usr/bin/env bash
# Row body for the subagents in the panel under the prompt. Replaces the default
#   name · description · token count
# with
#   Wegwerf-Auftrag · ctx 15k/1M (1%) · 0:35 · ≈26k/min · ▇▃▁▂▁▁▁
#
# Input: ONE JSON object on stdin with .columns (usable width) and .tasks[]. Output: one
# JSON line per row to override, {"id": …, "content": …}; content is rendered as-is, ANSI
# included. A task with no output line keeps the default rendering.
set -u

input=$(cat)

command -v jq >/dev/null 2>&1 || exit 0

# The images set no locale, so bash would count and cut bytes, not characters. C.UTF-8 exists on
# the images and on macOS.
export LC_ALL=C.UTF-8

if [ -n "${NO_COLOR:-}" ]; then
  R=""; DIM=""; BOLD=""; GREEN=""; YELLOW=""; RED=""; CYAN=""
else
  R=$'\033[0m'; DIM=$'\033[2m'; BOLD=$'\033[1m'
  GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RED=$'\033[31m'; CYAN=$'\033[36m'
fi

columns=$(printf '%s' "$input" | jq -r '.columns // empty' 2>/dev/null)
case "${columns:-}" in ''|*[!0-9]*) columns=80 ;; esac
[ "$columns" -lt 20 ] && columns=20

now_ms=$(( $(date +%s 2>/dev/null || echo 0) * 1000 ))

# Traffic light where BIG is bad, as in the main bar.
hot() {
  if   [ "$1" -ge 90 ]; then printf '%s' "$RED"
  elif [ "$1" -ge 70 ]; then printf '%s' "$YELLOW"
  else                       printf '%s' "$GREEN"; fi
}

short() {
  if   [ "$1" -ge 1000000 ]; then printf '%d.%dM' "$(($1 / 1000000))" "$((($1 % 1000000) / 100000))"
  elif [ "$1" -ge 1000 ];    then printf '%dk' "$(($1 / 1000))"
  else                            printf '%d' "$1"; fi
}

# elapsed <startTime>: the seconds since then. It reads milliseconds, seconds and ISO-8601, and
# drops a negative value or one over a day.
elapsed() {
  st="$1"
  [ -n "$st" ] || return 0
  [ "$now_ms" -gt 0 ] || return 0
  case "$st" in
    *[!0-9]*)
      k=${st%%.*}; k=${k%Z}
      # An assignment, not $( a || b ): the BSD and GNU spellings differ, and the loser can print
      # its usage text to stdout.
      s=$(date -j -u -f '%Y-%m-%dT%H:%M:%S' "$k" +%s 2>/dev/null) || s=""
      [ -n "$s" ] || s=$(date -d "$st" +%s 2>/dev/null) || return 0
      case "${s:-}" in ''|*[!0-9]*) return 0 ;; esac
      ms=$(( s * 1000 ))
      ;;
    *)
      if [ "${#st}" -ge 13 ]; then ms="$st"; else ms=$(( st * 1000 )); fi
      ;;
  esac
  d=$(( (now_ms - ms) / 1000 ))
  [ "$d" -ge 0 ] && [ "$d" -lt 86400 ] || return 0
  printf '%s' "$d"
}

clock() {
  if [ "$1" -ge 3600 ]; then
    printf '%d:%02d:%02d' "$(($1 / 3600))" "$((($1 % 3600) / 60))" "$(($1 % 60))"
  else
    printf '%d:%02d' "$(($1 / 60))" "$(($1 % 60))"
  fi
}

# spark <samples>: the increments of tokenSamples, not the cumulative series, which only rises. A
# picture and no number: the series holds only the last 16 ticks.
spark() {
  awk -v s="$1" 'BEGIN {
    z[1]="▁"; z[2]="▂"; z[3]="▃"; z[4]="▄"
    z[5]="▅"; z[6]="▆"; z[7]="▇"; z[8]="█"
    n = split(s, a, ",")
    if (n < 3) exit
    m = 0
    for (i = 2; i <= n; i++) { d[i-1] = a[i] - a[i-1]; if (d[i-1] > m) m = d[i-1] }
    last = n - 1
    from = last - 7; if (from < 1) from = 1
    if (m <= 0) m = 1
    for (i = from; i <= last; i++) {
      k = int(d[i] * 7 / m) + 1
      if (k < 1) k = 1
      if (k > 8) k = 8
      printf "%s", z[k]
    }
  }'
}

# pace <id> <tokens>: tokens per minute from this script's own timestamps. Not from tokenSamples:
# a 16-slot ring buffer, so "runtime / samples" drifts low after the sixteenth tick.
cache_dir="${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}/claude-statusline"
RATE_WINDOW=60   # how far back the pace is measured
RATE_MIN_SPAN=8  # shorter spans are too noisy to report
RATE_IDLE_AFTER=20  # this long without growth means: standing still

pace() {
  kid=${1//[^A-Za-z0-9_-]/}
  [ -n "$kid" ] || return 0
  [ "$now_ms" -gt 0 ] || return 0
  now_s=$(( now_ms / 1000 ))
  cf="$cache_dir/agent-$kid"

  if [ -f "$cf" ]; then
    old=$(cat "$cf" 2>/dev/null)
  else
    old=""
    # Old traces go on a task's first tick only, not on every tick.
    mkdir -p "$cache_dir" 2>/dev/null \
      && find "$cache_dir" -name 'agent-*' -type f -mtime +1 -delete 2>/dev/null || true
  fi

  fresh=""
  for point in $old; do
    pt=${point%%:*}; pv=${point#*:}
    case "$pt" in ''|*[!0-9]*) continue ;; esac
    case "$pv" in ''|*[!0-9]*) continue ;; esac
    [ $(( now_s - pt )) -le "$RATE_WINDOW" ] || continue
    fresh="${fresh}${fresh:+ }$pt:$pv"
  done
  fresh="${fresh}${fresh:+ }$now_s:$2"
  # The redirection is inside the group: the shell, not printf, reports a read-only cache dir.
  { printf '%s\n' "$fresh" > "$cf"; } 2>/dev/null || true

  first=${fresh%% *}; last=${fresh##* }
  t0=${first%%:*}; v0=${first#*:}
  t1=${last%%:*};  v1=${last#*:}
  span=$(( t1 - t0 ))
  [ "$span" -ge "$RATE_MIN_SPAN" ] || return 0
  dv=$(( v1 - v0 ))
  [ "$dv" -ge 0 ] || return 0
  if [ "$dv" -eq 0 ]; then
    [ "$span" -ge "$RATE_IDLE_AFTER" ] && printf 'idle'
    return 0
  fi
  printf '≈%s/min' "$(short "$(( dv * 60 / span ))")"
}

clip() {
  [ "$2" -le 0 ] && return 0
  if [ "${#1}" -le "$2" ]; then
    printf '%s' "$1"
  else
    printf '%s…' "${1:0:$(($2 - 1))}"
  fi
}

# Fields are joined on U+001F, not a tab: a tab is IFS whitespace, so an empty field would shift
# the rest. Control characters become spaces first, or a description could forge a line or a
# separator.
printf '%s' "$input" \
  | jq -r 'def clean: tostring | gsub("[[:cntrl:]]"; " ");
           .tasks[]? | [ (.id // "" | clean),
                         (.label // .name // "" | clean),
                         (.status // "" | clean),
                         (.tokenCount // 0 | clean),
                         (.contextWindowSize // 0 | clean),
                         (.startTime // "" | clean),
                         (.description // "" | clean),
                         (.tokenSamples // [] | map(tostring) | join(",")) ]
                       | join("\u001f")' 2>/dev/null \
  | while IFS=$'\037' read -r id name status tok window start description samples; do
      [ -n "$id" ] || continue
      case "$tok" in    ''|*[!0-9]*) tok=0 ;; esac
      case "$window" in ''|*[!0-9]*) window=0 ;; esac

      # In the measured payload the label equalled the description: show it once.
      [ "$description" = "$name" ] && description=""

      # Without a resolved model there is no contextWindowSize: then the bare number, no percentage.
      if [ "$window" -gt 0 ]; then
        case "$window" in
          1000000) window_label="1M" ;;
          *)       window_label="$((window / 1000))k" ;;
        esac
        p=$((tok * 100 / window))
        ctx_plain="ctx $(short "$tok")/${window_label} (${p}%)"
        ctx="${DIM}ctx${R} $(hot "$p")$(short "$tok")/${window_label} (${p}%)${R}"
      else
        ctx_plain="ctx $(short "$tok")"
        ctx="${DIM}ctx${R} $(short "$tok")"
      fi

      # Runtime, pace and history only while the task runs: a finished task has no end time in
      # the payload, so its state stands there instead.
      time=""; rate=""; picture=""; state=""; state_colour="$GREEN"
      case "$status" in
        running|"")
          secs=$(elapsed "$start")
          [ -n "$secs" ] && time=$(clock "$secs")
          rate=$(pace "$id" "$tok")
          picture=$(spark "$samples")
          ;;
        completed) state="✓" ;;
        # The states of the bundle's state machine; any other passes through unchanged.
        failed)         state="failed";           state_colour="$RED" ;;
        cancelled)      state="cancelled";        state_colour="$YELLOW" ;;
        input_required) state="waiting on input"; state_colour="$YELLOW" ;;
        *)              state="$status";          state_colour="$DIM" ;;
      esac

      # Share out the width: when the fixed parts do not fit, the picture goes first, then the
      # pace, then the clock. The fill always stays.
      fixed() {
        f=$(( ${#ctx_plain} + 3 ))
        [ -n "$name" ]    && f=$(( f + ${#name} + 3 ))
        [ -n "$time" ]    && f=$(( f + ${#time} + 3 ))
        [ -n "$rate" ]    && f=$(( f + ${#rate} + 3 ))
        [ -n "$picture" ] && f=$(( f + ${#picture} + 3 ))
        [ -n "$state" ]   && f=$(( f + ${#state} + 3 ))
        printf '%s' "$f"
      }
      [ "$(fixed)" -gt "$columns" ] && picture=""
      [ "$(fixed)" -gt "$columns" ] && rate=""
      [ "$(fixed)" -gt "$columns" ] && time=""
      room=$(( columns - $(fixed) ))

      # Piece by piece, so an empty name leaves no leading separator.
      line=""
      add() {
        if [ -z "$line" ]; then line="$1"; else line="${line}${DIM} · ${R}$1"; fi
      }
      [ -n "$name" ] && add "${BOLD}${CYAN}${name}${R}"
      # Under eight characters a description is only a stub: then none.
      if [ -n "$description" ] && [ "$room" -ge 8 ]; then
        add "${DIM}$(clip "$description" "$room")${R}"
      fi
      add "$ctx"
      [ -n "$time" ]    && add "${DIM}${time}${R}"
      [ -n "$rate" ]    && add "${DIM}${rate}${R}"
      [ -n "$picture" ] && add "${DIM}${picture}${R}"
      [ -n "$state" ]   && add "${state_colour}${state}${R}"

      jq -cn --arg id "$id" --arg content "$line" '{id: $id, content: $content}'
    done
