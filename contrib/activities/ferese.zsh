# Source explicitly from .zshrc; add-zsh-hook preserves other integrations.
[[ -o interactive ]] || return
[[ ${_ferese_installed:-} == 1 ]] && return
_ferese_installed=1
_ferese_sequence=0
autoload -Uz add-zsh-hook
_ferese_preexec() {
    local saved=$?
    ((_ferese_sequence+=1))
    _ferese_local="zsh-$$-$_ferese_sequence"
    feresectl activity hook "$_ferese_local" --command "${1[1,4096]}" --cwd "${PWD[1,1024]}" >/dev/null 2>&1
    return "$saved"
}
_ferese_precmd() {
    local saved=$?
    if [[ -n ${_ferese_local:-} ]]; then
        if (( saved == 130 || saved == 143 )); then
            feresectl activity cancel "$_ferese_local" >/dev/null 2>&1
        elif (( saved == 0 )); then
            feresectl activity end "$_ferese_local" --result ok >/dev/null 2>&1
        else
            feresectl activity end "$_ferese_local" --result fail >/dev/null 2>&1
        fi
        unset _ferese_local
    fi
    return "$saved"
}
add-zsh-hook preexec _ferese_preexec
add-zsh-hook precmd _ferese_precmd
