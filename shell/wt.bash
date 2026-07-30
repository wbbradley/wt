# Bash 3.2-compatible navigation wrapper and completion for wt.

wt() {
    case "${1-}" in
        repo|shell-init|worktree|help|__complete|-h|--help|-V|--version)
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
    local wt_candidate
    COMPREPLY=()
    while IFS= read -r wt_candidate; do
        if [ -n "$wt_candidate" ]; then
            COMPREPLY[${#COMPREPLY[@]}]="$wt_candidate"
        fi
    done < <(command wt __complete "${COMP_WORDS[@]:1}")
}

# Keep repository-qualified selectors as one completion word.
COMP_WORDBREAKS=${COMP_WORDBREAKS//:}
complete -F _wt_complete wt
