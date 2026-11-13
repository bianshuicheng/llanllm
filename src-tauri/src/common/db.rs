//! SQLite 持久化（设置、配对节点、模型清单）。

use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeRow {
    pub id: String,
    pub hostname: String,
    pub last_ip: String,
    pub gpu: Option<String>,
    pub vram_total_mb: Option<u64>,
    pub vram_free_mb: Option<u64>,
    pub version: Option<String>,
    /// 长期会话密钥（hex）。主控持有每节点独立密钥。
    pub node_key: String,
    pub paired_at: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelRow {
    pub id: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    /// import = 本地导入（原地使用）; download = HF 下载
    pub source: String,
    pub quant: Option<String>,
    pub hf_repo: Option<String>,
    pub added_at: i64,
    /// ok / missing / size_mismatch（启动前校验得出）
    pub status: String,
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建数据目录失败: {e}"))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| format!("打开数据库失败: {e}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS settings(
                k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS nodes(
                id TEXT PRIMARY KEY,
                hostname TEXT NOT NULL,
                last_ip TEXT NOT NULL,
                gpu TEXT,
                vram_total_mb INTEGER,
                vram_free_mb INTEGER,
                version TEXT,
                node_key TEXT NOT NULL,
                paired_at INTEGER NOT NULL,
                last_seen INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS models(
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                source TEXT NOT NULL,
                quant TEXT,
                hf_repo TEXT,
                added_at INTEGER NOT NULL);",
        )
        .map_err(|e| format!("初始化数据库失败: {e}"))?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    // —— settings ——

    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT v FROM settings WHERE k = ?1",
            [key],
            |r| r.get::<_, String>(0),
        )
        .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings(k, v) VALUES(?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            [key, value],
        )
        .map_err(|e| format!("写入设置失败: {e}"))?;
        Ok(())
    }

    // —— nodes（主控：已配对节点）——

    pub fn upsert_node(&self, n: &NodeRow) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO nodes(id, hostname, last_ip, gpu, vram_total_mb, vram_free_mb, version, node_key, paired_at, last_seen)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(id) DO UPDATE SET
                hostname=excluded.hostname, last_ip=excluded.last_ip,
                gpu=excluded.gpu, vram_total_mb=excluded.vram_total_mb,
                vram_free_mb=excluded.vram_free_mb, version=excluded.version,
                node_key=excluded.node_key, last_seen=excluded.last_seen",
            rusqlite::params![
                n.id, n.hostname, n.last_ip, n.gpu, n.vram_total_mb,
                n.vram_free_mb, n.version, n.node_key, n.paired_at, n.last_seen
            ],
        )
        .map_err(|e| format!("写入节点失败: {e}"))?;
        Ok(())
    }

    pub fn list_nodes(&self) -> Vec<NodeRow> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id, hostname, last_ip, gpu, vram_total_mb, vram_free_mb, version, node_key, paired_at, last_seen FROM nodes",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map([], |r| {
                Ok(NodeRow {
                    id: r.get(0)?,
                    hostname: r.get(1)?,
                    last_ip: r.get(2)?,
                    gpu: r.get(3)?,
                    vram_total_mb: r.get(4)?,
                    vram_free_mb: r.get(5)?,
                    version: r.get(6)?,
                    node_key: r.get(7)?,
                    paired_at: r.get(8)?,
                    last_seen: r.get(9)?,
                })
            })
            .unwrap_or_else(|_| panic!("节点查询"));
        rows.filter_map(|r| r.ok()).collect()
    }

    pub fn delete_node(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM nodes WHERE id = ?1", [id])
            .map_err(|e| format!("删除节点失败: {e}"))?;
        Ok(())
    }

    // —— models ——

    pub fn add_model(&self, m: &ModelRow) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO models(id, name, path, size, mtime, source, quant, hf_repo, added_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                m.id, m.name, m.path, m.size as i64, m.mtime, m.source,
                m.quant, m.hf_repo, m.added_at
            ],
        )
        .map_err(|e| format!("写入模型失败: {e}"))?;
        Ok(())
    }

    pub fn list_models(&self) -> Vec<ModelRow> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id, name, path, size, mtime, source, quant, hf_repo, added_at FROM models ORDER BY added_at DESC",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map([], |r| {
                Ok(ModelRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    path: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                    mtime: r.get(4)?,
                    source: r.get(5)?,
                    quant: r.get(6)?,
                    hf_repo: r.get(7)?,
                    added_at: r.get(8)?,
                    status: "unknown".into(),
                })
            })
            .expect("模型查询");
        rows.filter_map(|r| r.ok()).collect()
    }

    pub fn update_model_path(&self, id: &str, path: &str, size: u64, mtime: i64) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE models SET path = ?2, size = ?3, mtime = ?4 WHERE id = ?1",
            rusqlite::params![id, path, size as i64, mtime],
        )
        .map_err(|e| format!("更新模型路径失败: {e}"))?;
        Ok(())
    }

    pub fn delete_model(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM models WHERE id = ?1", [id])
            .map_err(|e| format!("删除模型失败: {e}"))?;
        Ok(())
    }

    pub fn get_model(&self, id: &str) -> Option<ModelRow> {
        self.list_models().into_iter().find(|m| m.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(tag: &str) -> Db {
        let dir = std::env::temp_dir().join(format!("llanllm-test-{tag}-{}", uuid::Uuid::new_v4()));
        Db::open(&dir.join("t.db")).unwrap()
    }

    #[test]
    fn settings_roundtrip() {
        let db = tmp_db("settings");
        assert!(db.get_setting("x").is_none());
        db.set_setting("x", "1").unwrap();
        db.set_setting("x", "2").unwrap();
        assert_eq!(db.get_setting("x").unwrap(), "2");
    }

    #[test]
    fn nodes_roundtrip() {
        let db = tmp_db("nodes");
        let n = NodeRow {
            id: "n1".into(),
            hostname: "pc-1".into(),
            last_ip: "192.168.1.5".into(),
            gpu: Some("RTX 4070".into()),
            vram_total_mb: Some(12288),
            vram_free_mb: Some(12000),
            version: Some("0.1.0".into()),
            node_key: "abcd".into(),
            paired_at: 1,
            last_seen: 2,
        };
        db.upsert_node(&n).unwrap();
        let list = db.list_nodes();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].hostname, "pc-1");
        n_hostname_update(&db);
    }

    fn n_hostname_update(db: &Db) {
        let mut n2 = db.list_nodes()[0].clone();
        n2.hostname = "pc-1b".into();
        db.upsert_node(&n2).unwrap();
        assert_eq!(db.list_nodes()[0].hostname, "pc-1b");
        db.delete_node("n1").unwrap();
        assert!(db.list_nodes().is_empty());
    }

    #[test]
    fn models_roundtrip() {
        let db = tmp_db("models");
        let m = ModelRow {
            id: "m1".into(),
            name: "qwen.gguf".into(),
            path: r"E:\models\qwen.gguf".into(),
            size: 2048,
            mtime: 100,
            source: "import".into(),
            quant: None,
            hf_repo: None,
            added_at: 1,
            status: "unknown".into(),
        };
        db.add_model(&m).unwrap();
        assert_eq!(db.list_models().len(), 1);
        db.update_model_path("m1", r"F:\new\qwen.gguf", 2048, 200).unwrap();
        assert_eq!(db.get_model("m1").unwrap().path, r"F:\new\qwen.gguf");
        db.delete_model("m1").unwrap();
        assert!(db.list_models().is_empty());
    }
}
