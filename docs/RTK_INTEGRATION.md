# RTK shell-output integration

Jcode can optionally route supported `bash` commands through [RTK](https://github.com/rtk-ai/rtk), a command-aware output compressor. The integration is disabled by default and RTK remains a separately installed executable.

## Enable it

```toml
[tools.bash]
output_backend = "rtk"
rtk_binary = "rtk" # or an absolute path
rtk_rewrite_timeout_ms = 500
```

Equivalent environment overrides:

```bash
export JCODE_BASH_OUTPUT_BACKEND=rtk
export JCODE_RTK_BINARY=/path/to/rtk
export JCODE_RTK_REWRITE_TIMEOUT_MS=500
```

## Behavior

Before executing a shell command, Jcode asks `rtk rewrite` whether the command has a supported RTK equivalent. Supported commands are rewritten and their output is compressed by RTK. Unsupported commands, a missing RTK executable, invalid UTF-8, and rewrite timeouts all fail open to the original command.

RTK rewrite exit code `3` carries a rewrite plus a Claude Code permission hint. Claude Code's permission model is not available inside Jcode, so Jcode treats outputs from exit `0` and `3` only as rewrite candidates and applies its own destructive-command gate again to the transformed command before execution. RTK passthrough (`1`) and deny (`2`) responses remain raw.

The destructive-command gate always evaluates the original command before any rewrite. On Unix, commands mentioning `cargo` inside repositories that provide `scripts/dev_cargo.sh` remain on that wrapper path so build policy and timing telemetry are not bypassed.

A successful rewrite adds these tool-result metadata fields:

- `bash_output_backend = "rtk"`
- `original_command`
- `rewritten_command`

To bypass RTK for one call and inspect ordinary command output, set `raw_output = true` on the `bash` tool call. RTK may also preserve its own full failure output according to RTK configuration.

## Operational considerations

- RTK tracks its own output-savings data and may have separate telemetry/configuration. Review RTK's settings before enabling it.
- Semantic compression can hide nonessential-looking lines that later prove useful. Use `raw_output = true` when diagnosing ambiguous failures.
- Jcode does not install or update RTK. Pin and manage RTK independently if reproducible behavior is required.
