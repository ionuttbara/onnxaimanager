use sha2::{Digest, Sha256, Sha512};
use serde::Deserialize;
use slint::{Color, SharedString};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};
use sysinfo::System;
use winreg::enums::*;
use winreg::RegKey;
use wmi::{COMLibrary, WMIConnection};

slint::include_modules!();

const EMBEDDED_JSON: &str = include_str!("../models.json");
const REGISTRY_PATH: &str = r#"Software\Gallery Inc\AIModelManager"#;
const MODELS_REGISTRY_PATH: &str = r#"Software\Gallery Inc\AIModelManager\Models"#;
const LEGACY_REGISTRY_PATH: &str = r#"Software\Gallery Inc\AI-ModelsManager"#;
const DEFAULT_FALLBACK_PATH: &str = r#"H:\AI_Models\ioscap"#;
const GITHUB_URL: &str = "https://github.com/ionuttbara/onnxaimanager";
const DOWNLOAD_MODELS_URL: &str = "https://k00.fr/aimodels";
const IO_BUFFER_SIZE: usize = 4 * 1024 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

// ============================================================================
// Model & Manifest Structures
// ============================================================================

#[derive(Deserialize, Debug, Clone)]
pub struct RawAiModel {
    pub name: String,
    pub model_type: String,
    pub used_for: String,
    #[serde(default)]
    pub supports: Vec<String>,
    #[serde(default = "default_author")]
    pub author: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub sha512: String,
    #[serde(default)]
    pub package_kind: String,
    #[serde(default)]
    pub install_dir: String,
    #[serde(default)]
    pub required_paths: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AiModel {
    pub id: String,
    pub name: String,
    pub model_type: String,
    pub used_for: String,
    pub supports: Vec<String>,
    pub author: String,
    pub expected_sha256: Option<String>,
    pub expected_sha512: Option<String>,
    pub package_kind: String,
    pub install_dir: Option<String>,
    pub required_paths: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct InstalledModel {
    pub name: String,
    pub model_type: String,
    pub used_for: String,
    pub supports: Vec<String>,
    pub size_bytes: u64,
    pub author: String,
    pub verification: String,
}

#[derive(Debug)]
pub struct ReconcileResult {
    pub installed: Vec<InstalledModel>,
    pub total_bytes: u64,
    pub invalid_models: usize,
}

#[derive(Debug, Clone)]
pub struct RegistryRecord {
    pub file_name: String,
    pub full_path: String,
    pub hash_algorithm: String,
    pub hash_value: String,
    pub last_write_token: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct Sha2Fingerprints {
    pub sha256: String,
    pub sha512: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashKind {
    Sha256,
    Sha512,
}

impl HashKind {
    fn label(self) -> &'static str {
        match self {
            Self::Sha256 => "SHA-256",
            Self::Sha512 => "SHA-512",
        }
    }
}

enum Sha2Hasher {
    Sha256(Sha256),
    Sha512(Sha512),
}

impl Sha2Hasher {
    fn new(kind: HashKind) -> Self {
        match kind {
            HashKind::Sha256 => Self::Sha256(Sha256::new()),
            HashKind::Sha512 => Self::Sha512(Sha512::new()),
        }
    }

    fn update(&mut self, chunk: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(chunk),
            Self::Sha512(hasher) => hasher.update(chunk),
        }
    }

    fn finalize_hex(self) -> String {
        match self {
            Self::Sha256(hasher) => hex::encode(hasher.finalize()),
            Self::Sha512(hasher) => hex::encode(hasher.finalize()),
        }
    }
}

fn default_author() -> String {
    "Unknown".to_string()
}

fn format_size(bytes: u64) -> String {
    const MB: f64 = 1_048_576.0;
    const GB: f64 = 1_073_741_824.0;

    if bytes as f64 >= GB {
        format!("{:.2} GB", bytes as f64 / GB)
    } else if bytes as f64 >= MB {
        format!("{:.2} MB", bytes as f64 / MB)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

// ============================================================================
// Class: ManifestManager
// ============================================================================

pub struct ManifestManager {
    models: Vec<AiModel>,
    by_name: HashMap<String, usize>,
    by_sha256: HashMap<String, usize>,
    by_sha512: HashMap<String, usize>,
}

impl ManifestManager {
    pub fn load_from_embedded() -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let raw_models: Vec<RawAiModel> = match serde_json::from_str(EMBEDDED_JSON) {
            Ok(models) => models,
            Err(error) => {
                warnings.push(format!("Failed to parse embedded manifest: {error}"));
                Vec::new()
            }
        };

        let mut models = Vec::new();
        let mut by_name = HashMap::new();
        let mut by_sha256 = HashMap::new();
        let mut by_sha512 = HashMap::new();
        let mut seen_names = HashSet::new();

        for (index, raw) in raw_models.into_iter().enumerate() {
            let name = raw.name.trim().to_string();
            if name.is_empty() {
                warnings.push(format!("Entry #{index} skipped: missing file name"));
                continue;
            }

            let normalized_name = name.to_ascii_lowercase();
            if !seen_names.insert(normalized_name.clone()) {
                warnings.push(format!("Entry '{name}' skipped: duplicate file name"));
                continue;
            }

            let expected_sha256 = Self::normalize_hash(&raw.sha256, 64, "SHA-256", &name, &mut warnings);
            let expected_sha512 = Self::normalize_hash(&raw.sha512, 128, "SHA-512", &name, &mut warnings);

            let id = Self::derive_id(&name);
            let model = AiModel {
                id,
                name: name.clone(),
                model_type: raw.model_type.trim().to_string(),
                used_for: raw.used_for.trim().to_string(),
                supports: raw
                    .supports
                    .into_iter()
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect(),
                author: if raw.author.trim().is_empty() {
                    default_author()
                } else {
                    raw.author.trim().to_string()
                },
                expected_sha256: expected_sha256.clone(),
                expected_sha512: expected_sha512.clone(),
                package_kind: raw.package_kind.trim().to_ascii_lowercase(),
                install_dir: if raw.install_dir.trim().is_empty() {
                    None
                } else {
                    Some(raw.install_dir.trim().replace('\\', "/"))
                },
                required_paths: raw
                    .required_paths
                    .into_iter()
                    .map(|p| p.trim().replace('\\', "/"))
                    .filter(|p| !p.is_empty())
                    .collect(),
            };

            let pos = models.len();
            by_name.insert(normalized_name, pos);
            if let Some(hash) = expected_sha256 {
                if by_sha256.insert(hash.clone(), pos).is_some() {
                    warnings.push(format!("Duplicate verification signature in manifest for '{name}'"));
                }
            }
            if let Some(hash) = expected_sha512 {
                if by_sha512.insert(hash.clone(), pos).is_some() {
                    warnings.push(format!("Duplicate verification signature in manifest for '{name}'"));
                }
            }
            models.push(model);
        }

        (
            Self {
                models,
                by_name,
                by_sha256,
                by_sha512,
            },
            warnings,
        )
    }

    pub fn find_by_name(&self, name: &str) -> Option<&AiModel> {
        self.by_name
            .get(&name.to_ascii_lowercase())
            .and_then(|idx| self.models.get(*idx))
    }

    pub fn find_by_fingerprints(&self, fp: &Sha2Fingerprints) -> Option<&AiModel> {
        self.by_sha512
            .get(&fp.sha512.to_ascii_lowercase())
            .and_then(|idx| self.models.get(*idx))
            .or_else(|| {
                self.by_sha256
                    .get(&fp.sha256.to_ascii_lowercase())
                    .and_then(|idx| self.models.get(*idx))
            })
    }

    pub fn models(&self) -> &[AiModel] {
        &self.models
    }

    fn normalize_hash(
        value: &str,
        expected_len: usize,
        label: &str,
        file_name: &str,
        warnings: &mut Vec<String>,
    ) -> Option<String> {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }
        if normalized.len() != expected_len || !normalized.bytes().all(|b| b.is_ascii_hexdigit()) {
            warnings.push(format!("Entry '{file_name}': invalid verification metadata; local verification will be used"));
            return None;
        }
        Some(normalized)
    }

    fn derive_id(file_name: &str) -> String {
        let stem = Path::new(file_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(file_name);
        stem.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    }
}

impl AiModel {
    fn expected_hash(&self) -> Option<(HashKind, &str)> {
        if let Some(hash) = self.expected_sha512.as_deref() {
            Some((HashKind::Sha512, hash))
        } else if let Some(hash) = self.expected_sha256.as_deref() {
            Some((HashKind::Sha256, hash))
        } else {
            None
        }
    }

    fn verification_label(&self) -> String {
        "Verified".to_string()
    }

    fn is_zip_bundle(&self) -> bool {
        self.package_kind.eq_ignore_ascii_case("zip")
    }

    fn installed_path(&self, root: &Path) -> PathBuf {
        if self.is_zip_bundle() {
            if let Some(dir) = self.install_dir.as_deref() {
                return root.join(dir);
            }
        }
        root.join(&self.name)
    }

    fn required_paths_present(&self, base: &Path) -> bool {
        self.required_paths.iter().all(|rel| base.join(rel).exists())
    }
}

// ============================================================================
// Class: StorageRegistry
// ============================================================================

pub struct StorageRegistry {
    app_key: RegKey,
    models_key: RegKey,
}

impl StorageRegistry {
    pub fn open() -> io::Result<Self> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (app_key, _) = hkcu.create_subkey(REGISTRY_PATH)?;
        let (models_key, _) = hkcu.create_subkey(MODELS_REGISTRY_PATH)?;

        let _ = app_key.set_value("AppVersion", &env!("CARGO_PKG_VERSION"));

        Ok(Self { app_key, models_key })
    }

    pub fn get_saved_location() -> Option<PathBuf> {
        if let Ok(store) = Self::open() {
            if let Ok(location) = store.app_key.get_value::<String, _>("ModelRoot") {
                let path = PathBuf::from(location);
                if path.is_dir() {
                    return Some(path);
                }
            }
        }

        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            let root = RegKey::predef(hive);
            if let Ok(key) = root.open_subkey(LEGACY_REGISTRY_PATH) {
                if let Ok(location) = key.get_value::<String, _>("location") {
                    let path = PathBuf::from(location);
                    if path.is_dir() {
                        let _ = Self::save_location(&path);
                        return Some(path);
                    }
                }
            }
        }

        let fallback = PathBuf::from(DEFAULT_FALLBACK_PATH);
        if fallback.is_dir() {
            let _ = Self::save_location(&fallback);
            return Some(fallback);
        }

        None
    }

    pub fn save_location(path: &Path) -> io::Result<()> {
        let store = Self::open()?;
        store.app_key.set_value("ModelRoot", &path.to_string_lossy().to_string())
    }

    pub fn read_model(&self, model_id: &str) -> io::Result<Option<RegistryRecord>> {
        let key = match self.models_key.open_subkey(model_id) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };

        // Old MD5/size based cache records are intentionally ignored. They will
        // be migrated once, on the next reconciliation, to the SHA-2 cache.
        let hash_algorithm: String = match key.get_value("HashAlgorithm") {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let hash_value: String = match key.get_value("HashValue") {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };

        Ok(Some(RegistryRecord {
            file_name: key.get_value("FileName")?,
            full_path: key.get_value("FullPath")?,
            hash_algorithm,
            hash_value,
            last_write_token: key.get_value("LastWriteToken")?,
            status: key.get_value("Status")?,
        }))
    }

    pub fn write_model(
        &self,
        model: &AiModel,
        full_path: &Path,
        metadata: &fs::Metadata,
        hash_kind: HashKind,
        hash_value: &str,
    ) -> io::Result<()> {
        let (key, _) = self.models_key.create_subkey(&model.id)?;
        let norm_path = fs::canonicalize(full_path)
            .unwrap_or_else(|_| full_path.to_path_buf())
            .to_string_lossy()
            .to_string();
        let modified_token = metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_else(|| "0".to_string());

        key.set_value("FileName", &model.name)?;
        key.set_value("FullPath", &norm_path)?;
        let algorithm = hash_kind.label().to_string();
        let normalized_hash = hash_value.to_ascii_lowercase();
        key.set_value("HashAlgorithm", &algorithm)?;
        key.set_value("HashValue", &normalized_hash)?;
        key.set_value("LastWriteToken", &modified_token)?;
        let status = if model.expected_hash().is_some() {
            "Verified".to_string()
        } else {
            "FingerprintStored".to_string()
        };
        key.set_value("Status", &status)?;

        // Best-effort cleanup of the legacy identity fields. File size is no
        // longer part of verification and MD5 is no longer trusted.
        let _ = key.delete_value("MD5");
        let _ = key.delete_value("SizeBytes");
        Ok(())
    }

    pub fn remove_model(&self, model_id: &str) -> io::Result<()> {
        match self.models_key.delete_subkey_all(model_id) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn cleanup_orphans(&self, valid_ids: &HashSet<String>) -> io::Result<()> {
        let keys: Vec<String> = self.models_key.enum_keys().filter_map(Result::ok).collect();
        for key in keys {
            if !valid_ids.contains(&key) {
                let _ = self.remove_model(&key);
            }
        }
        Ok(())
    }
}

// ============================================================================
// Class: SystemInspector
// ============================================================================

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct VideoControllerWmi {
    name: String,
    driver_version: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct PnPSignedDriverWmi {
    device_name: Option<String>,
    driver_version: Option<String>,
    manufacturer: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct PnPEntityWmi {
    name: Option<String>,
    manufacturer: Option<String>,
}

pub struct SysInfoData {
    pub general: String,
    pub gpu: String,
    pub npu: String,
}

pub struct SystemInspector;

impl SystemInspector {
    pub fn inspect() -> SysInfoData {
        let mut general = String::new();
        let mut gpu = String::new();
        let mut npu = String::new();

        let mut sys = System::new_all();
        sys.refresh_all();

        let cpu = sys.cpus().first().map(|c| c.brand().trim()).unwrap_or("Unknown CPU");
        let ram_gb = sys.total_memory() as f64 / 1_073_741_824.0;

        general.push_str(&format!("Application:\n- AI Models Manager: v{}\n\n", env!("CARGO_PKG_VERSION")));
        general.push_str(&format!("Core Hardware:\n- CPU: {cpu}\n- RAM: {ram_gb:.1} GB\n"));

        match COMLibrary::new() {
            Ok(com) => match WMIConnection::new(com) {
                Ok(wmi) => {
                    Self::query_gpus(&wmi, &mut gpu);
                    Self::query_npus(&wmi, &mut npu);
                }
                Err(e) => {
                    let err_msg = format!("- WMI Init failed: {e}\n");
                    gpu.push_str(&err_msg);
                    npu.push_str(&err_msg);
                }
            },
            Err(e) => {
                let err_msg = format!("- COM Init failed: {e}\n");
                gpu.push_str(&err_msg);
                npu.push_str(&err_msg);
            }
        }

        Self::query_cuda(&mut gpu);

        SysInfoData { general, gpu, npu }
    }

    fn query_gpus(wmi: &WMIConnection, info: &mut String) {
        let mut found = false;
        if let Ok(gpus) = wmi.raw_query::<VideoControllerWmi>("SELECT Name, DriverVersion FROM Win32_VideoController") {
            for gpu in gpus {
                found = true;
                let driver = gpu.driver_version.unwrap_or_else(|| "Unknown".to_string());
                info.push_str(&format!("- Graphic Adapter: {} (Driver: {})\n", gpu.name, driver));
                info.push_str("  [ONNX DirectML Capabilities: FP32, FP16, INT32, INT16, INT8, INT8 Quantized via D3D12 DP4a]\n\n");
            }
        }
        if !found {
            info.push_str("- GPU: Not detected via WMI.\n\n");
        }
    }

    fn query_npus(wmi: &WMIConnection, info: &mut String) {
        let mut found_npus = HashSet::new();
        let mut found = false;

        let driver_query = "SELECT DeviceName, DriverVersion, Manufacturer FROM Win32_PnPSignedDriver WHERE DeviceName LIKE '%NPU%' OR DeviceName LIKE '%Neural%'";
        if let Ok(drivers) = wmi.raw_query::<PnPSignedDriverWmi>(driver_query) {
            for drv in drivers {
                if let Some(name) = drv.device_name {
                    let name_lower = name.to_ascii_lowercase();
                    if !name_lower.contains("usb") && !name_lower.contains("input") {
                        let mfg = drv.manufacturer.unwrap_or_else(|| "Unknown Manufacturer".to_string());
                        let ver = drv.driver_version.unwrap_or_else(|| "Unknown Driver".to_string());
                        if found_npus.insert(name.clone()) {
                            found = true;
                            info.push_str(&format!("- Neural Processor: {} (Manufacturer: {}, Driver: {})\n", name, mfg, ver));
                            info.push_str("  [ONNX DirectML (MCDM) Capabilities: Optimized for INT8 Quantized TOPS, INT16, FP16. Non-quantized FP32/INT32 operations automatically delegate to fallback paths.]\n\n");
                        }
                    }
                }
            }
        }

        if !found {
            let entity_query = "SELECT Name, Manufacturer FROM Win32_PnPEntity WHERE Name LIKE '%NPU%' OR Name LIKE '%Neural%'";
            if let Ok(entities) = wmi.raw_query::<PnPEntityWmi>(entity_query) {
                for ent in entities {
                    if let Some(name) = ent.name {
                        let name_lower = name.to_ascii_lowercase();
                        if !name_lower.contains("usb") && !name_lower.contains("input") {
                            let mfg = ent.manufacturer.unwrap_or_else(|| "Unknown Manufacturer".to_string());
                            if found_npus.insert(name.clone()) {
                                found = true;
                                info.push_str(&format!("- Neural Processor: {} (Manufacturer: {})\n", name, mfg));
                                info.push_str("  [ONNX DirectML (MCDM) Capabilities: Optimized for INT8 Quantized TOPS, INT16, FP16. Non-quantized FP32/INT32 operations automatically delegate to fallback paths.]\n\n");
                            }
                        }
                    }
                }
            }
        }

        if !found {
            info.push_str("- NPU: Not detected.\n");
        }
    }

    fn query_cuda(info: &mut String) {
        info.push_str("CUDA & AI Runtime Environment:\n");
        match cuda_driver::query_cuda_driver_version() {
            Ok(v) => info.push_str(&format!("- NVIDIA CUDA Driver API Level: {v}\n")),
            Err(e) => info.push_str(&format!("- NVIDIA CUDA: Not available ({e})\n")),
        }
    }
}

// ============================================================================
// Class: ModelDeploymentService
// ============================================================================

pub struct ModelDeploymentService;

impl ModelDeploymentService {
    pub fn compute_hash<F>(path: &Path, kind: HashKind, mut progress: F) -> io::Result<String>
    where
        F: FnMut(f32),
    {
        let metadata = fs::metadata(path)?;
        let total_size = metadata.len();
        let denominator = total_size.max(1) as f64;

        let mut file = File::open(path)?;
        let mut hasher = Sha2Hasher::new(kind);
        let mut buffer = vec![0_u8; IO_BUFFER_SIZE];
        let mut processed = 0_u64;
        let mut last_emit = Instant::now() - PROGRESS_INTERVAL;

        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
            processed = processed.saturating_add(bytes_read as u64);

            if last_emit.elapsed() >= PROGRESS_INTERVAL {
                progress((processed as f64 / denominator) as f32);
                last_emit = Instant::now();
            }
        }

        progress(1.0);
        Ok(hasher.finalize_hex())
    }

    pub fn compute_sha2<F>(path: &Path, mut progress: F) -> io::Result<Sha2Fingerprints>
    where
        F: FnMut(f32),
    {
        let metadata = fs::metadata(path)?;
        let total_size = metadata.len();
        let denominator = total_size.max(1) as f64;

        let mut file = File::open(path)?;
        let mut sha256 = Sha256::new();
        let mut sha512 = Sha512::new();
        let mut buffer = vec![0_u8; IO_BUFFER_SIZE];
        let mut processed = 0_u64;
        let mut last_emit = Instant::now() - PROGRESS_INTERVAL;

        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            let chunk = &buffer[..bytes_read];
            sha256.update(chunk);
            sha512.update(chunk);
            processed = processed.saturating_add(bytes_read as u64);

            if last_emit.elapsed() >= PROGRESS_INTERVAL {
                progress((processed as f64 / denominator) as f32);
                last_emit = Instant::now();
            }
        }

        progress(1.0);
        Ok(Sha2Fingerprints {
            sha256: hex::encode(sha256.finalize()),
            sha512: hex::encode(sha512.finalize()),
        })
    }

    pub fn copy_file_computing_hash<F>(
        source: &Path,
        destination: &Path,
        kind: HashKind,
        mut progress: F,
    ) -> io::Result<String>
    where
        F: FnMut(f32),
    {
        let metadata = fs::metadata(source)?;
        let total_size = metadata.len();
        let denominator = total_size.max(1) as f64;

        if destination.exists() {
            fs::remove_file(destination)?;
        }

        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)?;
        let mut hasher = Sha2Hasher::new(kind);
        let mut buffer = vec![0_u8; IO_BUFFER_SIZE];
        let mut processed = 0_u64;
        let mut last_emit = Instant::now() - PROGRESS_INTERVAL;

        loop {
            let bytes_read = input.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            let chunk = &buffer[..bytes_read];
            output.write_all(chunk)?;
            hasher.update(chunk);
            processed = processed.saturating_add(bytes_read as u64);

            if last_emit.elapsed() >= PROGRESS_INTERVAL {
                progress((processed as f64 / denominator) as f32);
                last_emit = Instant::now();
            }
        }

        output.flush()?;
        output.sync_all()?;
        progress(1.0);
        Ok(hasher.finalize_hex())
    }

    fn hash_for_kind<'a>(fingerprints: &'a Sha2Fingerprints, kind: HashKind) -> &'a str {
        match kind {
            HashKind::Sha256 => &fingerprints.sha256,
            HashKind::Sha512 => &fingerprints.sha512,
        }
    }

    fn validate_fingerprints(
        model: &AiModel,
        fingerprints: &Sha2Fingerprints,
    ) -> io::Result<(HashKind, String)> {
        if let Some((kind, expected)) = model.expected_hash() {
            let actual = Self::hash_for_kind(fingerprints, kind);
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Verification failed for '{}'. The model contents do not match the expected model.",
                        model.name
                    ),
                ));
            }
            Ok((kind, actual.to_ascii_lowercase()))
        } else {
            // A manifest without a published SHA-2 digest can still be managed
            // by exact file name. We record a local SHA-256 fingerprint so any
            // later modification invalidates the cache without relying on size.
            Ok((HashKind::Sha256, fingerprints.sha256.to_ascii_lowercase()))
        }
    }

    fn validate_single_hash(model: &AiModel, kind: HashKind, actual: &str) -> io::Result<String> {
        match model.expected_hash() {
            Some((expected_kind, expected)) => {
                if expected_kind != kind || !actual.eq_ignore_ascii_case(expected) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Verification failed for '{}'. The model contents do not match the expected model.",
                            model.name
                        ),
                    ));
                }
            }
            None => {
                if kind != HashKind::Sha256 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Model verification configuration is invalid",
                    ));
                }
            }
        }
        Ok(actual.to_ascii_lowercase())
    }

    pub fn identify_file<F>(
        source: &Path,
        manifest: &ManifestManager,
        progress: F,
    ) -> io::Result<(AiModel, String)>
    where
        F: FnMut(f32),
    {
        let file_name = source
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid model or package file name"))?;

        if let Some(model) = manifest.find_by_name(file_name) {
            let kind = model
                .expected_hash()
                .map(|(kind, _)| kind)
                .unwrap_or(HashKind::Sha256);
            let actual = Self::compute_hash(source, kind, progress)?;
            let _ = Self::validate_single_hash(model, kind, &actual)?;
            return Ok((model.clone(), "Verified".to_string()));
        }

        // Unknown file names need both SHA-2 digests so they can still match a
        // manifest entry by content. This slower path is only used when the
        // canonical file name is not available.
        let fingerprints = Self::compute_sha2(source, progress)?;
        if let Some(model) = manifest.find_by_fingerprints(&fingerprints) {
            let (kind, _) = Self::validate_fingerprints(model, &fingerprints)?;
            let _ = kind;
            return Ok((model.clone(), "Verified".to_string()));
        }

        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Model or package not recognized. Use the expected file name or update models.json.",
        ))
    }

    fn cache_is_valid(
        record: &RegistryRecord,
        model: &AiModel,
        file_path: &Path,
        modified_token: &str,
    ) -> bool {
        let expected_status = if model.expected_hash().is_some() {
            "Verified"
        } else {
            "FingerprintStored"
        };
        if !record.status.eq_ignore_ascii_case(expected_status)
            || !record.file_name.eq_ignore_ascii_case(&model.name)
            || record.last_write_token != modified_token
        {
            return false;
        }

        let current_path = fs::canonicalize(file_path)
            .unwrap_or_else(|_| file_path.to_path_buf())
            .to_string_lossy()
            .to_string();
        if !record.full_path.eq_ignore_ascii_case(&current_path) {
            return false;
        }

        match model.expected_hash() {
            Some((kind, expected)) => {
                record.hash_algorithm.eq_ignore_ascii_case(kind.label())
                    && record.hash_value.eq_ignore_ascii_case(expected)
            }
            None => {
                record.hash_algorithm.eq_ignore_ascii_case(HashKind::Sha256.label())
                    && record.hash_value.len() == 64
                    && record.hash_value.bytes().all(|b| b.is_ascii_hexdigit())
            }
        }
    }

    fn modified_token(path: &Path) -> String {
        fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_else(|| "0".to_string())
    }

    fn installation_token(model: &AiModel, installed_path: &Path) -> String {
        if !model.is_zip_bundle() {
            return Self::modified_token(installed_path);
        }

        let mut parts = vec![Self::modified_token(installed_path)];
        for rel in &model.required_paths {
            let p = installed_path.join(rel);
            parts.push(format!("{}={}", rel, Self::modified_token(&p)));
        }
        parts.join("|")
    }

    fn directory_size(path: &Path) -> u64 {
        let mut total = 0_u64;
        let Ok(entries) = fs::read_dir(path) else { return 0; };
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total = total.saturating_add(meta.len());
                } else if meta.is_dir() {
                    total = total.saturating_add(Self::directory_size(&p));
                }
            }
        }
        total
    }

    fn archive_listing_is_safe(source: &Path) -> io::Result<()> {
        let output = Command::new("tar.exe")
            .arg("-tf")
            .arg(source)
            .output()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Unable to run Windows tar.exe: {e}")))?;
        if !output.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unable to inspect ZIP package: {}", String::from_utf8_lossy(&output.stderr).trim()),
            ));
        }

        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let item = Path::new(line.trim());
            if item.as_os_str().is_empty() {
                continue;
            }
            if item.is_absolute()
                || item.components().any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "ZIP package contains an unsafe path"));
            }
        }
        Ok(())
    }

    fn extract_zip(source: &Path, destination: &Path) -> io::Result<()> {
        Self::archive_listing_is_safe(source)?;
        fs::create_dir_all(destination)?;
        let output = Command::new("tar.exe")
            .arg("-xf")
            .arg(source)
            .arg("-C")
            .arg(destination)
            .output()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Unable to run Windows tar.exe: {e}")))?;
        if !output.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ZIP extraction failed: {}", String::from_utf8_lossy(&output.stderr).trim()),
            ));
        }
        Ok(())
    }

    fn find_bundle_root(root: &Path, required_paths: &[String], depth: usize) -> Option<PathBuf> {
        if required_paths.iter().all(|rel| root.join(rel).exists()) {
            return Some(root.to_path_buf());
        }
        if depth == 0 {
            return None;
        }
        let entries = fs::read_dir(root).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = Self::find_bundle_root(&path, required_paths, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn marker_path(installed_path: &Path) -> PathBuf {
        installed_path.join(".ai-model-package.sha512")
    }

    fn marker_matches(model: &AiModel, installed_path: &Path) -> bool {
        let Some((kind, expected)) = model.expected_hash() else { return false; };
        if kind != HashKind::Sha512 {
            return false;
        }
        fs::read_to_string(Self::marker_path(installed_path))
            .map(|value| value.trim().eq_ignore_ascii_case(expected))
            .unwrap_or(false)
    }

    pub fn reconcile<F>(
        root: &Path,
        manifest: &ManifestManager,
        mut progress: F,
    ) -> io::Result<ReconcileResult>
    where
        F: FnMut(String, f32),
    {
        if !root.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Folder not found"));
        }

        let store = StorageRegistry::open()?;
        store
            .app_key
            .set_value("ModelRoot", &root.to_string_lossy().to_string())?;

        let mut installed = Vec::new();
        let mut total_bytes = 0_u64;
        let mut invalid_models = 0_usize;
        let mut verified_ids = HashSet::new();
        let model_count = manifest.models().len().max(1) as f32;

        for (idx, model) in manifest.models().iter().enumerate() {
            let base_progress = idx as f32 / model_count;
            progress(format!("Checking {}...", model.name), base_progress);

            let installed_path = model.installed_path(root);
            if model.is_zip_bundle() {
                if !installed_path.is_dir() {
                    let _ = store.remove_model(&model.id);
                    continue;
                }
                if !model.required_paths_present(&installed_path) || !Self::marker_matches(model, &installed_path) {
                    invalid_models = invalid_models.saturating_add(1);
                    // Keep any previous registry record so a damaged bundle does
                    // not become trusted again on the next refresh merely because
                    // its cache entry disappeared. Reinstalling refreshes the token.
                    continue;
                }

                let metadata = fs::metadata(&installed_path)?;
                let token = Self::installation_token(model, &installed_path);
                let cached = store.read_model(&model.id)?;
                let is_cache_valid = cached
                    .as_ref()
                    .map(|record| Self::cache_is_valid(record, model, &installed_path, &token))
                    .unwrap_or(false);

                if !is_cache_valid {
                    // The bundle was cryptographically verified before extraction. If
                    // required files changed afterwards, the token changes and we no
                    // longer silently claim integrity. A matching installer marker lets
                    // us restore the registry after a registry-only reset.
                    if let Some(record) = cached.as_ref() {
                        if record.last_write_token != token {
                            invalid_models = invalid_models.saturating_add(1);
                            continue;
                        }
                    }
                    let Some((kind, expected)) = model.expected_hash() else {
                        invalid_models = invalid_models.saturating_add(1);
                        continue;
                    };
                    store.write_model(model, &installed_path, &metadata, kind, expected)?;
                    if let Ok((key, _)) = store.models_key.create_subkey(&model.id) {
                        let _ = key.set_value("LastWriteToken", &token);
                    }
                }

                verified_ids.insert(model.id.clone());
                let size = Self::directory_size(&installed_path);
                total_bytes = total_bytes.saturating_add(size);
                installed.push(InstalledModel {
                    name: model.name.clone(),
                    model_type: model.model_type.clone(),
                    used_for: model.used_for.clone(),
                    supports: model.supports.clone(),
                    size_bytes: size,
                    author: model.author.clone(),
                    verification: model.verification_label(),
                });
                continue;
            }

            if !installed_path.is_file() {
                let _ = store.remove_model(&model.id);
                continue;
            }

            let metadata = fs::metadata(&installed_path)?;
            let modified_token = Self::installation_token(model, &installed_path);
            let cached = store.read_model(&model.id)?;
            let is_cache_valid = cached
                .as_ref()
                .map(|record| Self::cache_is_valid(record, model, &installed_path, &modified_token))
                .unwrap_or(false);

            if !is_cache_valid {
                let kind = model
                    .expected_hash()
                    .map(|(kind, _)| kind)
                    .unwrap_or(HashKind::Sha256);
                let actual = Self::compute_hash(&installed_path, kind, |p| {
                    progress(
                        format!("Verifying {} with {}...", model.name, kind.label()),
                        base_progress + p / model_count,
                    );
                })?;

                let hash_value = match Self::validate_single_hash(model, kind, &actual) {
                    Ok(hash) => hash,
                    Err(_) => {
                        invalid_models = invalid_models.saturating_add(1);
                        let _ = store.remove_model(&model.id);
                        continue;
                    }
                };

                store.write_model(model, &installed_path, &metadata, kind, &hash_value)?;
            }

            verified_ids.insert(model.id.clone());
            total_bytes = total_bytes.saturating_add(metadata.len());
            installed.push(InstalledModel {
                name: model.name.clone(),
                model_type: model.model_type.clone(),
                used_for: model.used_for.clone(),
                supports: model.supports.clone(),
                size_bytes: metadata.len(),
                author: model.author.clone(),
                verification: model.verification_label(),
            });
        }

        let _ = store.cleanup_orphans(&verified_ids);
        progress("Synchronization complete.".to_string(), 1.0);

        Ok(ReconcileResult {
            installed,
            total_bytes,
            invalid_models,
        })
    }

    pub fn install<F>(
        source: &Path,
        root: &Path,
        model: &AiModel,
        mut progress: F,
    ) -> io::Result<()>
    where
        F: FnMut(String, f32),
    {
        fs::create_dir_all(root)?;

        if model.is_zip_bundle() {
            let Some((kind, expected)) = model.expected_hash() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "ZIP bundles require a published SHA-2 digest"));
            };
            if kind != HashKind::Sha512 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "ZIP bundles in this manifest must use SHA-512 verification"));
            }

            progress("Verifying model package...".to_string(), 0.05);
            let actual = Self::compute_hash(source, kind, |v| {
                progress("Verifying model package...".to_string(), 0.05 + v * 0.45);
            })?;
            Self::validate_single_hash(model, kind, &actual)?;

            let extraction = root.join(format!(".extract-{}", model.id));
            let staging = root.join(format!(".install-{}", model.id));
            if extraction.exists() { let _ = fs::remove_dir_all(&extraction); }
            if staging.exists() { let _ = fs::remove_dir_all(&staging); }

            progress("Extracting model package...".to_string(), 0.55);
            if let Err(e) = Self::extract_zip(source, &extraction) {
                let _ = fs::remove_dir_all(&extraction);
                return Err(e);
            }

            let Some(bundle_root) = Self::find_bundle_root(&extraction, &model.required_paths, 3) else {
                let _ = fs::remove_dir_all(&extraction);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "ZIP package does not contain the files required by this model",
                ));
            };

            progress("Preparing model files...".to_string(), 0.72);
            if bundle_root == extraction {
                fs::rename(&extraction, &staging)?;
            } else {
                fs::rename(&bundle_root, &staging)?;
                let _ = fs::remove_dir_all(&extraction);
            }

            let destination = model.installed_path(root);
            let parent = destination.parent().unwrap_or(root);
            fs::create_dir_all(parent)?;
            let backup = parent.join(format!(".{}.backup", model.id));
            if backup.exists() { let _ = fs::remove_dir_all(&backup); }
            let had_existing = destination.exists();
            if had_existing {
                fs::rename(&destination, &backup)?;
            }

            if let Err(e) = fs::rename(&staging, &destination) {
                if had_existing && backup.exists() {
                    let _ = fs::rename(&backup, &destination);
                }
                let _ = fs::remove_dir_all(&staging);
                return Err(e);
            }

            fs::write(Self::marker_path(&destination), format!("{}\n", expected.to_ascii_lowercase()))?;
            if backup.exists() { let _ = fs::remove_dir_all(&backup); }

            let metadata = fs::metadata(&destination)?;
            let store = StorageRegistry::open()?;
            store.write_model(model, &destination, &metadata, kind, &actual)?;
            let token = Self::installation_token(model, &destination);
            if let Ok((key, _)) = store.models_key.create_subkey(&model.id) {
                let _ = key.set_value("LastWriteToken", &token);
            }

            progress("Model package installed and verified.".to_string(), 1.0);
            return Ok(());
        }

        let destination = root.join(&model.name);
        let temporary = root.join(format!("{}.part", model.name));

        progress("Copying model weights...".to_string(), 0.05);
        let kind = model
            .expected_hash()
            .map(|(kind, _)| kind)
            .unwrap_or(HashKind::Sha256);
        let actual = match Self::copy_file_computing_hash(source, &temporary, kind, |v| {
            progress(
                format!("Copying and verifying {}...", kind.label()),
                0.05 + v * 0.85,
            );
        }) {
            Ok(hash) => hash,
            Err(e) => {
                let _ = fs::remove_file(&temporary);
                return Err(e);
            }
        };

        let hash_value = match Self::validate_single_hash(model, kind, &actual) {
            Ok(hash) => hash,
            Err(e) => {
                let _ = fs::remove_file(&temporary);
                return Err(e);
            }
        };

        let backup = root.join(format!("{}.backup", model.name));
        if backup.exists() {
            let _ = fs::remove_file(&backup);
        }
        let had_existing = destination.exists();
        if had_existing {
            fs::rename(&destination, &backup)?;
        }

        if let Err(e) = fs::rename(&temporary, &destination) {
            if had_existing && backup.exists() {
                let _ = fs::rename(&backup, &destination);
            }
            let _ = fs::remove_file(&temporary);
            return Err(e);
        }
        if backup.exists() {
            let _ = fs::remove_file(&backup);
        }

        let metadata = fs::metadata(&destination)?;
        let store = StorageRegistry::open()?;
        store.write_model(model, &destination, &metadata, kind, &hash_value)?;

        progress("Model installed and verified.".to_string(), 1.0);
        Ok(())
    }

    pub fn remove(root: &Path, model: &AiModel) -> io::Result<()> {
        let path = model.installed_path(root);
        if path.exists() {
            if path.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
        let store = StorageRegistry::open()?;
        store.remove_model(&model.id)
    }

}

