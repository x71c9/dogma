use crate::cli::CompletionShell;

pub fn run(shell: &CompletionShell) {
  match shell {
    CompletionShell::Bash => print!("{}", BASH),
    CompletionShell::Zsh => print!("{}", ZSH),
    CompletionShell::Fish => print!("{}", FISH),
  }
}

// ---------------------------------------------------------------------------
// Bash
// ---------------------------------------------------------------------------

const BASH: &str = r##"
_dogma_list_envs() {
    dogma --list-envs 2>/dev/null
}

_dogma_list_units() {
    dogma --list-units 2>/dev/null
}

_dogma_list_hosts() {
    dogma --list-hosts 2>/dev/null
}

_dogma_list_pipelines() {
    dogma --list-pipelines 2>/dev/null
}

_dogma_completions() {
    local cur prev words cword
    _init_completion || return

    local commands="credentials env output shell deploy infra completions"

    if [[ $cword -eq 1 ]]; then
        COMPREPLY=($(compgen -W "$commands --time --help --version" -- "$cur"))
        return
    fi

    local cmd="${words[1]}"

    case "$cmd" in
        credentials|env|shell)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=($(compgen -W "$(_dogma_list_envs)" -- "$cur"))
            fi
            ;;
        output)
            case $cword in
                2) COMPREPLY=($(compgen -W "$(_dogma_list_envs)" -- "$cur")) ;;
                3) COMPREPLY=($(compgen -W "$(_dogma_list_units)" -- "$cur")) ;;
            esac
            ;;
        deploy)
            # If dogma.yml declares no pipelines, the pipeline-name arg is
            # skipped and `deploy` takes just <env> [<host>].
            local pipelines env_word host_word
            pipelines="$(_dogma_list_pipelines)"
            if [[ -n "$pipelines" ]]; then
                env_word=3
                host_word=4
            else
                env_word=2
                host_word=3
            fi

            if [[ "$cur" == -* ]]; then
                COMPREPLY=($(compgen -W "--new --latest --version --skip-infra --skip-sops --refetch -m" -- "$cur"))
            elif [[ -n "$pipelines" && $cword -eq 2 ]]; then
                COMPREPLY=($(compgen -W "$pipelines" -- "$cur"))
            elif [[ $cword -eq $env_word ]]; then
                COMPREPLY=($(compgen -W "$(_dogma_list_envs)" -- "$cur"))
            elif [[ $cword -eq $host_word ]]; then
                COMPREPLY=($(compgen -W "$(_dogma_list_hosts)" -- "$cur"))
            else
                COMPREPLY=($(compgen -W "--new --latest --version --skip-infra --skip-sops --refetch -m" -- "$cur"))
            fi
            ;;
        infra)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=($(compgen -W "apply destroy init auth" -- "$cur"))
            elif [[ $cword -eq 3 ]]; then
                COMPREPLY=($(compgen -W "$(_dogma_list_envs)" -- "$cur"))
            elif [[ $cword -eq 4 ]]; then
                COMPREPLY=($(compgen -W "$(_dogma_list_units)" -- "$cur"))
            elif [[ $cword -ge 5 ]]; then
                COMPREPLY=($(compgen -W "--migrate-state --upgrade" -- "$cur"))
            fi
            ;;
        completions)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=($(compgen -W "bash zsh fish" -- "$cur"))
            fi
            ;;
    esac
}

complete -F _dogma_completions dogma
"##;

// ---------------------------------------------------------------------------
// Zsh
// ---------------------------------------------------------------------------

const ZSH: &str = r##"
#compdef dogma

_dogma_envs() {
    local -a envs
    envs=(${(f)"$(dogma --list-envs 2>/dev/null)"})
    _describe 'environment' envs
}

_dogma_units() {
    local -a units
    units=(${(f)"$(dogma --list-units 2>/dev/null)"})
    _describe 'unit' units
}

_dogma_hosts() {
    local -a hosts
    hosts=(${(f)"$(dogma --list-hosts 2>/dev/null)"})
    _describe 'host' hosts
}

_dogma_pipelines() {
    local -a pipelines
    pipelines=(${(f)"$(dogma --list-pipelines 2>/dev/null)"})
    _describe 'pipeline' pipelines
}

_dogma() {
    local state

    _arguments \
        '--time[print elapsed ms after command]' \
        '--help[show help]' \
        '--version[show version]' \
        '1: :->cmd' \
        '*: :->args'

    case $state in
        cmd)
            _values 'command' \
                'credentials[print export statements for infra credentials]' \
                'env[print export statements for all secrets]' \
                'output[print cached infra outputs]' \
                'shell[spawn a shell with infra credentials]' \
                'deploy[deploy to hosts]' \
                'infra[infra management]' \
                'completions[print shell completion script]'
            ;;
        args)
            case $words[2] in
                credentials|env|shell)
                    _arguments '2: :_dogma_envs'
                    ;;
                output)
                    _arguments \
                        '2: :_dogma_envs' \
                        '3: :_dogma_units' \
                        '4: :_message "output key"'
                    ;;
                deploy)
                    # If dogma.yml declares no pipelines, the pipeline-name
                    # arg is skipped and `deploy` takes just <env> [<host>].
                    if [[ -n "$(dogma --list-pipelines 2>/dev/null)" ]]; then
                        _arguments \
                            '2: :_dogma_pipelines' \
                            '3: :_dogma_envs' \
                            '4: :_dogma_hosts' \
                            '--new[create new version]' \
                            '--latest[use latest tag]' \
                            '--version[use specific tag]:tag' \
                            '--skip-infra[skip infra refresh]' \
                            '--skip-sops[skip sops regeneration]' \
                            '--refetch[clear caches]' \
                            '-m[commit message]:message'
                    else
                        _arguments \
                            '2: :_dogma_envs' \
                            '3: :_dogma_hosts' \
                            '--new[create new version]' \
                            '--latest[use latest tag]' \
                            '--version[use specific tag]:tag' \
                            '--skip-infra[skip infra refresh]' \
                            '--skip-sops[skip sops regeneration]' \
                            '--refetch[clear caches]' \
                            '-m[commit message]:message'
                    fi
                    ;;
                infra)
                    _arguments \
                        '2: :(apply destroy init auth)' \
                        '3: :_dogma_envs' \
                        '4: :_dogma_units' \
                        '--migrate-state[pass -migrate-state to init]' \
                        '--upgrade[pass -upgrade to init]'
                    ;;
                completions)
                    _arguments '2: :(bash zsh fish)'
                    ;;
            esac
            ;;
    esac
}

