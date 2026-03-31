use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::config::read_jsonc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModel {
    pub alias: String,
    pub path: String,
    #[serde(default)]
    pub hf_repo: Option<String>,
    #[serde(default = "default_context")]
    pub context_length: u32,
    #[serde(default = "default_gpu_layers")]
    pub gpu_layers: i32,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Set to true for models that produce reasoning_content chunks (e.g.
    /// DeepSeek-R1, Qwen3 with thinking mode). Enables the thinking-level UI.
    #[serde(default)]
    pub reasoning: bool,
}

fn default_context() -> u32 {
    8192
}
fn default_gpu_layers() -> i32 {
    99
}
fn default_port() -> u16 {
    1234
}

impl LocalModel {
    /// Resolve ~ in path.
    pub fn resolved_path(&self) -> PathBuf {
        if self.path.starts_with("~/")
            && let Some(home) = crate::home_dir()
        {
            return home.join(&self.path[2..]);
        }
        PathBuf::from(&self.path)
    }

    /// Build llama-server command arguments.
    pub fn server_args(&self) -> Vec<String> {
        let mut args = vec![
            "-m".into(),
            self.resolved_path().to_string_lossy().to_string(),
            "-c".into(),
            self.context_length.to_string(),
            "-ngl".into(),
            self.gpu_layers.to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            self.port.to_string(),
        ];
        args.extend(self.extra_args.clone());
        args
    }
}

/// Detect hardware and compute recommended defaults for a model.
pub fn recommended_defaults(model_path: &Path) -> LocalModel {
    let hw = detect_hardware();
    let model_size_bytes = std::fs::metadata(model_path).map(|m| m.len()).unwrap_or(0);
    let model_size_gb = model_size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    // Reserve 4GB for OS + nerv
    let available_gb = (hw.total_memory_gb - 4.0).max(2.0);

    // Rough param count from file size (Q4 ≈ 0.5 bytes/param, Q5 ≈ 0.6)
    let est_params_b = model_size_gb / 0.55;

    // KV cache estimate at q8_0: ~0.25MB per 1k context per billion parameters
    // (half of f16 at ~0.5MB, since we use q8_0 quantized KV cache)
    let kv_per_1k_ctx_mb = est_params_b * 0.25;

    // Max context: fill remaining memory after model, capped at 131072
    let remaining_gb = (available_gb - model_size_gb).max(0.5);
    let max_ctx_from_memory = ((remaining_gb * 1024.0) / kv_per_1k_ctx_mb * 1024.0) as u32;
    let context_length = max_ctx_from_memory.clamp(4096, 131_072);
    let context_length = (context_length / 1024) * 1024;

    // GPU layers: 99 (all) if model fits, otherwise partial offload
    let gpu_layers = if model_size_gb < available_gb { 99 } else { 0 };

    // Threads: generation leaves 2 cores for nerv + OS, batch uses all
    let gen_threads = (hw.physical_cores as i32 - 2).max(1);
    let batch_threads = hw.physical_cores as i32;

    // Batch size: larger on Apple Silicon with sufficient memory
    let batch_size = if remaining_gb > 6.0 {
        4096
    } else if remaining_gb > 3.0 {
        2048
    } else {
        1024
    };
    // On Apple Silicon, unified memory means no VRAM spill penalty from large
    // ubatch. Match ubatch to batch for maximum Metal throughput. On
    // memory-constrained systems, fall back to batch/2 to avoid stalling the
    // scheduler.
    let ubatch_size = if remaining_gb > 6.0 { batch_size } else { (batch_size / 2).max(256) };

    let extra_args = vec![
        "-fa".into(),
        "on".into(),
        "--mlock".into(),
        "-b".into(),
        batch_size.to_string(),
        "-ub".into(),
        ubatch_size.to_string(),
        "-t".into(),
        gen_threads.to_string(),
        "-tb".into(),
        batch_threads.to_string(),
        "-np".into(),
        "1".into(),
        "--host".into(),
        "127.0.0.1".into(),
        "--jinja".into(),
        "-ctk".into(),
        "q8_0".into(),
        "-ctv".into(),
        "q8_0".into(),
        // KV offload to Metal enabled (no -nkvo): on Apple Silicon unified memory,
        // keeping KV cache on GPU avoids CPU round-trips during generation.
        "--no-context-shift".into(),
        "--cache-reuse".into(),
        "256".into(),
        // No busy-polling: inference is Metal-bound, spinning wastes P-cores.
        "--poll".into(),
        "0".into(),
        // Elevate generation thread priority to reduce OS scheduling jitter.
        "--prio".into(),
        "2".into(),
    ];

    LocalModel {
        alias: String::new(), // filled by caller
        path: model_path.to_string_lossy().to_string(),
        hf_repo: None,
        context_length,
        gpu_layers,
        extra_args,
        port: 1234,
        reasoning: false,
    }
}

/// Total system RAM in GB (best-effort; falls back to 16 GB).
pub fn sysctl_mem_gb() -> f64 {
    #[cfg(target_os = "macos")]
    {
        sysctl_u64("hw.memsize").map(|b| b as f64 / (1024.0 * 1024.0 * 1024.0)).unwrap_or(16.0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        linux_mem_gb()
    }
}

/// Physical CPU core count (best-effort; falls back to 4).
pub fn sysctl_cores() -> u32 {
    #[cfg(target_os = "macos")]
    {
        sysctl_u32("hw.physicalcpu").unwrap_or(4)
    }
    #[cfg(not(target_os = "macos"))]
    {
        linux_cpu_cores()
    }
}

struct HardwareInfo {
    total_memory_gb: f64,
    physical_cores: u32,
}

fn detect_hardware() -> HardwareInfo {
    HardwareInfo { total_memory_gb: sysctl_mem_gb(), physical_cores: sysctl_cores() }
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &str) -> Option<u64> {
    use std::ffi::CString;
    let cname = CString::new(name).ok()?;
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let ret = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            &mut value as *mut u64 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 { Some(value) } else { None }
}

#[cfg(target_os = "macos")]
fn sysctl_u32(name: &str) -> Option<u32> {
    use std::ffi::CString;
    let cname = CString::new(name).ok()?;
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>();
    let ret = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            &mut value as *mut u32 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 { Some(value) } else { None }
}

/// Read total RAM from `/proc/meminfo` on Linux.
#[cfg(not(target_os = "macos"))]
fn linux_mem_gb() -> f64 {
    let Ok(content) = std::fs::read_to_string("/proc/meminfo") else {
        return 16.0;
    };
    for line in content.lines() {
        // "MemTotal:       16384000 kB"
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            if let Some(kb) = rest.split_whitespace().next().and_then(|s| s.parse::<u64>().ok()) {
                return kb as f64 / (1024.0 * 1024.0);
            }
        }
    }
    16.0
}

/// Count physical CPU cores via `/sys/devices/system/cpu/` on Linux.
#[cfg(not(target_os = "macos"))]
fn linux_cpu_cores() -> u32 {
    // nproc is logical cores, but that's the best we can do portably without
    // parsing /proc/cpuinfo for "core id" deduplication.
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(4)
}

/// Load models from ~/.nerv/models.json (JSONC).
pub fn load_models(nerv_dir: &Path) -> Vec<LocalModel> {
    let path = nerv_dir.join("models.json");
    read_jsonc::<Vec<LocalModel>>(&path).unwrap_or_default()
}

/// Save models to ~/.nerv/models.json.
pub fn save_models(nerv_dir: &Path, models: &[LocalModel]) -> anyhow::Result<()> {
    let path = nerv_dir.join("models.json");
    let tmp = nerv_dir.join("models.json.tmp");
    let content = serde_json::to_string_pretty(models)?;
    std::fs::write(&tmp, &content)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Find llama-server binary on PATH.
pub fn find_llama_server() -> Option<PathBuf> {
    which("llama-server")
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(name);
            if full.is_file() { Some(full) } else { None }
        })
    })
}