fn installed_model_to_ui(model: InstalledModel) -> ModelInfo {
    ModelInfo {
        name: SharedString::from(model.name),
        model_type: SharedString::from(model.model_type),
        used_for: SharedString::from(model.used_for),
        author: SharedString::from(model.author),
        size_formatted: SharedString::from(format_size(model.size_bytes)),
        verification: SharedString::from(model.verification),
        supports: Rc::new(slint::VecModel::from(
            model
                .supports
                .into_iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        ))
        .into(),
    }
}

fn pending_model_to_ui(model: &AiModel, source_size: u64) -> ModelInfo {
    ModelInfo {
        name: SharedString::from(model.name.clone()),
        model_type: SharedString::from(model.model_type.clone()),
        used_for: SharedString::from(model.used_for.clone()),
        author: SharedString::from(model.author.clone()),
        size_formatted: SharedString::from(format_size(source_size)),
        verification: SharedString::from(model.verification_label()),
        supports: Rc::new(slint::VecModel::from(
            model
                .supports
                .iter()
                .cloned()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        ))
        .into(),
    }
}

fn apply_reconcile_to_ui(ui: &MainWindow, reconciled: ReconcileResult) {
    let count = reconciled.installed.len() as i32;
    let invalid_models = reconciled.invalid_models;
    let rows: Vec<ModelInfo> = reconciled
        .installed
        .into_iter()
        .map(installed_model_to_ui)
        .collect();
    ui.set_models(Rc::new(slint::VecModel::from(rows)).into());
    ui.set_installed_count(count);
    ui.set_total_size_formatted(SharedString::from(format_size(reconciled.total_bytes)));

    let integrity_status = if invalid_models > 0 {
        "Needs attention"
    } else if count > 0 {
        "Models are OK"
    } else {
        "No models"
    };
    ui.set_integrity_status(SharedString::from(integrity_status));
}

