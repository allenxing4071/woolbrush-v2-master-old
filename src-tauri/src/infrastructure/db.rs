use std::path::Path;

use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};

use crate::commands::trade_cmd::TradeRecord;

// ── Types ──

/// cities 表对应的 Rust 模型
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CityRow {
    pub slug: String,
    pub city_name: String,
    pub utc_offset: String,
    pub iana_tz: String,
    pub unit: String,
    pub station_name: Option<String>,
    pub station_code: Option<String>,
    pub station_url: Option<String>,
    pub avatar: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub updated_at: String,
}

/// 种子数据条目（从 seed_cities.json 反序列化）
#[derive(Debug, Deserialize)]
struct SeedCity {
    slug: String,
    city_name: String,
    utc_offset: String,
    iana_tz: String,
    unit: String,
    station_name: Option<String>,
    station_code: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
}

/// 用户设置（对应 user_settings 表）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct UserSettings {
    pub private_key: Option<String>,
    pub wallet_address: String,
    pub funder_address: Option<String>,
    pub proxy_url: Option<String>,
    pub feishu_webhook: Option<String>,
    pub feishu_chat_id: Option<String>,
    pub qianfan_api_key: Option<String>,
    pub qianfan_secret_key: Option<String>,
    /// LLM 服务商: "qianfan" | "bailian" | "ollama"（默认 "qianfan"）
    pub llm_provider: Option<String>,
    /// 阿里百炼 API Key（DashScope 兼容模式 Bearer 认证）
    pub bailian_api_key: Option<String>,
    /// 阿里百炼模型名称（默认 "qwen-plus"）
    pub bailian_model: Option<String>,
    /// Ollama Cloud API Key（Bearer 认证）
    pub ollama_api_key: Option<String>,
    /// Ollama API URL（默认 https://ollama.com/v1/chat/completions）
    pub ollama_url: Option<String>,
    /// Ollama 模型名称（默认 "qwen2.5:14b"）
    pub ollama_model: Option<String>,
    /// LLM 分析助记词（系统指令），为空时使用代码中的默认值
    pub llm_prompt: Option<String>,
}

