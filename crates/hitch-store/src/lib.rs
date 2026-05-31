//! `hitch-store` — SQLite persistence for Hitch.
//!
//! This crate persists the daemon-owned view of Hitch: projects, worktree
//! metadata, session layout, and scrollback snapshots. It is intentionally a
//! feature crate in the ADR-0005 sense: it depends on `hitch-core`, but never on
//! any other Hitch feature crate.

use std::fmt;
use std::path::{Path, PathBuf};

use hitch_core::{
    Project, ProjectId, ProjectKind, Session, SessionId, SessionParent, Worktree, WorktreeId,
};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

const SCHEMA_VERSION: i32 = 2;
const PROJECT_KIND_GIT_BACKED: &str = "git-backed";
const PROJECT_KIND_PLAIN: &str = "plain";
const SESSION_PARENT_WORKTREE: &str = "worktree";
const SESSION_PARENT_PROJECT: &str = "project";

/// Convenient result alias for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// SQLite-backed persistence owned by the daemon.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

/// A complete persisted layout, enough for the daemon to reconstruct its
/// project/worktree/session tree on restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLayout {
    pub projects: Vec<Project>,
    pub worktrees: Vec<Worktree>,
    pub sessions: Vec<Session>,
}

impl Store {
    /// Open (or create) a database at `path`, applying all known migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// Open an in-memory database, useful for tests and daemon smoke checks.
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Apply database migrations. Safe to call repeatedly.
    pub fn migrate(&self) -> Result<()> {
        let version = self.schema_version()?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }

        if version < 1 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE projects (
                    id TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    root TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK (kind IN ('git-backed', 'plain'))
                );

                CREATE TABLE worktrees (
                    id TEXT PRIMARY KEY NOT NULL,
                    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                    path TEXT NOT NULL,
                    branch TEXT NOT NULL,
                    is_main INTEGER NOT NULL CHECK (is_main IN (0, 1)),
                    is_hitch_managed INTEGER NOT NULL CHECK (is_hitch_managed IN (0, 1))
                );

                CREATE INDEX worktrees_project_id_idx ON worktrees(project_id);

                CREATE TABLE sessions (
                    id TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    parent_kind TEXT NOT NULL CHECK (parent_kind IN ('worktree', 'project')),
                    parent_id TEXT NOT NULL,
                    cwd TEXT NOT NULL,
                    scrollback BLOB NOT NULL DEFAULT X''
                );

                CREATE INDEX sessions_parent_idx ON sessions(parent_kind, parent_id);