// ============================================================================
// Class: AppController
// ============================================================================

pub struct AppController {
    manifest: Arc<ManifestManager>,
    warning: Arc<Mutex<Option<String>>>,
}

impl AppController {
    pub fn new() -> Self {
        let (manifest, warnings) = ManifestManager::load_from_embedded();
        let warn_msg = if warnings.is_empty() { None } else { Some(warnings.join("\n")) };
        Self {
            manifest: Arc::new(manifest),
            warning: Arc::new(Mutex::new(warn_msg)),
        }
    }

    pub fn run(self) -> Result<(), slint::PlatformError> {
        let ui = MainWindow::new()?;
        ui.set_accent_color(Self::get_accent_color());
        ui.set_models(Rc::new(slint::VecModel::from(Vec::<ModelInfo>::new())).into());
        ui.set_installed_count(0);
        ui.set_integrity_status(SharedString::from("No models"));
        ui.set_total_size_formatted(SharedString::from(format_size(0)));
        ui.set_app_version(SharedString::from(env!("CARGO_PKG_VERSION")));

        let location = StorageRegistry::get_saved_location();
        match &location {
            Some(path) => {
                ui.set_current_location(SharedString::from(path.to_string_lossy().to_string()));
                ui.set_show_oobe(false);
            }
            None => {
                ui.set_show_oobe(true);
            }
        }

        self.bind_events(&ui);

        if let Some(path) = location {
            self.trigger_reconcile(&ui, path);
        }

        ui.run()
    }

