# Zsh navigation wrapper and completion for wt.

wt() {
    case "${1-}" in
        config|repo|shell-init|worktree|help|__complete|-h|--help|-V|--version)
            command wt "$@"
            return $?
            ;;
    esac

    local wt_destination wt_status
    wt_destination="$(command wt "$@")"
    wt_status=$?
    if [ "$wt_status" -ne 0 ]; then
        return "$wt_status"
    fi
    if [ -n "$wt_destination" ]; then
        builtin cd -- "$wt_destination"
    fi
}

_wt_complete() {
    local -a wt_candidates
    wt_candidates=("${(@f)$(command wt __complete "${words[@]:1}")}")
    compadd -Q -a wt_candidates
}

if (( ! $+functions[compdef] )); then
    autoload -Uz compinit
    compinit
fi
compdef _wt_complete wt
