//! Config directory initialization and stub delivery.
//!
//! `init_config()` scaffolds the full config directory (config.lua template,
//! .luarc.json, type stubs). `ensure_stubs()` updates only the type stubs
//! on every launch.

use crate::stubs;
use std::io;
use std::path::{Path, PathBuf};

/// Result of `init_config()` describing what was created or updated.
#[derive(Debug)]
#[non_exhaustive]
pub struct InitResult {
    /// The config directory path.
    pub config_dir: PathBuf,
    /// `true` if `config.lua` was created (was absent).
    pub created_config: bool,
    /// `true` if `.luarc.json` was created (was absent).
    pub created_luarc: bool,
    /// `true` if `types/oakterm.lua` was written (created or updated).
    pub updated_stubs: bool,
}

/// Create or update the config directory with all scaffolding.
///
/// - Creates the config directory if missing.
/// - Creates `config.lua` from template **only if absent** (never overwrites).
/// - Creates `.luarc.json` **only if absent** (never overwrites).
/// - Always writes `types/oakterm.lua` (overwritten on upgrade).
///
/// # Errors
///
/// Returns an I/O error if directory creation or file writes fail.
pub fn init_config(config_dir: &Path) -> io::Result<InitResult> {
    std::fs::create_dir_all(config_dir)?;

    let created_config = write_if_absent(&config_dir.join("config.lua"), stubs::CONFIG_TEMPLATE)?;
    let created_luarc = write_if_absent(&config_dir.join(".luarc.json"), stubs::LUARC_JSON)?;
    let updated_stubs = write_stubs(config_dir)?;

    Ok(InitResult {
        config_dir: config_dir.to_path_buf(),
        created_config,
        created_luarc,
        updated_stubs,
    })
}

/// Ensure type stubs are up to date. Called on every launch.
///
/// Only writes `types/oakterm.lua` if the content differs from the embedded
/// version. Does **not** create `config.lua` or `.luarc.json`.
///
/// # Errors
///
/// Returns an I/O error if the write fails. Missing config directory is
/// silently ignored (no config dir = nothing to update).
pub fn ensure_stubs(config_dir: &Path) -> io::Result<()> {
    if !config_dir.is_dir() {
        return Ok(());
    }
    write_stubs(config_dir)?;
    Ok(())
}

/// Write `types/oakterm.lua`, returning `true` if content was written.
fn write_stubs(config_dir: &Path) -> io::Result<bool> {
    let types_dir = config_dir.join("types");
    std::fs::create_dir_all(&types_dir)?;

    let stub_path = types_dir.join("oakterm.lua");

    // Skip write if content is identical (avoids unnecessary mtime changes).
    if let Ok(existing) = std::fs::read_to_string(&stub_path) {
        if existing == stubs::OAKTERM_LUA_STUB {
            return Ok(false);
        }
    }

    atomic_write(&stub_path, stubs::OAKTERM_LUA_STUB)?;
    Ok(true)
}

