//! SQLite station storage, mirroring the TypeORM store (SQLite flavour).

use std::{
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct StationInfo {
    pub duid: String,
    pub name: String,
    pub platform: String,
    pub quasar_config: Value,
    pub network_info: Value,
}

pub struct SqliteStorage {
    connection: Mutex<Connection>,
}

impl SqliteStorage {
    pub fn open(path: &str) -> Result<Self> {
        let connection =
            Connection::open(path).with_context(|| format!("opening SQLite database {path}"))?;
        Self::init(connection)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(connection: Connection) -> Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS stations (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 platform TEXT NOT NULL,
                 quasar_config TEXT NOT NULL,
                 network_info TEXT NOT NULL,
                 glagol_security TEXT NOT NULL
             );",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("station storage lock is poisoned"))
    }

    pub fn get_or_create(&self, duid: &str, platform: &str) -> Result<StationInfo> {
        let connection = self.connection()?;
        if let Some(info) = find(&connection, duid)? {
            return Ok(info);
        }

        let security = uuid::Uuid::new_v4().to_string();
        connection.execute(
            "INSERT INTO stations
                 (id, name, platform, quasar_config, network_info, glagol_security)
             VALUES (?1, '', ?2, ?3, '{}', ?4)",
            params![duid, platform, default_config().to_string(), security],
        )?;
        find(&connection, duid)?.ok_or_else(|| anyhow!("station {duid} was not created"))
    }

    pub fn list(&self) -> Result<Vec<StationInfo>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, name, platform, quasar_config, network_info
             FROM stations ORDER BY id",
        )?;
        let rows = statement.query_map([], row_to_info)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn update_name_and_config(
        &self,
        duid: &str,
        name: Option<&str>,
        patch: Option<&Value>,
    ) -> Result<Option<StationInfo>> {
        let connection = self.connection()?;
        let Some(mut info) = find(&connection, duid)? else {
            return Ok(None);
        };
        if let Some(name) = name {
            info.name = name.to_owned();
        }
        if let Some(patch) = patch {
            merge_json(&mut info.quasar_config, patch);
        }
        connection.execute(
            "UPDATE stations SET name = ?1, quasar_config = ?2 WHERE id = ?3",
            params![info.name, info.quasar_config.to_string(), duid],
        )?;
        Ok(Some(info))
    }

    pub fn update_network_info(
        &self,
        duid: &str,
        platform: &str,
        network_info: &Value,
    ) -> Result<StationInfo> {
        let existing = {
            let connection = self.connection()?;
            find(&connection, duid)?
        };

        let mut info = match existing {
            Some(info) => info,
            None => self.get_or_create(duid, platform)?,
        };
        info.network_info = network_info.clone();

        let connection = self.connection()?;
        connection.execute(
            "UPDATE stations SET network_info = ?1 WHERE id = ?2",
            params![info.network_info.to_string(), duid],
        )?;
        Ok(info)
    }
}

fn find(connection: &Connection, duid: &str) -> Result<Option<StationInfo>> {
    connection
        .query_row(
            "SELECT id, name, platform, quasar_config, network_info
             FROM stations WHERE id = ?1",
            [duid],
            row_to_info,
        )
        .optional()
        .map_err(Into::into)
}

fn row_to_info(row: &rusqlite::Row<'_>) -> rusqlite::Result<StationInfo> {
    let config: String = row.get(3)?;
    let network: String = row.get(4)?;
    Ok(StationInfo {
        duid: row.get(0)?,
        name: row.get(1)?,
        platform: row.get(2)?,
        quasar_config: serde_json::from_str(&config).unwrap_or(Value::Null),
        network_info: serde_json::from_str(&network).unwrap_or(Value::Null),
    })
}

fn default_config() -> Value {
    json!({
        "timezone": {
            "name": "Europe/Moscow",
            "offsetSeconds": 10800
        }
    })
}

fn merge_json(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_lists_stations() {
        let storage = SqliteStorage::in_memory().unwrap();
        assert!(storage.list().unwrap().is_empty());

        let created = storage.get_or_create("duid-1", "yandexmicro").unwrap();
        assert_eq!(created.duid, "duid-1");
        assert_eq!(created.platform, "yandexmicro");
        assert_eq!(created.quasar_config["timezone"]["name"], "Europe/Moscow");
        assert_eq!(storage.list().unwrap().len(), 1);

        // A second call must not duplicate the row.
        assert_eq!(
            storage.get_or_create("duid-1", "yandexmicro").unwrap(),
            created
        );
    }

    #[test]
    fn merges_config_patches_recursively() {
        let storage = SqliteStorage::in_memory().unwrap();
        storage.get_or_create("duid-1", "yandexmicro").unwrap();

        let updated = storage
            .update_name_and_config(
                "duid-1",
                Some("Кухня"),
                Some(&json!({"timezone": {"offsetSeconds": 7200}, "newKey": 1})),
            )
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "Кухня");
        assert_eq!(updated.quasar_config["timezone"]["offsetSeconds"], 7200);
        assert_eq!(updated.quasar_config["timezone"]["name"], "Europe/Moscow");
        assert_eq!(updated.quasar_config["newKey"], 1);
    }

    #[test]
    fn updates_network_info() {
        let storage = SqliteStorage::in_memory().unwrap();
        let info = storage
            .update_network_info("duid-1", "yandexmicro", &json!({"wifi": "home"}))
            .unwrap();
        assert_eq!(info.network_info["wifi"], "home");
    }

    #[test]
    fn missing_update_returns_none() {
        let storage = SqliteStorage::in_memory().unwrap();
        assert!(
            storage
                .update_name_and_config("missing", Some("x"), None)
                .unwrap()
                .is_none()
        );
    }
}