                PRAGMA user_version = 2;
                "#,
            )?;
        }

        if version == 1 {
            self.conn.execute_batch(
                r#"
                ALTER TABLE worktrees
                    ADD COLUMN is_hitch_managed INTEGER NOT NULL DEFAULT 0 CHECK (is_hitch_managed IN (0, 1));

                PRAGMA user_version = 2;
                "#,
            )?;
        }
        Ok(())
    }

    /// Return the current schema version (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<i32> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    /// Insert a project.
    pub fn insert_project(&self, project: &Project) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (id, name, root, kind) VALUES (?1, ?2, ?3, ?4)",
            params![
                id_to_string(project.id.as_uuid()),
                project.name,
                path_to_string(&project.root),
                project_kind_to_db(project.kind),
            ],
        )?;
        Ok(())
    }

    /// Replace all mutable fields of an existing project.
    pub fn update_project(&self, project: &Project) -> Result<()> {
        let rows = self.conn.execute(
            "UPDATE projects SET name = ?2, root = ?3, kind = ?4 WHERE id = ?1",
            params![
                id_to_string(project.id.as_uuid()),
                project.name,
                path_to_string(&project.root),
                project_kind_to_db(project.kind),
            ],
        )?;
        ensure_changed(rows, EntityKind::Project)
    }

    /// Delete a project and its worktree/session layout.
    pub fn delete_project(&self, project_id: ProjectId) -> Result<()> {
        let project_id = id_to_string(project_id.as_uuid());
        let worktree_ids = self
            .conn
            .prepare("SELECT id FROM worktrees WHERE project_id = ?1")?
            .query_map(params![project_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        self.conn.execute(
            "DELETE FROM sessions WHERE parent_kind = ?1 AND parent_id = ?2",
            params![SESSION_PARENT_PROJECT, project_id],
        )?;
        for worktree_id in worktree_ids {
            self.conn.execute(
                "DELETE FROM sessions WHERE parent_kind = ?1 AND parent_id = ?2",
                params![SESSION_PARENT_WORKTREE, worktree_id],
            )?;
        }

        let rows = self
            .conn
            .execute("DELETE FROM projects WHERE id = ?1", params![project_id])?;
        ensure_changed(rows, EntityKind::Project)
    }

    /// Load a single project.
    pub fn get_project(&self, project_id: ProjectId) -> Result<Option<Project>> {
        self.conn
            .query_row(
                "SELECT id, name, root, kind FROM projects WHERE id = ?1",
                params![id_to_string(project_id.as_uuid())],
                map_project,
            )
            .optional()
            .map_err(Into::into)
    }

    /// List all projects in stable display order.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, root, kind FROM projects ORDER BY name, id")?;
        let projects = stmt
            .query_map([], map_project)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(projects)
    }

    /// Insert a worktree. The referenced project must already exist.
    pub fn insert_worktree(&self, worktree: &Worktree) -> Result<()> {
        self.conn.execute(
            "INSERT INTO worktrees (id, project_id, path, branch, is_main, is_hitch_managed) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id_to_string(worktree.id.as_uuid()),
                id_to_string(worktree.project_id.as_uuid()),
                path_to_string(&worktree.path),
                worktree.branch,
                bool_to_i64(worktree.is_main),
                bool_to_i64(worktree.is_hitch_managed),
            ],
        )?;
        Ok(())
    }

    /// Replace all mutable fields of an existing worktree.
    pub fn update_worktree(&self, worktree: &Worktree) -> Result<()> {
        let rows = self.conn.execute(
            "UPDATE worktrees SET project_id = ?2, path = ?3, branch = ?4, is_main = ?5, is_hitch_managed = ?6 WHERE id = ?1",
            params![
                id_to_string(worktree.id.as_uuid()),
                id_to_string(worktree.project_id.as_uuid()),
                path_to_string(&worktree.path),
                worktree.branch,
                bool_to_i64(worktree.is_main),
                bool_to_i64(worktree.is_hitch_managed),
            ],
        )?;
        ensure_changed(rows, EntityKind::Worktree)
    }

    /// Delete a worktree and its session layout.
    pub fn delete_worktree(&self, worktree_id: WorktreeId) -> Result<()> {
        let worktree_id = id_to_string(worktree_id.as_uuid());
        self.conn.execute(
            "DELETE FROM sessions WHERE parent_kind = ?1 AND parent_id = ?2",
            params![SESSION_PARENT_WORKTREE, worktree_id],
        )?;
        let rows = self
            .conn
            .execute("DELETE FROM worktrees WHERE id = ?1", params![worktree_id])?;
        ensure_changed(rows, EntityKind::Worktree)
    }

    /// Load a single worktree.
    pub fn get_worktree(&self, worktree_id: WorktreeId) -> Result<Option<Worktree>> {
        self.conn
            .query_row(
                "SELECT id, project_id, path, branch, is_main, is_hitch_managed FROM worktrees WHERE id = ?1",
                params![id_to_string(worktree_id.as_uuid())],
                map_worktree,
            )
            .optional()
            .map_err(Into::into)
    }

    /// List worktrees for one project in stable display order.
    pub fn list_worktrees(&self, project_id: ProjectId) -> Result<Vec<Worktree>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, path, branch, is_main, is_hitch_managed FROM worktrees WHERE project_id = ?1 ORDER BY is_main DESC, branch, id",
        )?;
        let worktrees = stmt
            .query_map(params![id_to_string(project_id.as_uuid())], map_worktree)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(worktrees)
    }

    /// Insert a session layout row. The polymorphic parent must already exist.
    pub fn insert_session(&self, session: &Session) -> Result<()> {
        self.ensure_session_parent_exists(session.parent)?;
        let (parent_kind, parent_id) = session_parent_to_db(session.parent);
        self.conn.execute(
            "INSERT INTO sessions (id, name, parent_kind, parent_id, cwd) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id_to_string(session.id.as_uuid()),
                session.name,
                parent_kind,
                parent_id,
                path_to_string(&session.cwd),
            ],
        )?;
        Ok(())
    }

    /// Replace all mutable fields of an existing session. Existing scrollback is preserved.
    pub fn update_session(&self, session: &Session) -> Result<()> {
        self.ensure_session_parent_exists(session.parent)?;
        let (parent_kind, parent_id) = session_parent_to_db(session.parent);
        let rows = self.conn.execute(
            "UPDATE sessions SET name = ?2, parent_kind = ?3, parent_id = ?4, cwd = ?5 WHERE id = ?1",
            params![
                id_to_string(session.id.as_uuid()),
                session.name,
                parent_kind,
                parent_id,
                path_to_string(&session.cwd),
            ],
        )?;
        ensure_changed(rows, EntityKind::Session)
    }

    /// Delete a session layout row and its scrollback snapshot.
    pub fn delete_session(&self, session_id: SessionId) -> Result<()> {
        let rows = self.conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![id_to_string(session_id.as_uuid())],
        )?;
        ensure_changed(rows, EntityKind::Session)
    }

    /// Load a single session.
    pub fn get_session(&self, session_id: SessionId) -> Result<Option<Session>> {
        self.conn
            .query_row(
                "SELECT id, name, parent_kind, parent_id, cwd FROM sessions WHERE id = ?1",
                params![id_to_string(session_id.as_uuid())],
                map_session,
            )
            .optional()
            .map_err(Into::into)
    }

    /// List sessions, optionally narrowed to one parent.
    pub fn list_sessions(&self, parent: Option<SessionParent>) -> Result<Vec<Session>> {
        match parent {
            Some(parent) => {
                let (parent_kind, parent_id) = session_parent_to_db(parent);
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, parent_kind, parent_id, cwd FROM sessions WHERE parent_kind = ?1 AND parent_id = ?2 ORDER BY name, id",
                )?;
                let sessions = stmt
                    .query_map(params![parent_kind, parent_id], map_session)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(sessions)
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, parent_kind, parent_id, cwd FROM sessions ORDER BY parent_kind, parent_id, name, id",
                )?;
                let sessions = stmt
                    .query_map([], map_session)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(sessions)
            }
        }
    }

    /// Persist a point-in-time scrollback snapshot for a session.
    pub fn save_scrollback(&self, session_id: SessionId, bytes: &[u8]) -> Result<()> {
        let rows = self.conn.execute(
            "UPDATE sessions SET scrollback = ?2 WHERE id = ?1",
            params![id_to_string(session_id.as_uuid()), bytes],
        )?;
        ensure_changed(rows, EntityKind::Session)
    }

    /// Load the persisted scrollback snapshot for a session.
    pub fn load_scrollback(&self, session_id: SessionId) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row(
                "SELECT scrollback FROM sessions WHERE id = ?1",
                params![id_to_string(session_id.as_uuid())],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Load the complete daemon layout graph.
    pub fn load_layout(&self) -> Result<StoredLayout> {
        Ok(StoredLayout {
            projects: self.list_projects()?,
            worktrees: self.list_all_worktrees()?,
            sessions: self.list_sessions(None)?,
        })
    }

    fn list_all_worktrees(&self) -> Result<Vec<Worktree>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, path, branch, is_main, is_hitch_managed FROM worktrees ORDER BY project_id, is_main DESC, branch, id",
        )?;
        let worktrees = stmt
            .query_map([], map_worktree)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(worktrees)
    }

    fn ensure_session_parent_exists(&self, parent: SessionParent) -> Result<()> {
        let exists: bool = match parent {
            SessionParent::Worktree(id) => self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM worktrees WHERE id = ?1)",
                params![id_to_string(id.as_uuid())],
                |row| row.get(0),
            )?,
            SessionParent::Project(id) => self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
                params![id_to_string(id.as_uuid())],
                |row| row.get(0),
            )?,
        };

        if exists {
            Ok(())
        } else {
            Err(StoreError::InvalidSessionParent(parent))
        }
    }
}

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    UnsupportedSchemaVersion { found: i32, supported: i32 },
    NotFound(EntityKind),
    InvalidProjectKind(String),
    InvalidSessionParentKind(String),
    InvalidSessionParent(SessionParent),
    InvalidUuid { value: String, source: uuid::Error },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Project,
    Worktree,
    Session,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(err) => fmt::Display::fmt(err, f),
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                f,
                "database schema version {found} is newer than supported version {supported}"
            ),
            Self::NotFound(kind) => write!(f, "{} not found", kind.as_str()),
            Self::InvalidProjectKind(kind) => write!(f, "invalid project kind in store: {kind}"),
            Self::InvalidSessionParentKind(kind) => {
                write!(f, "invalid session parent kind in store: {kind}")
            }
            Self::InvalidSessionParent(parent) => {
                write!(f, "session parent does not exist: {parent:?}")
            }
            Self::InvalidUuid { value, source } => write!(f, "invalid UUID {value:?}: {source}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(err) => Some(err),
            Self::InvalidUuid { source, .. } => Some(source),
            Self::UnsupportedSchemaVersion { .. }
            | Self::NotFound(_)
            | Self::InvalidProjectKind(_)
            | Self::InvalidSessionParentKind(_)
            | Self::InvalidSessionParent(_) => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl EntityKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Worktree => "worktree",
            Self::Session => "session",
        }
    }
}

