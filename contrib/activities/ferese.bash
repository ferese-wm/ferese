# Opt in by sourcing this file in an interactive Bash shell. Rules must also
# enable activities.sources.shell-hook. No command is executed by these hooks.
[[ $- == *i* ]] || return
[[ ${__ferese_installed:-} == 1 ]] && return
__ferese_installed=1
__ferese_sequence=0
__ferese_phase=prompt
_ferese_preexec() {
    local command=${1:-$BASH_COMMAND}
    [[ $__ferese_phase == armed && $command != _ferese_* ]] || return 0
    __ferese_phase=running
    ((__ferese_sequence+=1))
    __ferese_local="bash-$$-$__ferese_sequence"
    feresectl activity hook "$__ferese_local" --command "${command:0:4096}" --cwd "${PWD:0:1024}" >/dev/null 2>&1
    # bash-preexec restores the command status. A nonzero preexec return would
    # instead skip the user command when Bash extdebug is enabled.
    return 0
}
_ferese_finish() {
    local saved=$?
    __ferese_phase=prompt
    if [[ -n ${__ferese_local:-} ]]; then
        if ((saved == 130 || saved == 143)); then
            feresectl activity cancel "$__ferese_local" >/dev/null 2>&1
        elif ((saved == 0)); then
            feresectl activity end "$__ferese_local" --result ok >/dev/null 2>&1
        else
            feresectl activity end "$__ferese_local" --result fail >/dev/null 2>&1
        fi
        unset __ferese_local
    fi
    return "$saved"
}
_ferese_arm() { local saved=$?; __ferese_phase=armed; return "$saved"; }
if [[ -n ${bash_preexec_imported:-} ]] || declare -p preexec_functions >/dev/null 2>&1; then
    preexec_functions+=(_ferese_preexec)
    # bash-preexec restores the user status before each precmd callback, even
    # when its delayed installer rewrites PROMPT_COMMAND.
    precmd_functions+=(_ferese_finish _ferese_arm)
else
    # Bash DEBUG has a single owner and is not inherited reliably by sourced
    # files. The established bash-preexec arrays compose without replacing it.
    printf '%s\n' 'Ferese: load bash-preexec before this hook to preserve existing DEBUG integrations.' >&2
    unset __ferese_installed
    return
fi