_dogma "$@"
"##;

// ---------------------------------------------------------------------------
// Fish
// ---------------------------------------------------------------------------

const FISH: &str = r##"
function __dogma_envs
    dogma --list-envs 2>/dev/null
end

function __dogma_units
    dogma --list-units 2>/dev/null
end

function __dogma_hosts
    dogma --list-hosts 2>/dev/null
end

function __dogma_pipelines
    dogma --list-pipelines 2>/dev/null
end

function __dogma_has_pipelines
    set -l pipelines (__dogma_pipelines)
    test (count $pipelines) -gt 0
end

set -l cmds credentials env output shell deploy infra completions

# top-level subcommands
complete -c dogma -f -n "not __fish_seen_subcommand_from $cmds" -a credentials -d 'print infra credential exports'
complete -c dogma -f -n "not __fish_seen_subcommand_from $cmds" -a env         -d 'print all secret exports'
complete -c dogma -f -n "not __fish_seen_subcommand_from $cmds" -a output       -d 'print cached infra outputs'
complete -c dogma -f -n "not __fish_seen_subcommand_from $cmds" -a shell        -d 'spawn credentialed shell'
complete -c dogma -f -n "not __fish_seen_subcommand_from $cmds" -a deploy       -d 'deploy to hosts'
complete -c dogma -f -n "not __fish_seen_subcommand_from $cmds" -a infra        -d 'infra management'
complete -c dogma -f -n "not __fish_seen_subcommand_from $cmds" -a completions  -d 'print completion script'

# global flags
complete -c dogma -l time    -d 'print elapsed ms'
complete -c dogma -l help    -d 'show help'
complete -c dogma -l version -d 'show version'

# credentials / env / shell: <env>
for subcmd in credentials env shell
    complete -c dogma -f -n "__fish_seen_subcommand_from $subcmd" \
        -a "(__dogma_envs)" -d 'environment'
end

# output: <env> then <unit>
complete -c dogma -f -n "__fish_seen_subcommand_from output; and test (count (commandline -opc)) -eq 2" \
    -a "(__dogma_envs)" -d 'environment'
complete -c dogma -f -n "__fish_seen_subcommand_from output; and test (count (commandline -opc)) -eq 3" \
    -a "(__dogma_units)" -d 'unit'

# deploy: [<pipeline>] <env> then optional <host>, then flags.
# If dogma.yml declares no pipelines, the pipeline-name arg is skipped and
# `deploy` takes just <env> [<host>].
complete -c dogma -f -n "__fish_seen_subcommand_from deploy; and __dogma_has_pipelines; and test (count (commandline -opc)) -eq 2" \
    -a "(__dogma_pipelines)" -d 'pipeline'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy; and __dogma_has_pipelines; and test (count (commandline -opc)) -eq 3" \
    -a "(__dogma_envs)" -d 'environment'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy; and __dogma_has_pipelines; and test (count (commandline -opc)) -eq 4" \
    -a "(__dogma_hosts)" -d 'host'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy; and not __dogma_has_pipelines; and test (count (commandline -opc)) -eq 2" \
    -a "(__dogma_envs)" -d 'environment'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy; and not __dogma_has_pipelines; and test (count (commandline -opc)) -eq 3" \
    -a "(__dogma_hosts)" -d 'host'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy" -l new          -d 'create new version'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy" -l latest       -d 'use latest tag'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy" -l version      -d 'use specific tag'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy" -l skip-infra   -d 'skip infra refresh'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy" -l skip-sops    -d 'skip sops regen'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy" -l refetch      -d 'clear caches'
complete -c dogma -f -n "__fish_seen_subcommand_from deploy" -s m            -d 'commit message'

# infra: apply/destroy/init/auth, then <env>, then <unit>
complete -c dogma -f -n "__fish_seen_subcommand_from infra; and test (count (commandline -opc)) -eq 2" \
    -a "apply destroy init auth"
complete -c dogma -f -n "__fish_seen_subcommand_from infra; and test (count (commandline -opc)) -eq 3" \
    -a "(__dogma_envs)" -d 'environment'
complete -c dogma -f -n "__fish_seen_subcommand_from infra; and test (count (commandline -opc)) -eq 4" \
    -a "(__dogma_units)" -d 'unit'
complete -c dogma -f -n "__fish_seen_subcommand_from infra" -l migrate-state -d 'migrate state'
complete -c dogma -f -n "__fish_seen_subcommand_from infra" -l upgrade -d 'upgrade providers'

# completions: shell name
complete -c dogma -f -n "__fish_seen_subcommand_from completions" -a "bash zsh fish"
"##;