fn ensure_changed(rows: usize, kind: EntityKind) -> Result<()> {
    if rows == 0 {
        Err(StoreError::NotFound(kind))
    } else {
        Ok(())
    }
}

fn map_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let id: String = row.get(0)?;
    let name = row.get(1)?;
    let root: String = row.get(2)?;
    let kind: String = row.get(3)?;
    Ok(Project {
        id: parse_uuid(&id).map(ProjectId::from).map_err(to_sql_error)?,
        name,
        root: PathBuf::from(root),
        kind: project_kind_from_db(&kind).map_err(to_sql_error)?,
    })
}

fn map_worktree(row: &rusqlite::Row<'_>) -> rusqlite::Result<Worktree> {
    let id: String = row.get(0)?;
    let project_id: String = row.get(1)?;
    let path: String = row.get(2)?;
    let branch = row.get(3)?;
    let is_main: i64 = row.get(4)?;
    let is_hitch_managed: i64 = row.get(5)?;
    Ok(Worktree {
        id: parse_uuid(&id)
            .map(WorktreeId::from)
            .map_err(to_sql_error)?,
        project_id: parse_uuid(&project_id)
            .map(ProjectId::from)
            .map_err(to_sql_error)?,
        path: PathBuf::from(path),
        branch,
        is_main: is_main != 0,
        is_hitch_managed: is_hitch_managed != 0,
    })
}

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let id: String = row.get(0)?;
    let name = row.get(1)?;
    let parent_kind: String = row.get(2)?;
    let parent_id: String = row.get(3)?;
    let cwd: String = row.get(4)?;
    Ok(Session {
        id: parse_uuid(&id).map(SessionId::from).map_err(to_sql_error)?,
        name,
        parent: session_parent_from_db(&parent_kind, &parent_id).map_err(to_sql_error)?,
        cwd: PathBuf::from(cwd),
    })
}

