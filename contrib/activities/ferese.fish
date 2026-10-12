# Source explicitly from config.fish. Named event handlers compose with others.
status is-interactive; or return
set -q __ferese_installed; and return
set -g __ferese_installed 1
set -g __ferese_sequence 0
function __ferese_preexec --on-event fish_preexec
    set -l saved $status
    set -g __ferese_sequence (math $__ferese_sequence + 1)
    set -g __ferese_local "fish-$fish_pid-$__ferese_sequence"
    feresectl activity hook "$__ferese_local" --command (string sub -l 4096 -- "$argv[1]") --cwd (string sub -l 1024 -- "$PWD") >/dev/null 2>&1
    return $saved
end
function __ferese_postexec --on-event fish_postexec
    set -l saved $status
    if set -q __ferese_local
        if test $saved -eq 130; or test $saved -eq 143
            feresectl activity cancel "$__ferese_local" >/dev/null 2>&1
        else if test $saved -eq 0
            feresectl activity end "$__ferese_local" --result ok >/dev/null 2>&1
        else
            feresectl activity end "$__ferese_local" --result fail >/dev/null 2>&1
        end
        set -e __ferese_local
    end
    return $saved
end
