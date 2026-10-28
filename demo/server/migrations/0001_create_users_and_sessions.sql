-- 0001_create_users_and_sessions.sql
-- 目标：SQLite（demo 默认）。运行时由 server.rp 的 setup_schema 按 db.driver 生成方言版本，
-- 本文件用于记录结构，迁移到 MySQL / PostgreSQL / SQL Server 时按方言改写自增列与文本类型。
--
-- SQLite   : id INTEGER PRIMARY KEY AUTOINCREMENT, 文本 TEXT
-- MySQL    : id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY, 文本 VARCHAR(255)
-- PostgreSQL: id BIGSERIAL PRIMARY KEY, 文本 TEXT
-- SQL Server: id BIGINT IDENTITY(1,1) PRIMARY KEY, 文本 NVARCHAR(255)

CREATE TABLE IF NOT EXISTS users (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL,
    email      TEXT NOT NULL UNIQUE,
    phone      TEXT NOT NULL DEFAULT '',
    password   TEXT NOT NULL,
    role       TEXT NOT NULL DEFAULT 'user',
    status     TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    token      TEXT NOT NULL UNIQUE,
    user_id    BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    created_at TEXT NOT NULL
);
