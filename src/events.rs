// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeEventLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeEventKind {
    DepositSeen,
    ConfirmDepSubmitted,
    ConfirmDepOk,
    ConfirmDepFailed,
    WithdrawSeen,
    EvmRequestSent,
    EvmRequestFailed,
    EvmFinalized,
    EvmFinalizeFailed,
    DepositSubmitted,
    DepositOk,
    DepositFailed,
    ConfigUpdated,
    RouteReloaded,
}

#[derive(Debug, Clone, Serialize)]
pub struct BridgeEventRecord {
    pub id: u64,
    pub ts_ms: u64,
    pub route_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub kind: BridgeEventKind,
    pub level: BridgeEventLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventsPage {
    pub events: Vec<BridgeEventRecord>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub total_pages: u32,
}

pub fn events_db_path(config_path: &Path) -> std::path::PathBuf {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = config_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bridge");
    parent.join(format!("{stem}-events.db"))
}

pub struct EventStore {
    db: Arc<Mutex<Connection>>,
    next_id: AtomicU64,
    notify: broadcast::Sender<BridgeEventRecord>,
}

impl EventStore {
    pub fn open(db_path: &Path) -> anyhow::Result<Arc<Self>> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS bridge_events (
                id INTEGER PRIMARY KEY,
                ts_ms INTEGER NOT NULL,
                route_id TEXT NOT NULL,
                token TEXT,
                kind TEXT NOT NULL,
                level TEXT NOT NULL,
                message_id INTEGER,
                amount INTEGER,
                detail TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_bridge_events_ts
                ON bridge_events(ts_ms DESC, id DESC);
            CREATE INDEX IF NOT EXISTS idx_bridge_events_route
                ON bridge_events(route_id, ts_ms DESC, id DESC);
            ",
        )?;
        let max_id: u64 = conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM bridge_events",
            [],
            |row| row.get(0),
        )?;
        let (notify, _) = broadcast::channel(512);
        Ok(Arc::new(Self {
            db: Arc::new(Mutex::new(conn)),
            next_id: AtomicU64::new(max_id.saturating_add(1)),
            notify,
        }))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BridgeEventRecord> {
        self.notify.subscribe()
    }

    pub async fn emit(
        &self,
        route_id: impl Into<String>,
        token: Option<String>,
        kind: BridgeEventKind,
        level: BridgeEventLevel,
        message_id: Option<u64>,
        amount: Option<u64>,
        detail: impl Into<String>,
    ) {
        let event = BridgeEventRecord {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            route_id: route_id.into(),
            token,
            kind,
            level,
            message_id,
            amount,
            detail: detail.into(),
        };
        let db = self.db.clone();
        let stored = event.clone();
        match tokio::task::spawn_blocking(move || insert_event(&db, &stored)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => log::warn!("bridge event persist failed: {err}"),
            Err(err) => log::warn!("bridge event db task failed: {err}"),
        }
        let _ = self.notify.send(event);
    }

    pub async fn list_page(
        &self,
        route_id: Option<&str>,
        token: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> EventsPage {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 200);
        let db = self.db.clone();
        let route_id = route_id.map(str::to_string);
        let token = token.map(str::to_string);
        match tokio::task::spawn_blocking(move || {
            query_page(&db, route_id.as_deref(), token.as_deref(), page, page_size)
        })
        .await
        {
            Ok(Ok(page)) => page,
            Ok(Err(err)) => {
                log::warn!("bridge event query failed: {err}");
                empty_page(page, page_size)
            }
            Err(err) => {
                log::warn!("bridge event query task failed: {err}");
                empty_page(page, page_size)
            }
        }
    }
}

fn insert_event(db: &Arc<Mutex<Connection>>, event: &BridgeEventRecord) -> anyhow::Result<()> {
    let conn = db.lock().map_err(|_| anyhow::anyhow!("event db lock poisoned"))?;
    conn.execute(
        "INSERT INTO bridge_events (id, ts_ms, route_id, token, kind, level, message_id, amount, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event.id,
            event.ts_ms,
            event.route_id,
            event.token,
            event_kind_name(event.kind),
            event_level_name(event.level),
            event.message_id,
            event.amount,
            event.detail,
        ],
    )?;
    Ok(())
}

