//! 监控服务模块
//!
//! 独立于 Agent Runtime 的后台监控任务。
//! 负责：系统状态采集、异常检测、事件推送。
//!
//! 设计原则：Ollama 离线时仪表盘仍可用，监控服务完全独立。

use serde::Serialize;
use std::process::Command;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows CREATE_NO_WINDOW 标志，防止控制台窗口闪烁
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// ============================================================
// 数据模型
// ============================================================

/// 系统指标快照（用于 IPC 返回）
#[derive(Debug, Clone, Serialize)]
pub struct SystemMetrics {
    pub cpu_usage: f32,
    pub memory_total_gb: f32,
    pub memory_used_gb: f32,
    pub memory_usage_percent: f32,
    pub disks: Vec<DiskInfo>,
    pub network_ok: bool,
    pub gpu_name: Option<String>,
    pub gpu_usage: Option<f32>,
}

/// 磁盘信息
#[derive(Debug, Clone, Serialize)]
pub struct DiskInfo {
    pub drive: String,
    pub total_gb: f32,
    pub used_gb: f32,
    pub usage_percent: f32,
}

/// GPU 信息
#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    pub name: String,
    pub usage_percent: f32,
    pub memory_used_mb: u64,
    pub memory_total_mb: u64,
}

/// 监控状态快照（兼容旧接口）
#[derive(Debug, Serialize)]
pub struct MonitorStateSnapshot {
    pub status: &'static str,
    pub message: &'static str,
}

// ============================================================
// 主入口：获取系统指标
// ============================================================

/// 获取当前系统指标（跨平台）
///
/// Windows：通过 PowerShell 查询真实数据
/// 其他平台：返回默认值
pub fn get_system_metrics() -> SystemMetrics {
    #[cfg(target_os = "windows")]
    {
        get_system_metrics_windows()
    }
    #[cfg(not(target_os = "windows"))]
    {
        SystemMetrics {
            cpu_usage: 0.0,
            memory_total_gb: 0.0,
            memory_used_gb: 0.0,
            memory_usage_percent: 0.0,
            disks: vec![],
            network_ok: true,
            gpu_name: None,
            gpu_usage: None,
        }
    }
}

// ============================================================
// Windows 实现
// ============================================================

#[cfg(target_os = "windows")]
fn get_system_metrics_windows() -> SystemMetrics {
    let cpu_usage = get_cpu_usage_windows().unwrap_or(0.0);
    let (memory_total_gb, memory_used_gb, memory_usage_percent) =
        get_memory_usage_windows().unwrap_or((0.0, 0.0, 0.0));
    let disks = get_disk_usage_windows().unwrap_or_default();
    let network_ok = check_network_windows().unwrap_or(true);
    let gpu = get_gpu_usage_windows().ok();

    SystemMetrics {
        cpu_usage,
        memory_total_gb,
        memory_used_gb,
        memory_usage_percent,
        disks,
        network_ok,
        gpu_name: gpu.as_ref().map(|g| g.name.clone()),
        gpu_usage: gpu.as_ref().map(|g| g.usage_percent),
    }
}

/// 查询 CPU 使用率（Windows）
#[cfg(target_os = "windows")]
fn get_cpu_usage_windows() -> Option<f32> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle", "Hidden",
            "-Command",
            r"(Get-Counter '\Processor(_Total)\% Processor Time').CounterSamples.CookedValue",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<f32>().ok().map(|v| v.clamp(0.0, 100.0))
}

/// 查询内存使用情况（Windows）
#[cfg(target_os = "windows")]
fn get_memory_usage_windows() -> Option<(f32, f32, f32)> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle", "Hidden",
            "-Command",
            "$os = Get-WmiObject Win32_OperatingSystem; $total = [math]::Round($os.TotalVisibleMemorySize / 1MB, 2); $free = [math]::Round($os.FreePhysicalMemory / 1MB, 2); $used = $total - $free; $pct = [math]::Round(($used / $total) * 100, 1); \"$total,$used,$pct\"",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = text.trim().split(',').collect();
    if parts.len() == 3 {
        let total = parts[0].parse::<f32>().ok()?;
        let used = parts[1].parse::<f32>().ok()?;
        let pct = parts[2].parse::<f32>().ok()?;
        Some((total, used, pct))
    } else {
        None
    }
}