fn project_kind_to_db(kind: ProjectKind) -> &'static str {
    match kind {
        ProjectKind::GitBacked => PROJECT_KIND_GIT_BACKED,
        ProjectKind::Plain => PROJECT_KIND_PLAIN,
    }
}

fn project_kind_from_db(kind: &str) -> Result<ProjectKind> {
    match kind {
        PROJECT_KIND_GIT_BACKED => Ok(ProjectKind::GitBacked),
        PROJECT_KIND_PLAIN => Ok(ProjectKind::Plain),
        other => Err(StoreError::InvalidProjectKind(other.to_string())),
    }
}

fn session_parent_to_db(parent: SessionParent) -> (&'static str, String) {
    match parent {
        SessionParent::Worktree(id) => (SESSION_PARENT_WORKTREE, id_to_string(id.as_uuid())),
        SessionParent::Project(id) => (SESSION_PARENT_PROJECT, id_to_string(id.as_uuid())),
    }
}

fn session_parent_from_db(kind: &str, id: &str) -> Result<SessionParent> {
    let id = parse_uuid(id)?;
    match kind {
        SESSION_PARENT_WORKTREE => Ok(SessionParent::Worktree(WorktreeId::from(id))),
        SESSION_PARENT_PROJECT => Ok(SessionParent::Project(ProjectId::from(id))),
        other => Err(StoreError::InvalidSessionParentKind(other.to_string())),
    }
}