fn query_page(
    db: &Arc<Mutex<Connection>>,
    route_id: Option<&str>,
    token: Option<&str>,
    page: u32,
    page_size: u32,
) -> anyhow::Result<EventsPage> {
    let conn = db.lock().map_err(|_| anyhow::anyhow!("event db lock poisoned"))?;
    let total: u64 = conn.query_row(
        "SELECT COUNT(*) FROM bridge_events
         WHERE (?1 IS NULL OR route_id = ?1)
           AND (?2 IS NULL OR token = ?2)",
        params![route_id, token],
        |row| row.get(0),
    )?;
    let total_pages = if total == 0 {
        0
    } else {
        ((total + u64::from(page_size) - 1) / u64::from(page_size)) as u32
    };
    let offset = u64::from(page.saturating_sub(1)) * u64::from(page_size);
    let mut stmt = conn.prepare(
        "SELECT id, ts_ms, route_id, token, kind, level, message_id, amount, detail
         FROM bridge_events
         WHERE (?1 IS NULL OR route_id = ?1)
           AND (?2 IS NULL OR token = ?2)
         ORDER BY ts_ms DESC, id DESC
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = stmt.query_map(params![route_id, token, page_size, offset], map_event_row)?;
    let events = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(EventsPage {
        events,
        total,
        page,
        page_size,
        total_pages,
    })
}

fn map_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BridgeEventRecord> {
    let kind_raw: String = row.get(4)?;
    let level_raw: String = row.get(5)?;
    let kind = parse_kind(&kind_raw).map_err(|_| {
        rusqlite::Error::InvalidColumnType(4, kind_raw, rusqlite::types::Type::Text)
    })?;
    let level = parse_level(&level_raw).map_err(|_| {
        rusqlite::Error::InvalidColumnType(5, level_raw, rusqlite::types::Type::Text)
    })?;
    Ok(BridgeEventRecord {
        id: row.get(0)?,
        ts_ms: row.get(1)?,
        route_id: row.get(2)?,
        token: row.get(3)?,
        kind,
        level,
        message_id: row.get(6)?,
        amount: row.get(7)?,
        detail: row.get(8)?,
    })
}

fn event_kind_name(kind: BridgeEventKind) -> &'static str {
    match kind {
        BridgeEventKind::DepositSeen => "deposit_seen",
        BridgeEventKind::ConfirmDepSubmitted => "confirm_dep_submitted",
        BridgeEventKind::ConfirmDepOk => "confirm_dep_ok",
        BridgeEventKind::ConfirmDepFailed => "confirm_dep_failed",
        BridgeEventKind::WithdrawSeen => "withdraw_seen",
        BridgeEventKind::EvmRequestSent => "evm_request_sent",
        BridgeEventKind::EvmRequestFailed => "evm_request_failed",
        BridgeEventKind::EvmFinalized => "evm_finalized",
        BridgeEventKind::EvmFinalizeFailed => "evm_finalize_failed",
        BridgeEventKind::DepositSubmitted => "deposit_submitted",
        BridgeEventKind::DepositOk => "deposit_ok",
        BridgeEventKind::DepositFailed => "deposit_failed",
        BridgeEventKind::ConfigUpdated => "config_updated",
        BridgeEventKind::RouteReloaded => "route_reloaded",
    }
}

fn event_level_name(level: BridgeEventLevel) -> &'static str {
    match level {
        BridgeEventLevel::Info => "info",
        BridgeEventLevel::Warn => "warn",
        BridgeEventLevel::Error => "error",
    }
}

fn parse_kind(raw: &str) -> anyhow::Result<BridgeEventKind> {
    Ok(match raw {
        "deposit_seen" => BridgeEventKind::DepositSeen,
        "confirm_dep_submitted" => BridgeEventKind::ConfirmDepSubmitted,
        "confirm_dep_ok" => BridgeEventKind::ConfirmDepOk,
        "confirm_dep_failed" => BridgeEventKind::ConfirmDepFailed,
        "withdraw_seen" => BridgeEventKind::WithdrawSeen,
        "evm_request_sent" => BridgeEventKind::EvmRequestSent,
        "evm_request_failed" => BridgeEventKind::EvmRequestFailed,
        "evm_finalized" => BridgeEventKind::EvmFinalized,
        "evm_finalize_failed" => BridgeEventKind::EvmFinalizeFailed,
        "deposit_submitted" => BridgeEventKind::DepositSubmitted,
        "deposit_ok" => BridgeEventKind::DepositOk,
        "deposit_failed" => BridgeEventKind::DepositFailed,
        "config_updated" => BridgeEventKind::ConfigUpdated,
        "route_reloaded" => BridgeEventKind::RouteReloaded,
        other => anyhow::bail!("unknown bridge event kind: {other}"),
    })
}

fn parse_level(raw: &str) -> anyhow::Result<BridgeEventLevel> {
    Ok(match raw {
        "info" => BridgeEventLevel::Info,
        "warn" => BridgeEventLevel::Warn,
        "error" => BridgeEventLevel::Error,
        other => anyhow::bail!("unknown bridge event level: {other}"),
    })
}

fn empty_page(page: u32, page_size: u32) -> EventsPage {
    EventsPage {
        events: Vec::new(),
        total: 0,
        page,
        page_size,
        total_pages: 0,
    }
}
