use rusqlite::Connection;

pub fn leaked_sqlite(_: Connection) {}