fn parse_uuid(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|source| StoreError::InvalidUuid {
        value: value.to_string(),
        source,
    })
}

fn id_to_string(id: Uuid) -> String {
    id.to_string()
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bool_to_i64(value: bool) -> i64 {
    i64::from(value)
}

fn to_sql_error(err: StoreError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn migrates_new_database_and_is_idempotent() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        store
            .conn
            .execute(
                "INSERT INTO projects (id, name, root, kind) VALUES (?1, ?2, ?3, ?4)",
                params![
                    ProjectId::new().to_string(),
                    "hitch",
                    "/tmp/hitch",
                    PROJECT_KIND_GIT_BACKED
                ],
            )
            .unwrap();
        assert_eq!(store.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn migrates_v1_worktrees_as_not_hitch_managed() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE projects (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                root TEXT NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('git-backed', 'plain'))
            );

            CREATE TABLE worktrees (
                id TEXT PRIMARY KEY NOT NULL,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                branch TEXT NOT NULL,
                is_main INTEGER NOT NULL CHECK (is_main IN (0, 1))
            );

            CREATE INDEX worktrees_project_id_idx ON worktrees(project_id);

            CREATE TABLE sessions (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                parent_kind TEXT NOT NULL CHECK (parent_kind IN ('worktree', 'project')),
                parent_id TEXT NOT NULL,
                cwd TEXT NOT NULL,
                scrollback BLOB NOT NULL DEFAULT X''
            );

            CREATE INDEX sessions_parent_idx ON sessions(parent_kind, parent_id);
            PRAGMA user_version = 1;
            "#,
        )
        .unwrap();

        let project_id = ProjectId::new();
        let worktree_id = WorktreeId::new();
        conn.execute(
            "INSERT INTO projects (id, name, root, kind) VALUES (?1, ?2, ?3, ?4)",
            params![
                id_to_string(project_id.as_uuid()),
                "hitch",
                "/tmp/hitch",
                PROJECT_KIND_GIT_BACKED
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, project_id, path, branch, is_main) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id_to_string(worktree_id.as_uuid()),
                id_to_string(project_id.as_uuid()),
                "/tmp/hitch-linked",
                "feature",
                0
            ],
        )
        .unwrap();

        let store = Store::from_connection(conn).unwrap();
        let worktree = store.get_worktree(worktree_id).unwrap().unwrap();
        assert!(!worktree.is_main);
        assert!(!worktree.is_hitch_managed);
    }

    #[test]
    fn rejects_database_from_newer_schema_version() {
        let path = temp_db_path("future-schema");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }

        let error = Store::open(&path).unwrap_err();
        assert!(matches!(
            error,
            StoreError::UnsupportedSchemaVersion {
                found: 99,
                supported: SCHEMA_VERSION
            }
        ));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn crud_projects_worktrees_sessions_and_scrollback() {
        let store = Store::in_memory().unwrap();
        let mut project = Project::new("hitch", "/Users/me/Code/hitch", ProjectKind::GitBacked);
        store.insert_project(&project).unwrap();
        assert_eq!(
            store.get_project(project.id).unwrap(),
            Some(project.clone())
        );

        project.name = "Hitch".into();
        store.update_project(&project).unwrap();
        assert_eq!(store.list_projects().unwrap(), vec![project.clone()]);

        let mut worktree = Worktree::new(
            project.id,
            "/Users/me/.hitch/worktrees/hitch/main",
            "main",
            true,
            false,
        );
        store.insert_worktree(&worktree).unwrap();
        assert_eq!(
            store.get_worktree(worktree.id).unwrap(),
            Some(worktree.clone())
        );

        worktree.branch = "trunk".into();
        store.update_worktree(&worktree).unwrap();
        assert_eq!(
            store.list_worktrees(project.id).unwrap(),
            vec![worktree.clone()]
        );

        let mut session = Session::new(
            "shell",
            SessionParent::Worktree(worktree.id),
            &worktree.path,
        );
        store.insert_session(&session).unwrap();
        assert_eq!(
            store.get_session(session.id).unwrap(),
            Some(session.clone())
        );

        session.name = "agent".into();
        store.update_session(&session).unwrap();
        assert_eq!(
            store
                .list_sessions(Some(SessionParent::Worktree(worktree.id)))
                .unwrap(),
            vec![session.clone()]
        );

        store
            .save_scrollback(session.id, b"hello scrollback")
            .unwrap();
        assert_eq!(
            store.load_scrollback(session.id).unwrap(),
            Some(b"hello scrollback".to_vec())
        );

        store.delete_session(session.id).unwrap();
        assert_eq!(store.get_session(session.id).unwrap(), None);
        store.delete_worktree(worktree.id).unwrap();
        assert_eq!(store.get_worktree(worktree.id).unwrap(), None);
        store.delete_project(project.id).unwrap();
        assert_eq!(store.get_project(project.id).unwrap(), None);
    }

    #[test]
    fn rejects_session_with_missing_parent() {
        let store = Store::in_memory().unwrap();
        let session = Session::new(
            "orphan",
            SessionParent::Project(ProjectId::new()),
            "/tmp/orphan",
        );
        assert!(matches!(
            store.insert_session(&session),
            Err(StoreError::InvalidSessionParent(SessionParent::Project(_)))
        ));
    }

    #[test]
    fn deleting_project_removes_worktrees_and_sessions() {
        let store = Store::in_memory().unwrap();
        let project = Project::new("hitch", "/tmp/hitch", ProjectKind::GitBacked);
        store.insert_project(&project).unwrap();
        let worktree = Worktree::new(project.id, "/tmp/hitch-main", "main", true, false);
        store.insert_worktree(&worktree).unwrap();
        let project_session = Session::new(
            "project-shell",
            SessionParent::Project(project.id),
            &project.root,
        );
        let worktree_session = Session::new(
            "worktree-shell",
            SessionParent::Worktree(worktree.id),
            &worktree.path,
        );
        store.insert_session(&project_session).unwrap();
        store.insert_session(&worktree_session).unwrap();

        store.delete_project(project.id).unwrap();

        assert!(store.load_layout().unwrap().projects.is_empty());
        assert!(store.load_layout().unwrap().worktrees.is_empty());
        assert!(store.load_layout().unwrap().sessions.is_empty());
    }

    #[test]
    fn reload_reconstructs_layout_and_scrollback_snapshots() {
        let path = temp_db_path("reload-layout");
        let project = Project::new("hitch", "/Users/me/Code/hitch", ProjectKind::GitBacked);
        let worktree = Worktree::new(
            project.id,
            "/Users/me/.hitch/worktrees/hitch/feature",
            "feature",
            false,
            true,
        );
        let session = Session::new(
            "agent",
            SessionParent::Worktree(worktree.id),
            &worktree.path,
        );

        {
            let store = Store::open(&path).unwrap();
            store.insert_project(&project).unwrap();
            store.insert_worktree(&worktree).unwrap();
            store.insert_session(&session).unwrap();
            store
                .save_scrollback(session.id, b"previous output")
                .unwrap();
        }

        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.load_layout().unwrap(),
            StoredLayout {
                projects: vec![project],
                worktrees: vec![worktree],
                sessions: vec![session.clone()],
            }
        );
        assert_eq!(
            reopened.load_scrollback(session.id).unwrap(),
            Some(b"previous output".to_vec())
        );

        let _ = fs::remove_file(path);
    }

    fn temp_db_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("hitch-store-{name}-{nonce}.sqlite"))
    }
}
