# Local models

nerv supports two ways to use local models:

| | `models.json` + `nerv load` | `local_providers` in `config.json` |
|---|---|---|
| **Who manages the server?** | nerv (`exec` into llama-server) | You (Ollama, your own llama-server) |
| **Model discovery** | Declared in `models.json` | Queried from `/v1/models` at startup |
| **Hardware tuning** | Yes — context, GPU layers, KV cache | No — server decides |
| **Use when** | You want nerv to own the llama-server lifecycle | You already run Ollama or a standalone server |

## Ollama

Ollama is auto-discovered by default. Just run Ollama and pull models:

```
ollama pull llama3.2
nerv
```

nerv queries `http://localhost:11434/v1/models` at startup and registers whatever models are pulled. If Ollama is offline, it is silently skipped.

To use a non-default host or port, edit `~/.nerv/config.json`:

```jsonc
{
  "local_providers": [
    { "name": "ollama", "base_url": "http://localhost:11434/v1" }
  ]
}
```

## llama.cpp (nerv-managed)

### Workflow

```
nerv add <hf-repo> <quant>   # download GGUF
nerv load [alias]             # exec into llama-server (separate terminal)
nerv                          # model appears automatically
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

Models declared in `models.json` are registered as `local/{alias}` providers at startup regardless of whether llama-server is running. nerv will use whichever are healthy. No manual `/model add local` step is needed.

## Health checks

On startup, nerv spawns a background thread that polls each custom
provider's `/models` endpoint every 5 seconds until it responds. The
footer shows the provider status (green = online, red = offline).