    fn get_accent_color() -> Color {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(dwm) = hkcu.open_subkey(r#"Software\Microsoft\Windows\DWM"#) {
            if let Ok(color_value) = dwm.get_value::<u32, _>("ColorizationColor") {
                return Color::from_rgb_u8(
                    ((color_value >> 16) & 0xFF) as u8,
                    ((color_value >> 8) & 0xFF) as u8,
                    (color_value & 0xFF) as u8,
                );
            }
        }
        Color::from_rgb_u8(0, 120, 215)
    }

    fn trigger_reconcile(&self, ui: &MainWindow, path: PathBuf) {
        let ui_weak = ui.as_weak();
        let manifest = Arc::clone(&self.manifest);
        let warning = Arc::clone(&self.warning);

        ui.set_progress_text(SharedString::from("Synchronizing AI models..."));
        ui.set_progress_value(0.0);
        ui.set_show_progress(true);

        thread::spawn(move || {
            let res = ModelDeploymentService::reconcile(&path, &manifest, |txt, val| {
                let ui_thread = ui_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_thread.upgrade() {
                        ui.set_progress_text(SharedString::from(txt));
                        ui.set_progress_value(val);
                    }
                });
            });

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_show_progress(false);
                    match res {
                        Ok(reconciled) => {
                            apply_reconcile_to_ui(&ui, reconciled);

                            if let Some(warn) = warning.lock().ok().and_then(|mut w| w.take()) {
                                ui.set_error_text(SharedString::from(warn));
                                ui.set_show_error(true);
                            }
                        }
                        Err(e) => {
                            ui.set_error_text(SharedString::from(format!("Reconciliation failed:\n{e}")));
                            ui.set_show_error(true);
                        }
                    }
                }
            });
        });
    }

    fn bind_events(&self, ui: &MainWindow) {
        ui.on_open_download_models(|| {
            let _ = std::process::Command::new("explorer.exe").arg(DOWNLOAD_MODELS_URL).spawn();
        });

        ui.on_open_link(|| {
            let _ = std::process::Command::new("explorer.exe").arg(GITHUB_URL).spawn();
        });

        {
            let ui_weak = ui.as_weak();
            ui.on_open_folder(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    let path = PathBuf::from(ui.get_current_location().to_string());
                    if path.is_dir() {
                        let _ = std::process::Command::new("explorer.exe").arg(&path).spawn();
                    }
                }
            });
        }

        {
            let ui_weak = ui.as_weak();
            let manifest = Arc::clone(&self.manifest);
            ui.on_refresh_models(move || {
                let Some(ui) = ui_weak.upgrade() else { return; };
                let root = PathBuf::from(ui.get_current_location().to_string());
                if !root.is_dir() {
                    ui.set_error_text(SharedString::from("The configured model folder no longer exists."));
                    ui.set_show_error(true);
                    return;
                }

                ui.set_progress_text(SharedString::from("Refreshing installed models..."));
                ui.set_progress_value(0.0);
                ui.set_show_progress(true);

                let ui_thread = ui.as_weak();
                let m_clone = Arc::clone(&manifest);
                thread::spawn(move || {
                    let res = ModelDeploymentService::reconcile(&root, &m_clone, |text, value| {
                        let ui_progress = ui_thread.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_progress.upgrade() {
                                ui.set_progress_text(SharedString::from(text));
                                ui.set_progress_value(value);
                            }
                        });
                    });

                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            ui.set_show_progress(false);
                            match res {
                                Ok(reconciled) => apply_reconcile_to_ui(&ui, reconciled),
                                Err(e) => {
                                    ui.set_error_text(SharedString::from(format!("Refresh failed:\n{e}")));
                                    ui.set_show_error(true);
                                }
                            }
                        }
                    });
                });
            });
        }

        {
            let ui_weak = ui.as_weak();
            ui.on_request_sysinfo(move || {
                let ui_thread = ui_weak.clone();
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_progress_text(SharedString::from("Inspecting system hardware..."));
                    ui.set_progress_value(0.1);
                    ui.set_show_progress(true);
                }
                thread::spawn(move || {
                    let sysinfo_data = SystemInspector::inspect();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            ui.set_sysinfo_general(SharedString::from(sysinfo_data.general));
                            ui.set_sysinfo_gpu(SharedString::from(sysinfo_data.gpu));
                            ui.set_sysinfo_npu(SharedString::from(sysinfo_data.npu));
                            ui.set_show_progress(false);
                            ui.set_show_sysinfo(true);
                        }
                    });
                });
            });
        }

        {
            let ui_weak = ui.as_weak();
            let manifest = Arc::clone(&self.manifest);
            let warning = Arc::clone(&self.warning);

            ui.on_pick_oobe_folder(move || {
                let Some(ui) = ui_weak.upgrade() else { return; };
                if let Some(folder) = rfd::FileDialog::new().set_title("Choose AI Models Folder").pick_folder() {
                    let _ = StorageRegistry::save_location(&folder);
                    ui.set_current_location(SharedString::from(folder.to_string_lossy().to_string()));
                    ui.set_show_oobe(false);

                    let ui_thread = ui_weak.clone();
                    let m_clone = Arc::clone(&manifest);
                    let w_clone = Arc::clone(&warning);
                    thread::spawn(move || {
                        let res = ModelDeploymentService::reconcile(&folder, &m_clone, |_, _| {});
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_thread.upgrade() {
                                if let Ok(reconciled) = res {
                            apply_reconcile_to_ui(&ui, reconciled);
                                }
                                if let Some(warn) = w_clone.lock().ok().and_then(|mut w| w.take()) {
                                    ui.set_error_text(SharedString::from(warn));
                                    ui.set_show_error(true);
                                }
                            }
                        });
                    });
                }
            });
        }

        {
            let ui_weak = ui.as_weak();
            let manifest = Arc::clone(&self.manifest);

            ui.on_verify_model(move || {
                let Some(ui) = ui_weak.upgrade() else { return; };
                let Some(selected) = rfd::FileDialog::new()
                    .add_filter("AI Model or Package", &["onnx", "zip"])
                    .pick_file()
                else {
                    return;
                };

                ui.set_progress_text(SharedString::from("Identifying and verifying model or package..."));
                ui.set_progress_value(0.0);
                ui.set_show_progress(true);

                let ui_thread = ui.as_weak();
                let m_clone = Arc::clone(&manifest);

                thread::spawn(move || {
                    let source_size = fs::metadata(&selected).map(|m| m.len()).unwrap_or(0);
                    let identify_res = ModelDeploymentService::identify_file(&selected, &m_clone, |v| {
                        let ui_t = ui_thread.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_t.upgrade() {
                                ui.set_progress_value(v);
                            }
                        });
                    });

                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            ui.set_show_progress(false);
                            match identify_res {
                                Ok((model, verification_text)) => {
                                    ui.set_pending_model(pending_model_to_ui(&model, source_size));
                                    ui.set_pending_verification(SharedString::from(verification_text));
                                    ui.set_pending_file_path(SharedString::from(
                                        selected.to_string_lossy().to_string(),
                                    ));
                                    ui.set_show_install_confirm(true);
                                }
                                Err(e) => {
                                    ui.set_error_text(SharedString::from(format!(
                                        "Model/package verification failed:\n{e}"
                                    )));
                                    ui.set_show_error(true);
                                }
                            }
                        }
                    });
                });
            });
        }

        {
            let ui_weak = ui.as_weak();
            let manifest = Arc::clone(&self.manifest);

            ui.on_confirm_install(move || {
                let Some(ui) = ui_weak.upgrade() else { return; };
                let source = PathBuf::from(ui.get_pending_file_path().to_string());
                let root = PathBuf::from(ui.get_current_location().to_string());
                let model_name = ui.get_pending_model().name.to_string();

                let Some(model) = manifest.find_by_name(&model_name).cloned() else {
                    ui.set_show_install_confirm(false);
                    return;
                };

                ui.set_show_install_confirm(false);
                ui.set_progress_text(SharedString::from("Installing model..."));
                ui.set_progress_value(0.0);
                ui.set_show_progress(true);

                let ui_thread = ui.as_weak();
                let m_clone = Arc::clone(&manifest);

                thread::spawn(move || {
                    let install_res = ModelDeploymentService::install(&source, &root, &model, |txt, val| {
                        let ui_t = ui_thread.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_t.upgrade() {
                                ui.set_progress_text(SharedString::from(txt));
                                ui.set_progress_value(val);
                            }
                        });
                    });

                    if let Err(e) = install_res {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_thread.upgrade() {
                                ui.set_show_progress(false);
                                ui.set_error_text(SharedString::from(format!("Installation failed:\n{e}")));
                                ui.set_show_error(true);
                            }
                        });
                        return;
                    }

                    let rec_res = ModelDeploymentService::reconcile(&root, &m_clone, |_, _| {});
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            ui.set_show_progress(false);
                            if let Ok(reconciled) = rec_res {
                            apply_reconcile_to_ui(&ui, reconciled);
                            }
                        }
                    });
                });
            });
        }

        {
            let ui_weak = ui.as_weak();
            let manifest = Arc::clone(&self.manifest);

            ui.on_remove_model(move |model_name| {
                let Some(ui) = ui_weak.upgrade() else { return; };
                let root = PathBuf::from(ui.get_current_location().to_string());
                let Some(model) = manifest.find_by_name(&model_name.to_string()).cloned() else { return; };

                let ui_thread = ui.as_weak();
                let m_clone = Arc::clone(&manifest);

                thread::spawn(move || {
                    let _ = ModelDeploymentService::remove(&root, &model);
                    let rec_res = ModelDeploymentService::reconcile(&root, &m_clone, |_, _| {});
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_thread.upgrade() {
                            if let Ok(reconciled) = rec_res {
                            apply_reconcile_to_ui(&ui, reconciled);
                            }
                        }
                    });
                });
            });
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let app = AppController::new();
    app.run()
}