/// Check if llama-server is healthy on a port.
pub fn is_healthy(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/health", port);
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(1)))
        .build()
        .new_agent();
    agent.get(&url).call().is_ok_and(|r| r.status() == 200)
}

/// Download a GGUF file from HuggingFace. Returns the local path.
pub fn download_gguf(hf_repo: &str, quant: &str, cache_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(cache_dir)?;
    let agent = crate::http::agent();

    // Find the GGUF filename matching the quant pattern
    let api_url = format!("https://huggingface.co/api/models/{}/tree/main", hf_repo);
    eprintln!("Fetching file list from {}", api_url);
    let http_resp =
        agent.get(&api_url).call().map_err(|e| anyhow::anyhow!("GET {} failed: {}", api_url, e))?;
    let status = http_resp.status();
    if status != 200 {
        let body = http_resp.into_body().read_to_string().unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(String::from))
            .unwrap_or(body);
        anyhow::bail!(
            "GET {} returned {}: {}\n\
             hint: repo should be 'owner/name' (e.g. 'Qwen/Qwen3-30B-A3B-GGUF')",
            api_url,
            status,
            detail,
        );
    }
    let resp: serde_json::Value = http_resp
        .into_body()
        .read_json()
        .map_err(|e| anyhow::anyhow!("GET {} returned non-JSON: {}", api_url, e))?;

    let files = resp.as_array().ok_or_else(|| {
        anyhow::anyhow!(
            "GET {} returned unexpected JSON: {}",
            api_url,
            serde_json::to_string_pretty(&resp).unwrap_or_default(),
        )
    })?;

    let quant_lower = quant.to_lowercase();
    let gguf_file = files
        .iter()
        .filter_map(|f| f["path"].as_str())
        .find(|name| {
            let lower = name.to_lowercase();
            lower.ends_with(".gguf") && lower.contains(&quant_lower)
        })
        .ok_or_else(|| {
            let available: Vec<&str> = files
                .iter()
                .filter_map(|f| f["path"].as_str())
                .filter(|n| n.ends_with(".gguf"))
                .collect();
            anyhow::anyhow!(
                "no GGUF matching '{}' in {}. Available: {}",
                quant,
                hf_repo,
                available.join(", ")
            )
        })?;

    let local_path = cache_dir.join(gguf_file);
    if local_path.exists() {
        println!("Already downloaded: {}", local_path.display());
        return Ok(local_path);
    }

    let download_url = format!("https://huggingface.co/{}/resolve/main/{}", hf_repo, gguf_file);

    // Resume partial download if .part file exists
    let tmp_path = cache_dir.join(format!("{}.part", gguf_file));
    let existing_size = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);

    let mut req = agent.get(&download_url);
    if existing_size > 0 {
        req = req.header("Range", &format!("bytes={}-", existing_size));
        println!("Resuming {} from {:.0}MB...", gguf_file, existing_size as f64 / 1_048_576.0);
    } else {
        println!("Downloading {}...", gguf_file);
    }

    let resp = req.call()?;

    // Content-Length is the remaining bytes for range requests
    let remaining =
        resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    let total = remaining.map(|r| r + existing_size);

    let mut body = resp.into_body();
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&tmp_path)?;
    let mut downloaded = existing_size;
    let mut buf = vec![0u8; 256 * 1024];
    let mut last_print = std::time::Instant::now();

    loop {
        let n = std::io::Read::read(&mut body.as_reader(), &mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])?;
        downloaded += n as u64;

        if last_print.elapsed() > std::time::Duration::from_secs(1) {
            if let Some(total) = total {
                let pct = (downloaded as f64 / total as f64) * 100.0;
                let mb = downloaded as f64 / 1_048_576.0;
                let total_mb = total as f64 / 1_048_576.0;
                print!("\r  {:.0}MB / {:.0}MB ({:.0}%)", mb, total_mb, pct);
            } else {
                print!("\r  {:.0}MB", downloaded as f64 / 1_048_576.0);
            }
            let _ = std::io::Write::flush(&mut std::io::stdout());
            last_print = std::time::Instant::now();
        }
    }
    println!();

    std::fs::rename(&tmp_path, &local_path)?;
    println!("Saved to {}", local_path.display());
    Ok(local_path)
}
