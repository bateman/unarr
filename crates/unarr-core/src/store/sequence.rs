//! Ids that keep counting, as the originals' AUTOINCREMENT tables do: a
//! deleted row's id is never handed out again.

use rusqlite::{params, Connection, OptionalExtension};

/// The next id for `table`. Without a stored counter the table's current
/// maximum starts it, which is what the importer finds in a database whose
/// highest row was never deleted.
pub fn next_id(conn: &Connection, table: &str) -> anyhow::Result<i64> {
    let stored: Option<i64> = conn
        .query_row(
            "SELECT value FROM sequences WHERE name = ?1",
            params![table],
            |r| r.get(0),
        )
        .optional()?;
    let current = match stored {
        Some(v) => v,
        None => {
            let max: Option<i64> =
                conn.query_row(&format!("SELECT max(id) FROM {table}"), [], |r| r.get(0))?;
            max.unwrap_or(0)
        }
    };
    let next = current + 1;
    conn.execute(
        "INSERT OR REPLACE INTO sequences (name, value) VALUES (?1, ?2)",
        params![table, next],
    )?;
    Ok(next)
}

/// Reserve `count` consecutive ids for `table` and return the first; zero
/// reserves nothing and returns the id the next call would hand out.
pub fn next_ids(conn: &Connection, table: &str, count: usize) -> anyhow::Result<i64> {
    if count == 0 {
        let first = next_id(conn, table)?;
        conn.execute(
            "UPDATE sequences SET value = value - 1 WHERE name = ?1",
            params![table],
        )?;
        return Ok(first);
    }
    let first = next_id(conn, table)?;
    if count > 1 {
        conn.execute(
            "UPDATE sequences SET value = value + ?2 WHERE name = ?1",
            params![table, (count - 1) as i64],
        )?;
    }
    Ok(first)
}

/// Seed a counter (from the original's `sqlite_sequence`), never lowering
/// it below the rows already there or the ids already handed out.
pub fn seed(conn: &Connection, table: &str, value: i64) -> anyhow::Result<()> {
    let max: Option<i64> = conn.query_row(&format!("SELECT max(id) FROM {table}"), [], |r| r.get(0))?;
    let stored: Option<i64> = conn
        .query_row(
            "SELECT value FROM sequences WHERE name = ?1",
            params![table],
            |r| r.get(0),
        )
        .optional()?;
    let value = value.max(max.unwrap_or(0)).max(stored.unwrap_or(0));
    conn.execute(
        "INSERT OR REPLACE INTO sequences (name, value) VALUES (?1, ?2)",
        params![table, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keep_counting_past_deleted_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut conn).unwrap();
        conn.execute("CREATE TABLE things (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        conn.execute("INSERT INTO things (id) VALUES (5)", []).unwrap();
        assert_eq!(next_id(&conn, "things").unwrap(), 6);
        conn.execute("DELETE FROM things", []).unwrap();
        assert_eq!(next_id(&conn, "things").unwrap(), 7);
        seed(&conn, "things", 3).unwrap();
        assert_eq!(
            next_id(&conn, "things").unwrap(),
            8,
            "a seed never lowers the counter"
        );
        seed(&conn, "things", 20).unwrap();
        assert_eq!(next_id(&conn, "things").unwrap(), 21);
        assert_eq!(next_ids(&conn, "things", 3).unwrap(), 22);
        assert_eq!(next_id(&conn, "things").unwrap(), 25);
    }
}
