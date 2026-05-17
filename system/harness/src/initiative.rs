use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Initiative {
    pub id: String,
    pub name: String,
    #[serde(default = "default_open")]
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub horizon: Option<String>,
    #[serde(default)]
    pub key_results: Vec<serde_yaml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(flatten)]
    pub extra: serde_yaml::Mapping,
}

fn default_open() -> String {
    "open".to_string()
}

fn initiatives_dir(hex_dir: &Path) -> PathBuf {
    hex_dir.join("initiatives")
}

fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn find_initiative_file(dir: &Path, id: &str) -> Option<PathBuf> {
    let direct = dir.join(format!("{id}.yaml"));
    if direct.exists() {
        return Some(direct);
    }
    let Ok(entries) = fs::read_dir(dir) else { return None };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let fname = name.to_string_lossy();
        if fname.ends_with(".yaml") && !fname.ends_with(".lock.yaml") {
            if fname.starts_with(id) {
                return Some(entry.path());
            }
        }
    }
    None
}

fn load_initiative(path: &Path) -> Result<Initiative, String> {
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_yaml::from_str(&contents)
        .map_err(|e| format!("parse error in {}: {e}", path.display()))
}

fn save_initiative(initiative: &Initiative, path: &Path) -> Result<(), String> {
    let tmp = path.with_extension("yaml.tmp");
    let contents = serde_yaml::to_string(initiative)
        .map_err(|e| format!("serialize error: {e}"))?;
    fs::write(&tmp, &contents).map_err(|e| format!("write error: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("rename error: {e}"))?;
    Ok(())
}

fn list_initiative_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return vec![] };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.ends_with(".yaml") && !s.ends_with(".lock.yaml")
        })
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths
}

pub fn run_list(hex_dir: &Path, status_filter: Option<&str>) {
    let dir = initiatives_dir(hex_dir);
    if !dir.exists() {
        println!("No initiatives found (directory does not exist).");
        return;
    }
    let files = list_initiative_files(&dir);
    if files.is_empty() {
        println!("No initiatives found.");
        return;
    }
    let filter = status_filter.unwrap_or("all");
    println!("{:<32} {:<12} {:<12} {}", "ID", "STATUS", "HORIZON", "NAME");
    println!("{}", "─".repeat(80));
    let mut shown = 0;
    for path in &files {
        match load_initiative(path) {
            Ok(init) => {
                if filter != "all" && init.status != filter {
                    continue;
                }
                let horizon = init.horizon.as_deref().unwrap_or("—");
                println!(
                    "{:<32} {:<12} {:<12} {}",
                    &init.id, &init.status, horizon, &init.name
                );
                shown += 1;
            }
            Err(e) => eprintln!("WARN: {e}"),
        }
    }
    if shown == 0 {
        println!("(no initiatives match filter: {filter})");
    }
}

pub fn run_show(hex_dir: &Path, id: &str) {
    let dir = initiatives_dir(hex_dir);
    let Some(path) = find_initiative_file(&dir, id) else {
        eprintln!("ERROR: initiative not found: {id}");
        std::process::exit(1);
    };
    match load_initiative(&path) {
        Ok(init) => {
            println!("{}", serde_yaml::to_string(&init).unwrap_or_default());
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

pub fn run_create(hex_dir: &Path, name: &str, status: &str) {
    let dir = initiatives_dir(hex_dir);
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("ERROR: cannot create initiatives dir: {e}");
        std::process::exit(1);
    }
    let id = slugify(name);
    if id.is_empty() {
        eprintln!("ERROR: name produces empty id after slugification");
        std::process::exit(1);
    }
    let path = dir.join(format!("{id}.yaml"));
    if path.exists() {
        eprintln!("ERROR: initiative already exists: {id}");
        std::process::exit(1);
    }
    let today = today();
    let init = Initiative {
        id: id.clone(),
        name: name.to_string(),
        status: status.to_string(),
        goal: None,
        owner: None,
        horizon: None,
        key_results: vec![],
        created: Some(today.clone()),
        updated: Some(today),
        extra: serde_yaml::Mapping::new(),
    };
    if let Err(e) = save_initiative(&init, &path) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
    println!("Created initiative: {id} (status: {status})");
    println!("  → {}", path.display());
}

pub fn run_update(hex_dir: &Path, id: &str, new_status: &str) {
    let dir = initiatives_dir(hex_dir);
    let Some(path) = find_initiative_file(&dir, id) else {
        eprintln!("ERROR: initiative not found: {id}");
        std::process::exit(1);
    };
    let mut init = match load_initiative(&path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    };
    let old_status = init.status.clone();
    init.status = new_status.to_string();
    init.updated = Some(today());
    if let Err(e) = save_initiative(&init, &path) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
    println!("Updated {id}: {old_status} → {new_status}");
}

pub fn run_close(hex_dir: &Path, id: &str) {
    run_update(hex_dir, id, "closed");
}
