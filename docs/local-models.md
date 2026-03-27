# Local models

nerv can download and run GGUF models via llama-server.

## Workflow

```
nerv add <hf-repo> <quant>   # download GGUF
nerv load [alias]             # exec llama-server
nerv                          # connect via /model add local
```

## Download

`nerv add` resolves a GGUF file from HuggingFace:

1. Queries `huggingface.co/api/models/{repo}/tree/main` for file listing
2. Finds the `.gguf` file matching the quant pattern (case-insensitive)
3. Downloads with resume support (Range header) to `~/.nerv/models/`
4. Auto-detects hardware and writes recommended defaults to `~/.nerv/models.json`

## Hardware detection

On macOS, reads `hw.memsize` and `hw.physicalcpu` via sysctl.
Defaults are computed based on available memory after the model:

| Setting | Logic |
|---|---|
| Context length | Fill remaining memory with q8_0 KV cache, cap at 128k |
| GPU layers | 99 (all) if model fits in memory |
| KV cache type | q8_0 (half the memory of f16, negligible quality loss) |
| Batch size | 4096 if >6GB free, 2048 if >3GB, else 1024 |
| Micro-batch | batch_size / 4, min 256 |
| Gen threads | physical_cores - 2 (leave room for nerv + OS) |
| Batch threads | physical_cores (use all for prompt processing) |
| OS reserve | 4GB |

## llama-server args

Models are stored in `~/.nerv/models.json` (JSONC):

```jsonc
{
  "models": [
    {
      "alias": "qwen3.5-27b",
      "path": "~/.nerv/models/Qwen3.5-27B-UD-Q4_K_XL.gguf",
      "hf_repo": "unsloth/Qwen3.5-27B-GGUF",
      "context_length": 65536,
      "gpu_layers": 99,
      "port": 1234,
      "extra_args": [
        "-fa", "on",
        "--mlock",
        "-b", "4096",
        "-ub", "1024",
        "-t", "8",
        "-tb", "10",
        "-np", "1",
        "--host", "127.0.0.1",
        "--jinja",
        "-ctk", "q8_0",
        "-ctv", "q8_0",
        "-nkvo",
        "--no-context-shift",
        "--cache-reuse", "256"
      ]
    }
  ]
}
```

### Key args

| Arg | Purpose |
|---|---|
| `-fa on` | Flash attention |
| `--mlock` | Lock model in RAM (no swap) |
| `-ctk q8_0` / `-ctv q8_0` | Quantized KV cache (half memory vs f16) |
| `-nkvo` | No KV offloading to CPU |
| `--no-context-shift` | Don't silently shift context (fail explicitly) |
| `--cache-reuse 256` | Reuse KV cache when 256+ token prefix matches |
| `--jinja` | Use Jinja chat templates from GGUF metadata |
| `-np 1` | Single slot (one conversation at a time) |

## Loading

`nerv load [alias]` calls `exec()` (replaces the nerv process with
llama-server). This means the terminal stays attached to llama-server's
output. Use a separate terminal for nerv.

If no alias is given, loads the first model in `models.json`.

## Connecting

Once llama-server is running, nerv connects via `/model add local`
(which probes `http://localhost:1234/v1/models`). The connection is
also attempted automatically on startup if a `local` provider is
configured in `~/.nerv/config.json`.

## Health checks

On startup, nerv spawns a background thread that polls each custom
provider's `/models` endpoint every 5 seconds until it responds. The
footer shows the provider status (green = online, red = offline).
