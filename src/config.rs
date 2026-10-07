use crate::machine::{Letter, Variant};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_ACCENTS: &str = include_str!("../config/accents.toml");

#[derive(Debug)]
pub struct Config {
    pub hold_ms: u64,
    pub letters: Vec<Letter>,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    hold_ms: u64,
    letter: Vec<LetterConfig>,
}

#[derive(Debug, Deserialize)]
struct LetterConfig {
    key: String,
    variant: Vec<VariantConfig>,
}

#[derive(Debug, Deserialize)]
struct VariantConfig {
    text: String,
    steps: Vec<String>,
}

pub fn config_path(arg: Option<&str>) -> PathBuf {
    if let Some(path) = arg {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("macmykeys").join("accents.toml")
}

pub fn load(path: &Path) -> Result<Config, String> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        fs::write(path, DEFAULT_ACCENTS).map_err(|err| err.to_string())?;
    }
    let raw = fs::read_to_string(path).map_err(|err| err.to_string())?;
    parse(&raw)
}

pub fn parse(raw: &str) -> Result<Config, String> {
    let file: FileConfig = toml::from_str(raw).map_err(|err| err.to_string())?;
    if file.hold_ms < 2 {
        return Err("hold_ms must be at least 2".into());
    }
    let mut letters = Vec::new();
    for letter in file.letter {
        let code = key_code(&letter.key).ok_or_else(|| format!("unknown key '{}'", letter.key))?;
        if letter.variant.is_empty() || letter.variant.len() > 9 {
            return Err(format!(
                "'{}' needs between 1 and 9 accents",
                letter.key
            ));
        }
        let mut variants = Vec::new();
        for variant in letter.variant {
            if variant.steps.is_empty() {
                return Err(format!("'{}' has an accent with no compose steps", letter.key));
            }
            for step in &variant.steps {
                if chord(step, false).is_none() {
                    return Err(format!("unknown compose step '{step}'"));
                }
            }
            variants.push(Variant {
                text: variant.text,
                steps: variant.steps,
            });
        }
        letters.push(Letter { code, variants });
    }
    Ok(Config {
        hold_ms: file.hold_ms,
        letters,
    })
}

pub fn key_code(name: &str) -> Option<u16> {
    Some(match name {
        "a" => 30,
        "b" => 48,
        "c" => 46,
        "d" => 32,
        "e" => 18,
        "f" => 33,
        "g" => 34,
        "h" => 35,
        "i" => 23,
        "j" => 36,
        "k" => 37,
        "l" => 38,
        "m" => 50,
        "n" => 49,
        "o" => 24,
        "p" => 25,
        "q" => 16,
        "r" => 19,
        "s" => 31,
        "t" => 20,
        "u" => 22,
        "v" => 47,
        "w" => 17,
        "x" => 45,
        "y" => 21,
        "z" => 44,
        _ => return None,
    })
}

/// Keys to press, in order, for one compose step. Letter steps pick up Shift
/// when the original letter was uppercase.
pub fn chord(step: &str, upper: bool) -> Option<Vec<u16>> {
    if let Some(code) = key_code(step) {
        return Some(if upper {
            vec![crate::machine::KEY_LEFTSHIFT, code]
        } else {
            vec![code]
        });
    }
    Some(match step {
        "grave" => vec![41],
        "apostrophe" => vec![40],
        "asciicircum" => vec![crate::machine::KEY_LEFTSHIFT, 7],
        "quotedbl" => vec![crate::machine::KEY_LEFTSHIFT, 40],
        "asciitilde" => vec![crate::machine::KEY_LEFTSHIFT, 41],
        "minus" => vec![12],
        "period" => vec![52],
        "comma" => vec![51],
        "slash" => vec![53],
        "less" => vec![crate::machine::KEY_LEFTSHIFT, 51],
        _ => return None,
    })
}

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    runtime.join("macmykeys.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_e_row_puts_dots_on_four() {
        let config = parse(DEFAULT_ACCENTS).unwrap();
        let e = config
            .letters
            .iter()
            .find(|letter| letter.code == key_code("e").unwrap())
            .unwrap();
        assert_eq!(e.variants[3].text, "ë");
        assert_eq!(
            e.variants[3].steps,
            vec!["quotedbl".to_string(), "e".to_string()]
        );
        assert_eq!(config.hold_ms, 300);
    }
}