/// 查询磁盘使用情况（Windows）
#[cfg(target_os = "windows")]
fn get_disk_usage_windows() -> Option<Vec<DiskInfo>> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle", "Hidden",
            "-Command",
            "Get-WmiObject Win32_LogicalDisk | Where-Object { $_.DriveType -eq 3 } | ForEach-Object { $total = [math]::Round($_.Size / 1GB, 2); $free = [math]::Round($_.FreeSpace / 1GB, 2); $used = $total - $free; $pct = [math]::Round(($used / $total) * 100, 1); \"$($_.DeviceID),$total,$used,$pct\" }",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    let mut disks = Vec::new();

    for line in text.lines() {
        let parts: Vec<&str> = line.trim().split(',').collect();
        if parts.len() == 4 {
            disks.push(DiskInfo {
                drive: parts[0].to_string(),
                total_gb: parts[1].parse::<f32>().unwrap_or(0.0),
                used_gb: parts[2].parse::<f32>().unwrap_or(0.0),
                usage_percent: parts[3].parse::<f32>().unwrap_or(0.0),
            });
        }
    }

    Some(disks)
}

/// 检查网络状态（Windows）
#[cfg(target_os = "windows")]
fn check_network_windows() -> Option<bool> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle", "Hidden",
            "-Command",
            "Test-Connection -ComputerName 8.8.8.8 -Count 1 -Quiet",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.trim().eq_ignore_ascii_case("true"))
}

/// 查询 GPU 使用情况（Windows）
///
/// 优先使用 nvidia-smi，回退到 WMI Win32_VideoController
#[cfg(target_os = "windows")]
pub fn get_gpu_usage_windows() -> Result<GpuInfo, String> {
    // 尝试 nvidia-smi
    if let Ok(gpu) = get_gpu_nvidia_smi() {
        return Ok(gpu);
    }

    // 回退到 WMI
    get_gpu_wmi()
}

/// 通过 nvidia-smi 查询 NVIDIA GPU
#[cfg(target_os = "windows")]
fn get_gpu_nvidia_smi() -> Result<GpuInfo, String> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("nvidia-smi failed: {}", e))?;

    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().ok_or("No GPU output")?;
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();

    if parts.len() >= 4 {
        Ok(GpuInfo {
            name: parts[0].to_string(),
            usage_percent: parts[1].parse::<f32>().unwrap_or(0.0),
            memory_used_mb: parts[2].parse::<u64>().unwrap_or(0),
            memory_total_mb: parts[3].parse::<u64>().unwrap_or(0),
        })
    } else {
        Err("Invalid nvidia-smi output format".to_string())
    }
}

/// 通过 WMI 查询 GPU（回退方案）
#[cfg(target_os = "windows")]
fn get_gpu_wmi() -> Result<GpuInfo, String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle", "Hidden",
            "-Command",
            "$gpu = Get-WmiObject Win32_VideoController | Select-Object -First 1; \"$($gpu.Name),0,0,$($gpu.AdapterRAM / 1MB)\"",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("WMI query failed: {}", e))?;

    let text = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = text.trim().split(',').collect();

    if parts.len() >= 4 {
        Ok(GpuInfo {
            name: parts[0].to_string(),
            usage_percent: parts[1].parse::<f32>().unwrap_or(0.0),
            memory_used_mb: parts[2].parse::<u64>().unwrap_or(0),
            memory_total_mb: parts[3].parse::<u64>().unwrap_or(0),
        })
    } else {
        Err("Invalid WMI output format".to_string())
    }
}

// ============================================================
// 兼容旧接口
// ============================================================

/// 监控服务
pub struct MonitoringService;

impl MonitoringService {
    pub fn new() -> Self {
        Self
    }

    pub async fn start(&self) {
        // 占位：后台监控任务在后续版本实现
    }

    pub async fn get_status(&self) -> MonitorStateSnapshot {
        MonitorStateSnapshot {
            status: "ok",
            message: "Monitoring service active",
        }
    }
}

impl Default for MonitoringService {
    fn default() -> Self {
        Self::new()
    }
}