// ── Database ──

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// 打开数据库连接并初始化表结构
    pub async fn open(path: &str) -> anyhow::Result<Self> {
        // 确保父目录存在
        if let Some(parent) = Path::new(path).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let absolute = std::path::absolute(path)?;
        let opts = SqliteConnectOptions::new()
            .filename(absolute)
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(opts).await?;
        Self::init_schema(&pool).await?;
        Self::seed_if_empty(&pool).await?;

        Ok(Self { pool })
    }

    async fn init_schema(pool: &SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS cities (
                slug          TEXT PRIMARY KEY,
                city_name     TEXT NOT NULL,
                utc_offset    TEXT NOT NULL,
                iana_tz       TEXT NOT NULL,
                unit          TEXT NOT NULL,
                station_name  TEXT,
                station_code  TEXT,
                station_url   TEXT,
                avatar        TEXT,
                lat           REAL,
                lon           REAL,
                updated_at    TEXT NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await?;

        // 迁移：已存在的表添加 avatar 列
        let has_avatar: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('cities') WHERE name = 'avatar'",
        )
        .fetch_one(pool)
        .await?;

        if has_avatar.0 == 0 {
            sqlx::query("ALTER TABLE cities ADD COLUMN avatar TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added avatar column to cities table");
        }

        // 迁移：已存在的表添加 station_url 列（数据采集站点网址）
        let has_station_url: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('cities') WHERE name = 'station_url'",
        )
        .fetch_one(pool)
        .await?;

        if has_station_url.0 == 0 {
            sqlx::query("ALTER TABLE cities ADD COLUMN station_url TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added station_url column to cities table");
        }

        // 迁移：已存在的表添加 lat / lon 列
        let has_lat: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM pragma_table_info('cities') WHERE name = 'lat'")
                .fetch_one(pool)
                .await?;
        if has_lat.0 == 0 {
            sqlx::query("ALTER TABLE cities ADD COLUMN lat REAL")
                .execute(pool)
                .await?;
            tracing::info!("Added lat column to cities table");
        }

        let has_lon: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM pragma_table_info('cities') WHERE name = 'lon'")
                .fetch_one(pool)
                .await?;
        if has_lon.0 == 0 {
            sqlx::query("ALTER TABLE cities ADD COLUMN lon REAL")
                .execute(pool)
                .await?;
            tracing::info!("Added lon column to cities table");
        }

        // 用户设置表（单条记录，id 固定为 1）
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS user_settings (
                id              INTEGER PRIMARY KEY DEFAULT 1,
                private_key     TEXT,
                wallet_address  TEXT NOT NULL DEFAULT '',
                funder_address  TEXT,
                proxy_url       TEXT,
                feishu_webhook  TEXT,
                feishu_chat_id  TEXT,
                qianfan_api_key    TEXT,
                qianfan_secret_key TEXT,
                llm_provider       TEXT DEFAULT 'qianfan',
                bailian_api_key    TEXT,
                bailian_model      TEXT,
                ollama_api_key     TEXT,
                ollama_url         TEXT,
                ollama_model       TEXT,
                llm_prompt         TEXT
            )
            "#,
        )
        .execute(pool)
        .await?;

        // 迁移：已存在的表添加 qianfan_api_key / qianfan_secret_key 列
        let has_qianfan_key: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'qianfan_api_key'",
        )
        .fetch_one(pool)
        .await?;
        if has_qianfan_key.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN qianfan_api_key TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added qianfan_api_key column to user_settings table");
        }

        let has_qianfan_secret: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'qianfan_secret_key'",
        )
        .fetch_one(pool)
        .await?;
        if has_qianfan_secret.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN qianfan_secret_key TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added qianfan_secret_key column to user_settings table");
        }

        // 迁移：已存在的表添加 llm_provider 列
        let has_llm_provider: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'llm_provider'",
        )
        .fetch_one(pool)
        .await?;
        if has_llm_provider.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN llm_provider TEXT DEFAULT 'qianfan'")
                .execute(pool)
                .await?;
            tracing::info!("Added llm_provider column to user_settings table");
        }

        // 迁移：已存在的表添加 bailian_api_key 列
        let has_bailian_key: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'bailian_api_key'",
        )
        .fetch_one(pool)
        .await?;
        if has_bailian_key.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN bailian_api_key TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added bailian_api_key column to user_settings table");
        }

        // 迁移：已存在的表添加 bailian_model 列
        let has_bailian_model: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'bailian_model'",
        )
        .fetch_one(pool)
        .await?;
        if has_bailian_model.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN bailian_model TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added bailian_model column to user_settings table");
        }

        // 迁移：已存在的表添加 ollama_api_key 列
        let has_ollama_api_key: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'ollama_api_key'",
        )
        .fetch_one(pool)
        .await?;
        if has_ollama_api_key.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN ollama_api_key TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added ollama_api_key column to user_settings table");
        }

        // 迁移：已存在的表添加 ollama_url 列
        let has_ollama_url: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'ollama_url'",
        )
        .fetch_one(pool)
        .await?;
        if has_ollama_url.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN ollama_url TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added ollama_url column to user_settings table");
        }

        // 迁移：已存在的表添加 ollama_model 列
        let has_ollama_model: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'ollama_model'",
        )
        .fetch_one(pool)
        .await?;
        if has_ollama_model.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN ollama_model TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added ollama_model column to user_settings table");
        }

        // 迁移：已存在的表添加 llm_prompt 列
        let has_llm_prompt: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('user_settings') WHERE name = 'llm_prompt'",
        )
        .fetch_one(pool)
        .await?;
        if has_llm_prompt.0 == 0 {
            sqlx::query("ALTER TABLE user_settings ADD COLUMN llm_prompt TEXT")
                .execute(pool)
                .await?;
            tracing::info!("Added llm_prompt column to user_settings table");
        }

        // 确保有默认行
        sqlx::query(
            r#"INSERT OR IGNORE INTO user_settings (id, wallet_address) VALUES (1, '')"#,
        )
        .execute(pool)
        .await?;

        // 交易记录表
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS trades (
                id TEXT PRIMARY KEY,
                market_id TEXT NOT NULL,
                question TEXT,
                city TEXT,
                side TEXT,
                token_id TEXT NOT NULL,
                threshold TEXT,
                entry_price REAL,
                size INTEGER,
                cost REAL,
                timestamp TEXT,
                exit_price REAL,
                exit_timestamp TEXT,
                realized_pnl REAL,
                status TEXT
            )
            "#,
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// 如果 cities 表为空，从 seed_cities.json 导入初始数据
    async fn seed_if_empty(pool: &SqlitePool) -> anyhow::Result<()> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM cities")
            .fetch_one(pool)
            .await?;

        if count.0 > 0 {
            tracing::info!("cities table already has {} rows, skipping seed", count.0);
            return Ok(());
        }

        let seed_json = include_str!("../../data/seed_cities.json");
        let seeds: Vec<SeedCity> = serde_json::from_str(seed_json)?;

        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = pool.begin().await?;

        for s in &seeds {
            sqlx::query(
                r#"INSERT INTO cities (slug, city_name, utc_offset, iana_tz, unit, station_name, station_code, station_url, avatar, lat, lon, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(&s.slug)
            .bind(&s.city_name)
            .bind(&s.utc_offset)
            .bind(&s.iana_tz)
            .bind(&s.unit)
            .bind(&s.station_name)
            .bind(&s.station_code)
            .bind(None::<&str>)
            .bind(None::<&str>)
            .bind(s.lat)
            .bind(s.lon)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!("Seeded {} cities from seed_cities.json", seeds.len());
        Ok(())
    }

    /// 获取所有城市
    pub async fn get_all_cities(&self) -> anyhow::Result<Vec<CityRow>> {
        let rows = sqlx::query_as::<_, CityRow>(
            r#"SELECT slug, city_name, utc_offset, iana_tz, unit, station_name, station_code, station_url, avatar, lat, lon, updated_at
               FROM cities ORDER BY slug"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 获取指定 slug 的城市
    pub async fn get_city(&self, slug: &str) -> anyhow::Result<Option<CityRow>> {
        let row = sqlx::query_as::<_, CityRow>(
            r#"SELECT slug, city_name, utc_offset, iana_tz, unit, station_name, station_code, station_url, avatar, lat, lon, updated_at
               FROM cities WHERE slug = ?"#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// 插入新城市
    pub async fn insert_city(&self, row: &CityRow) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO cities (slug, city_name, utc_offset, iana_tz, unit, station_name, station_code, station_url, avatar, lat, lon, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&row.slug)
        .bind(&row.city_name)
        .bind(&row.utc_offset)
        .bind(&row.iana_tz)
        .bind(&row.unit)
        .bind(&row.station_name)
        .bind(&row.station_code)
        .bind(&row.station_url)
        .bind(&row.avatar)
        .bind(row.lat)
        .bind(row.lon)
        .bind(&row.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 更新城市的气象站信息、采集站点网址、头像和经纬度
    pub async fn update_station(
        &self,
        slug: &str,
        station_name: &Option<String>,
        station_code: &Option<String>,
        station_url: &Option<String>,
        avatar: &Option<String>,
        lat: Option<f64>,
        lon: Option<f64>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE cities SET station_name = ?, station_code = ?, station_url = ?, avatar = ?, lat = ?, lon = ?, updated_at = ? WHERE slug = ?"#,
        )
        .bind(station_name)
        .bind(station_code)
        .bind(station_url)
        .bind(avatar)
        .bind(lat)
        .bind(lon)
        .bind(&now)
        .bind(slug)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 更新城市的气象站编号（station_code），供前端城市编辑页使用
    pub async fn update_station_code(
        &self,
        slug: &str,
        station_code: &Option<String>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE cities SET station_code = ?, updated_at = ? WHERE slug = ?"#,
        )
        .bind(station_code)
        .bind(&now)
        .bind(slug)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 删除城市（同步时清除活跃市场已不存在的城市）
    pub async fn delete_city(&self, slug: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM cities WHERE slug = ?")
            .bind(slug)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 更新城市的温度单位
    pub async fn update_unit(&self, slug: &str, unit: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE cities SET unit = ?, updated_at = ? WHERE slug = ?"#,
        )
        .bind(unit)
        .bind(&now)
        .bind(slug)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 批量更新城市坐标（从 AWC 返回中提取，只需执行一次）
    pub async fn update_city_coords(&self, coords: &[(String, f64, f64)]) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.pool.begin().await?;
        for (slug, lat, lon) in coords {
            sqlx::query(r#"UPDATE cities SET lat = ?, lon = ?, updated_at = ? WHERE slug = ? AND lat IS NULL"#)
                .bind(lat)
                .bind(lon)
                .bind(&now)
                .bind(slug)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ── user_settings ──

    /// 读取用户设置（解密 private_key）
    pub async fn get_settings(&self) -> anyhow::Result<UserSettings> {
        let row = sqlx::query_as::<_, UserSettings>(
            r#"SELECT private_key, wallet_address, funder_address, proxy_url, feishu_webhook, feishu_chat_id, qianfan_api_key, qianfan_secret_key, llm_provider, bailian_api_key, bailian_model, ollama_api_key, ollama_url, ollama_model, llm_prompt
               FROM user_settings WHERE id = 1"#,
        )
        .fetch_one(&self.pool)
        .await?;

        // Decrypt private_key if encrypted
        let decrypted_key = match row.private_key.as_deref() {
            Some(pk) if !pk.is_empty() => {
                match crate::infrastructure::crypto::decrypt(pk) {
                    Ok(plaintext) => Some(plaintext),
                    Err(e) => {
                        tracing::warn!("Failed to decrypt private key: {} (returning as-is)", e);
                        // Return as-is for backward compat (might be legacy plaintext)
                        Some(pk.to_string())
                    }
                }
            }
            _ => row.private_key,
        };

        Ok(UserSettings {
            private_key: decrypted_key,
            ..row
        })
    }

    /// 保存用户设置（加密 private_key）
    pub async fn save_settings(&self, settings: &UserSettings) -> anyhow::Result<()> {
        // Encrypt private_key before storing
        let encrypted_key = match settings.private_key.as_deref() {
            Some(pk) if !pk.is_empty() => {
                // If already encrypted, keep as-is; otherwise encrypt
                if crate::infrastructure::crypto::is_encrypted(pk) {
                    Some(pk.to_string())
                } else {
                    match crate::infrastructure::crypto::encrypt(pk) {
                        Ok(enc) => {
                            tracing::info!("Private key encrypted before storage");
                            Some(enc)
                        }
                        Err(e) => {
                            tracing::warn!("Failed to encrypt private key: {} (storing plaintext as fallback)", e);
                            Some(pk.to_string())
                        }
                    }
                }
            }
            _ => settings.private_key.clone(),
        };

        sqlx::query(
            r#"INSERT INTO user_settings (id, private_key, wallet_address, funder_address, proxy_url, feishu_webhook, feishu_chat_id, qianfan_api_key, qianfan_secret_key, llm_provider, bailian_api_key, bailian_model, ollama_api_key, ollama_url, ollama_model, llm_prompt)
               VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                     private_key        = excluded.private_key,
                     wallet_address     = excluded.wallet_address,
                     funder_address     = excluded.funder_address,
                     proxy_url          = excluded.proxy_url,
                     feishu_webhook     = excluded.feishu_webhook,
                     feishu_chat_id     = excluded.feishu_chat_id,
                     qianfan_api_key    = excluded.qianfan_api_key,
                     qianfan_secret_key = excluded.qianfan_secret_key,
                     llm_provider       = excluded.llm_provider,
                     bailian_api_key    = excluded.bailian_api_key,
                     bailian_model      = excluded.bailian_model,
                     ollama_api_key     = excluded.ollama_api_key,
                     ollama_url         = excluded.ollama_url,
                     ollama_model       = excluded.ollama_model,
                     llm_prompt         = excluded.llm_prompt"#,
        )
        .bind(&encrypted_key)
        .bind(&settings.wallet_address)
        .bind(&settings.funder_address)
        .bind(&settings.proxy_url)
        .bind(&settings.feishu_webhook)
        .bind(&settings.feishu_chat_id)
        .bind(&settings.qianfan_api_key)
        .bind(&settings.qianfan_secret_key)
        .bind(&settings.llm_provider)
        .bind(&settings.bailian_api_key)
        .bind(&settings.bailian_model)
        .bind(&settings.ollama_api_key)
        .bind(&settings.ollama_url)
        .bind(&settings.ollama_model)
        .bind(&settings.llm_prompt)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ── trades ──

    /// 保存交易记录
    pub async fn save_trade(&self, trade: &TradeRecord) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO trades (
                id, market_id, question, city, side, token_id, threshold,
                entry_price, size, cost, timestamp,
                exit_price, exit_timestamp, realized_pnl, status
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&trade.id)
        .bind(&trade.market_id)
        .bind(&trade.question)
        .bind(&trade.city)
        .bind(&trade.side)
        .bind(&trade.token_id)
        .bind(&trade.threshold)
        .bind(trade.entry_price)
        .bind(trade.size as i64)
        .bind(trade.cost)
        .bind(&trade.timestamp)
        .bind(trade.exit_price)
        .bind(trade.exit_timestamp.as_ref())
        .bind(trade.realized_pnl)
        .bind(&trade.status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 按时间范围查询交易记录
    ///
    /// `time_field` = "open"  → 按开仓时间 (timestamp) 过滤排序
    /// `time_field` = "close" → 按平仓时间 (exit_timestamp) 过滤排序（仅含已平仓记录）
    pub async fn get_trades_by_date_range(
        &self,
        start_date: &str,
        end_date: &str,
        time_field: &str,
    ) -> anyhow::Result<Vec<TradeRecord>> {
        let col = match time_field {
            "close" => "exit_timestamp",
            _ => "timestamp",
        };
        let sql = format!(
            r#"
            SELECT id, market_id, question, city, side, token_id, threshold,
                   entry_price, size, cost, timestamp,
                   exit_price, exit_timestamp, realized_pnl, status
            FROM trades
            WHERE datetime({col}) >= datetime(?, 'utc')
              AND datetime({col}) <  datetime(?, 'utc')
            ORDER BY {col} DESC
            "#,
        );
        let rows = sqlx::query_as::<_, TradeRow>(&sql)
            .bind(start_date)
            .bind(end_date)
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(TryFrom::try_from).collect()
    }

    /// 按平仓时间聚合每日盈亏
    pub async fn get_daily_pnl(&self, days: u32) -> anyhow::Result<Vec<DailyPnlRow>> {
        let rows = sqlx::query_as::<_, DailyPnlRow>(
            r#"
            SELECT
                strftime('%m-%d', exit_timestamp, 'localtime') AS date,
                COALESCE(SUM(realized_pnl), 0) AS pnl
            FROM trades
            WHERE status IN ('settled', 'closed')
              AND realized_pnl IS NOT NULL
              AND exit_timestamp IS NOT NULL
              AND exit_timestamp >= datetime('now', ?)
            GROUP BY strftime('%Y-%m-%d', exit_timestamp, 'localtime')
            ORDER BY exit_timestamp ASC
            "#,
        )
        .bind(format!("-{} days", days))
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    /// Get all open trades (status = 'open'), ordered by timestamp desc
    pub async fn get_all_open_trades(&self) -> anyhow::Result<Vec<TradeRecord>> {
        let rows = sqlx::query_as::<_, TradeRow>(
            r#"
            SELECT id, market_id, question, city, side, token_id, threshold,
                   entry_price, size, cost, timestamp,
                   exit_price, exit_timestamp, realized_pnl, status
            FROM trades
            WHERE status = 'open'
            ORDER BY timestamp DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryFrom::try_from).collect()
    }

    /// Get the most recent open trade for a given token_id
    pub async fn get_open_trade_by_token(&self, token_id: &str) -> anyhow::Result<Option<TradeRecord>> {
        let row = sqlx::query_as::<_, TradeRow>(
            r#"
            SELECT id, market_id, question, city, side, token_id, threshold,
                   entry_price, size, cost, timestamp,
                   exit_price, exit_timestamp, realized_pnl, status
            FROM trades
            WHERE token_id = ? AND status = 'open'
            ORDER BY timestamp DESC
            LIMIT 1
            "#,
        )
        .bind(token_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|r| r.try_into()).transpose()
    }

    /// 清空 trades 表所有记录
    pub async fn clear_trades(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM trades")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 更新交易记录的平仓信息
    pub async fn update_exit(
        &self,
        trade_id: &str,
        exit_price: f64,
        exit_timestamp: &str,
        realized_pnl: f64,
        status: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE trades SET
                exit_price = ?,
                exit_timestamp = ?,
                realized_pnl = ?,
                status = ?
            WHERE id = ?
            "#,
        )
        .bind(exit_price)
        .bind(exit_timestamp)
        .bind(realized_pnl)
        .bind(status)
        .bind(trade_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

// ── DB row types ──

#[derive(sqlx::FromRow)]
struct TradeRow {
    id: String,
    market_id: String,
    question: Option<String>,
    city: Option<String>,
    side: Option<String>,
    token_id: String,
    threshold: Option<String>,
    entry_price: Option<f64>,
    size: Option<i64>,
    cost: Option<f64>,
    timestamp: Option<String>,
    exit_price: Option<f64>,
    exit_timestamp: Option<String>,
    realized_pnl: Option<f64>,
    status: Option<String>,
}

impl TryFrom<TradeRow> for TradeRecord {
    type Error = anyhow::Error;

    fn try_from(row: TradeRow) -> Result<Self, Self::Error> {
        Ok(TradeRecord {
            id: row.id,
            market_id: row.market_id,
            question: row.question.unwrap_or_default(),
            city: row.city.unwrap_or_default(),
            side: row.side.unwrap_or_else(|| "NO".to_string()),
            token_id: row.token_id,
            threshold: row.threshold.unwrap_or_default(),
            entry_price: row.entry_price.unwrap_or(0.0),
            size: row.size.unwrap_or(0) as f64,
            cost: row.cost.unwrap_or(0.0),
            timestamp: row.timestamp.unwrap_or_default(),
            exit_price: row.exit_price,
            exit_timestamp: row.exit_timestamp,
            realized_pnl: row.realized_pnl,
            status: row.status.unwrap_or_else(|| "open".to_string()),
        })
    }
}

/// 每日盈亏聚合结果
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DailyPnlRow {
    pub date: String,
    pub pnl: f64,
}
