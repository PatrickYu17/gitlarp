use std::fs;
use std::path::{Path, PathBuf};

/// The persisted schedule (config.toml). Everything else the CLI
/// needs (auth, remote, repo) lives in gh + env.
#[derive(Clone)]
pub struct Schedule {
    pub from: Option<String>,
    pub to: Option<String>,
    pub min: u32,
    pub max: u32,
    pub weekends: bool,
    pub catchup: u32,
}

#[derive(Clone, Default)]
pub struct Config {
    pub schedule: Option<Schedule>,
}

pub fn load(path: &Path) -> Result<Config, String> {
    let mut c = Config::default();
    if !path.exists() {
        return Ok(c);
    }
    let text = fs::read_to_string(path).map_err(|e| crate::t!("cfg.read", path = format!("{:?}", path), e = e))?;
    let mut s = Schedule { from: None, to: None, min: 0, max: 0, weekends: true, catchup: 14 };
    let mut any = false;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(crate::t!("cfg.badline", path = format!("{:?}", path), line = i + 1));
        };
        let v = v.trim().trim_matches('"');
        let err = |line: usize, msg: String| crate::t!("cfg.bad", path = format!("{:?}", path), line = line, msg = msg);
        match k.trim() {
            // legacy keys from the local-git era: accepted, ignored
            "remote" | "email" | "branch" => {}
            "schedule_min" => {
                s.min = v.parse().map_err(|_| err(i + 1, crate::t!("cfg.bad_min", v = v)))?;
                any = true;
            }
            "schedule_max" => {
                s.max = v.parse().map_err(|_| err(i + 1, crate::t!("cfg.bad_max", v = v)))?;
                any = true;
            }
            "schedule_from" => {
                s.from = Some(v.to_string());
                any = true;
            }
            "schedule_to" => {
                s.to = Some(v.to_string());
                any = true;
            }
            "schedule_weekends" => match v {
                "true" => {
                    s.weekends = true;
                    any = true;
                }
                "false" => {
                    s.weekends = false;
                    any = true;
                }
                _ => return Err(err(i + 1, crate::t!("cfg.bad_weekends", v = v))),
            },
            "schedule_catchup" => {
                s.catchup = v.parse().map_err(|_| err(i + 1, crate::t!("cfg.bad_catchup", v = v)))?;
                any = true;
            }
            _ => {}
        }
    }
    if any {
        if s.min == 0 || s.max == 0 {
            return Err(crate::t!("cfg.sched_minmax", path = format!("{:?}", path)));
        }
        if s.min > s.max {
            return Err(crate::t!("cfg.sched_order", path = format!("{:?}", path)));
        }
        c.schedule = Some(s);
    }
    Ok(c)
}

pub fn save(path: &Path, c: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut text = String::new();
    if let Some(s) = &c.schedule {
        text.push_str(&format!(
            "schedule_min = {}\nschedule_max = {}\nschedule_weekends = {}\nschedule_catchup = {}\n",
            s.min, s.max, s.weekends, s.catchup
        ));
        if let Some(f) = &s.from {
            text.push_str(&format!("schedule_from = \"{f}\"\n"));
        }
        if let Some(t) = &s.to {
            text.push_str(&format!("schedule_to = \"{t}\"\n"));
        }
    }
    // tmp + rename, like the core FileStore: a crash mid-write can
    // never truncate an existing config to zero bytes.
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    if let Err(e) = fs::write(&tmp, &text) {
        return Err(e.to_string());
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            fs::remove_file(&tmp).ok(); // best-effort: no tmp litter
            Err(e.to_string())
        }
    }
}
