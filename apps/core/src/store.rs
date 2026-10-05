//! Storage for schedule records. Keys are (user, id); values are
//! opaque encrypted strings from `crypto`. Shells provide the impl:
//! filesystem for the server, D1 for the Cloudflare worker.

use crate::http::BoxFut;
use crate::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleRecord {
    pub id: String,
    pub payload: String,
}

pub trait Store: Send + Sync {
    fn list_users(&self) -> BoxFut<Result<Vec<String>, Error>>;
    fn list(&self, user: &str) -> BoxFut<Result<Vec<ScheduleRecord>, Error>>;
    fn get(&self, user: &str, id: &str) -> BoxFut<Result<Option<String>, Error>>;
    fn put(&self, user: &str, id: &str, payload: &str) -> BoxFut<Result<(), Error>>;
    fn delete(&self, user: &str, id: &str) -> BoxFut<Result<(), Error>>;
}

/// Filesystem impl: one file per record at `<root>/<user>/<id>.json`.
/// Native only (server, CLI-less tests); workers use D1.
#[cfg(not(target_arch = "wasm32"))]
pub mod file {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    pub struct FileStore {
        root: PathBuf,
    }

    impl FileStore {
        pub fn new(root: impl Into<PathBuf>) -> Self {
            FileStore { root: root.into() }
        }

        fn check_key(k: &str) -> Result<(), Error> {
            let ok = !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if ok {
                Ok(())
            } else {
                Err(Error::new(400, format!("unsafe store key: {k}")))
            }
        }

        fn dir(&self, user: &str) -> Result<PathBuf, Error> {
            Self::check_key(user)?;
            Ok(self.root.join(user))
        }

        fn path(&self, user: &str, id: &str) -> Result<PathBuf, Error> {
            Ok(self.dir(user)?.join(format!("{id}.json")))
        }
    }

    impl Store for FileStore {
        fn list_users(&self) -> BoxFut<Result<Vec<String>, Error>> {
            let out = match fs::read_dir(&self.root) {
                Ok(rd) => rd
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect(),
                // no store root yet = no users
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                // a real outage must NOT look like "no users":
                // run_due_schedules would silently no-op
                Err(e) => {
                    return Box::pin(std::future::ready(Err(io_err(e))));
                }
            };
            Box::pin(std::future::ready(Ok(out)))
        }

        fn list(&self, user: &str) -> BoxFut<Result<Vec<ScheduleRecord>, Error>> {
            let res = (|| {
                let dir = self.dir(user)?;
                let mut names: Vec<String> = match fs::read_dir(&dir) {
                    Ok(rd) => rd
                        .flatten()
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter(|n| n.ends_with(".json"))
                        .collect(),
                    // user has no directory yet = no schedules
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                    Err(e) => return Err(io_err(e)),
                };
                names.sort();
                let mut out = Vec::new();
                for n in names {
                    let payload = fs::read_to_string(dir.join(&n)).map_err(io_err)?;
                    let id = n.strip_suffix(".json").unwrap_or(&n).to_string();
                    out.push(ScheduleRecord { id, payload });
                }
                Ok(out)
            })();
            Box::pin(std::future::ready(res))
        }

        fn get(&self, user: &str, id: &str) -> BoxFut<Result<Option<String>, Error>> {
            let res = (|| {
                Self::check_key(id)?;
                match fs::read_to_string(self.path(user, id)?) {
                    Ok(s) => Ok(Some(s)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(io_err(e)),
                }
            })();
            Box::pin(std::future::ready(res))
        }

        fn put(&self, user: &str, id: &str, payload: &str) -> BoxFut<Result<(), Error>> {
            let res = (|| {
                Self::check_key(id)?;
                let dir = self.dir(user)?;
                fs::create_dir_all(&dir).map_err(io_err)?;
                let tmp = dir.join(format!(".{id}.tmp"));
                fs::write(&tmp, payload).map_err(io_err)?;
                fs::rename(&tmp, self.path(user, id)?).map_err(io_err)?;
                Ok(())
            })();
            Box::pin(std::future::ready(res))
        }

        fn delete(&self, user: &str, id: &str) -> BoxFut<Result<(), Error>> {
            let res = (|| {
                Self::check_key(id)?;
                match fs::remove_file(self.path(user, id)?) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(io_err(e)),
                }
            })();
            Box::pin(std::future::ready(res))
        }
    }

    fn io_err(e: std::io::Error) -> Error {
        Error::new(500, format!("store: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_store_roundtrip() {
        let root = std::env::temp_dir().join(format!("gitlarp-store-{}", fastrand::u32(..)));
        let s = file::FileStore::new(&root);
        assert_eq!(crate::block_on(s.list_users()).unwrap(), Vec::<String>::new());
        assert_eq!(crate::block_on(s.list("u")).unwrap(), Vec::<ScheduleRecord>::new());
        assert_eq!(crate::block_on(s.get("u", "a")).unwrap(), None);
        crate::block_on(s.put("u", "a", "payload")).unwrap();
        assert_eq!(crate::block_on(s.get("u", "a")).unwrap(), Some("payload".into()));
        assert_eq!(
            crate::block_on(s.list_users()).unwrap(),
            vec!["u".to_string()]
        );
        let listed = crate::block_on(s.list("u")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "a");
        crate::block_on(s.delete("u", "a")).unwrap();
        assert_eq!(crate::block_on(s.get("u", "a")).unwrap(), None);
        // unsafe keys rejected
        assert!(crate::block_on(s.get("../evil", "a")).is_err());
        assert!(crate::block_on(s.get("u", "a/b")).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_store_outage_surfaces_not_empty() {
        // root pointing at a regular file: read_dir fails with a real
        // error: must surface as 500, not read as "no users"
        let file = std::env::temp_dir().join(format!("gitlarp-store-file-{}", fastrand::u32(..)));
        std::fs::write(&file, "x").unwrap();
        let s = file::FileStore::new(&file);
        let err = crate::block_on(s.list_users()).unwrap_err();
        assert_eq!(err.status, 500);
        assert!(crate::block_on(s.list("u")).is_err());
        std::fs::remove_file(&file).ok();
    }
}