/// Write a file only if it does not already exist. Returns `true` if created.
///
/// Uses `create_new(true)` for atomic create-if-absent (no TOCTOU race).
fn write_if_absent(path: &Path, content: &str) -> io::Result<bool> {
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            f.write_all(content.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Write content to a file atomically via a temporary file + rename.
///
/// On Unix, `rename` atomically replaces the target. On Windows, `rename`
/// fails if the target exists, so we remove the target first (not atomic,
/// but the content is regenerated on every launch).
fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    // On Windows, remove the target first since rename cannot overwrite.
    if cfg!(windows) && path.exists() {
        let _ = std::fs::remove_file(path);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create tempdir")
    }

    #[test]
    fn init_creates_all_files() {
        let dir = tmpdir();
        let config_dir = dir.path().join("oakterm");

        let result = init_config(&config_dir).unwrap();

        assert!(config_dir.join("config.lua").exists());
        assert!(config_dir.join(".luarc.json").exists());
        assert!(config_dir.join("types").join("oakterm.lua").exists());
        assert!(result.created_config);
        assert!(result.created_luarc);
        assert!(result.updated_stubs);
    }

    #[test]
    fn init_preserves_existing_config() {
        let dir = tmpdir();
        let config_dir = dir.path().join("oakterm");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.lua"), "-- my custom config").unwrap();

        let result = init_config(&config_dir).unwrap();

        assert!(!result.created_config);
        let content = std::fs::read_to_string(config_dir.join("config.lua")).unwrap();
        assert_eq!(content, "-- my custom config");
    }

    #[test]
    fn init_preserves_existing_luarc() {
        let dir = tmpdir();
        let config_dir = dir.path().join("oakterm");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join(".luarc.json"), r#"{"custom": true}"#).unwrap();

        let result = init_config(&config_dir).unwrap();

        assert!(!result.created_luarc);
        let content = std::fs::read_to_string(config_dir.join(".luarc.json")).unwrap();
        assert_eq!(content, r#"{"custom": true}"#);
    }

    #[test]
    fn init_overwrites_stubs() {
        let dir = tmpdir();
        let config_dir = dir.path().join("oakterm");
        let types_dir = config_dir.join("types");
        std::fs::create_dir_all(&types_dir).unwrap();
        std::fs::write(types_dir.join("oakterm.lua"), "-- old stubs").unwrap();

        let result = init_config(&config_dir).unwrap();

        assert!(result.updated_stubs);
        let content = std::fs::read_to_string(types_dir.join("oakterm.lua")).unwrap();
        assert_eq!(content, stubs::OAKTERM_LUA_STUB);
    }

    #[test]
    fn init_creates_directory_if_missing() {
        let dir = tmpdir();
        let config_dir = dir.path().join("deep").join("nested").join("oakterm");

        let result = init_config(&config_dir).unwrap();

        assert!(config_dir.is_dir());
        assert!(result.created_config);
    }

    #[test]
    fn init_result_flags_correct_when_all_exist() {
        let dir = tmpdir();
        let config_dir = dir.path().join("oakterm");

        // First init creates everything.
        let r1 = init_config(&config_dir).unwrap();
        assert!(r1.created_config);
        assert!(r1.created_luarc);
        assert!(r1.updated_stubs);

        // Second init: stubs unchanged, user files preserved.
        let r2 = init_config(&config_dir).unwrap();
        assert!(!r2.created_config);
        assert!(!r2.created_luarc);
        assert!(!r2.updated_stubs); // Content identical, skip write.
    }

    #[test]
    fn ensure_stubs_only_writes_types() {
        let dir = tmpdir();
        let config_dir = dir.path().join("oakterm");
        std::fs::create_dir_all(&config_dir).unwrap();

        ensure_stubs(&config_dir).unwrap();

        assert!(config_dir.join("types").join("oakterm.lua").exists());
        assert!(!config_dir.join("config.lua").exists());
        assert!(!config_dir.join(".luarc.json").exists());
    }

    #[test]
    fn ensure_stubs_skips_if_unchanged() {
        let dir = tmpdir();
        let config_dir = dir.path().join("oakterm");
        std::fs::create_dir_all(&config_dir).unwrap();

        // First call writes.
        ensure_stubs(&config_dir).unwrap();
        let stub_path = config_dir.join("types").join("oakterm.lua");
        let mtime1 = std::fs::metadata(&stub_path).unwrap().modified().unwrap();

        // Small delay to ensure mtime would differ on a re-write.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Second call skips (content identical).
        ensure_stubs(&config_dir).unwrap();
        let mtime2 = std::fs::metadata(&stub_path).unwrap().modified().unwrap();
        assert_eq!(
            mtime1, mtime2,
            "mtime should not change when content is identical"
        );
    }

    #[test]
    fn ensure_stubs_overwrites_outdated_content() {
        let dir = tmpdir();
        let config_dir = dir.path().join("oakterm");
        let types_dir = config_dir.join("types");
        std::fs::create_dir_all(&types_dir).unwrap();
        std::fs::write(types_dir.join("oakterm.lua"), "-- old version stubs").unwrap();

        ensure_stubs(&config_dir).unwrap();

        let content = std::fs::read_to_string(types_dir.join("oakterm.lua")).unwrap();
        assert_eq!(content, stubs::OAKTERM_LUA_STUB);
    }

    #[test]
    fn ensure_stubs_noop_if_no_config_dir() {
        let dir = tmpdir();
        let config_dir = dir.path().join("nonexistent");
        // Should not error — just silently skip.
        ensure_stubs(&config_dir).unwrap();
        assert!(!config_dir.exists());
    }

    #[test]
    fn stub_content_valid_lua() {
        let lua = mlua::Lua::new();
        lua.load(stubs::OAKTERM_LUA_STUB)
            .exec()
            .expect("type stub must be valid Lua syntax");
    }

    #[test]
    fn luarc_valid_json() {
        let v: serde_json::Value =
            serde_json::from_str(stubs::LUARC_JSON).expect(".luarc.json must be valid JSON");
        let libs = v["workspace"]["library"]
            .as_array()
            .expect("workspace.library must be an array");
        assert!(
            libs.iter().any(|l| l.as_str() == Some("types")),
            "workspace.library must include \"types\""
        );
    }

    #[test]
    fn config_template_valid_lua() {
        let lua = mlua::Lua::new();
        lua.load(stubs::CONFIG_TEMPLATE)
            .exec()
            .expect("config template must be valid Lua syntax");
    }

    #[test]
    fn config_template_content() {
        // Template should contain common config patterns.
        assert!(stubs::CONFIG_TEMPLATE.contains("oakterm.config.font_family"));
        assert!(stubs::CONFIG_TEMPLATE.contains("oakterm.config.font_size"));
        assert!(stubs::CONFIG_TEMPLATE.contains("oakterm.keybind"));
        assert!(stubs::CONFIG_TEMPLATE.contains("oakterm.os()"));
    }

    #[test]
    fn stub_covers_all_config_keys() {
        // Every key in SCHEMA should appear in the stub.
        for def in crate::schema::SCHEMA {
            assert!(
                stubs::OAKTERM_LUA_STUB.contains(def.name),
                "stub missing config key '{}'",
                def.name
            );
        }
    }

    #[test]
    fn stub_covers_all_events() {
        for event in crate::event::KNOWN_EVENTS {
            assert!(
                stubs::OAKTERM_LUA_STUB.contains(event),
                "stub missing event '{event}'"
            );
        }
    }
}