// ============================================================================
// CUDA Driver Helper Module
// ============================================================================

#[cfg(windows)]
mod cuda_driver {
    use std::ffi::{c_char, c_void};
    use std::os::windows::ffi::OsStrExt;

    type ModuleHandle = *mut c_void;
    type CuDriverGetVersion = unsafe extern "system" fn(*mut i32) -> i32;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(file_name: *const u16) -> ModuleHandle;
        fn GetProcAddress(module: ModuleHandle, procedure_name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: ModuleHandle) -> i32;
    }

    pub fn query_cuda_driver_version() -> Result<String, String> {
        unsafe {
            let library_name: Vec<u16> = std::ffi::OsStr::new("nvcuda.dll")
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let module = LoadLibraryW(library_name.as_ptr());

            if module.is_null() {
                return Err("nvcuda.dll not loaded".to_string());
            }

            let procedure = GetProcAddress(module, b"cuDriverGetVersion\0".as_ptr() as *const c_char);
            if procedure.is_null() {
                let _ = FreeLibrary(module);
                return Err("cuDriverGetVersion not exported".to_string());
            }

            let get_version: CuDriverGetVersion = std::mem::transmute(procedure);
            let mut version_encoded = 0_i32;
            let status = get_version(&mut version_encoded);
            let _ = FreeLibrary(module);

            if status != 0 || version_encoded <= 0 {
                return Err(format!("CUDA query code {status}"));
            }

            Ok(format!("{}.{}", version_encoded / 1000, (version_encoded % 1000) / 10))
        }
    }
}