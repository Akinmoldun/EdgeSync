package db

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/jackc/pgx/v5/pgxpool"
)

// DB wraps the pgxpool connection pool.
type DB struct {
	Pool *pgxpool.Pool
}

// Config holds connection parameters.
type Config struct {
	Host     string
	Port     int
	User     string
	Password string
	DBName   string
	SSLMode  string
}

// ConnString returns the PostgreSQL DSN.
func (c *Config) ConnString() string {
	if c.SSLMode == "" {
		c.SSLMode = "disable"
	}
	return fmt.Sprintf("postgres://%s:%s@%s:%d/%s?sslmode=%s",
		c.User, c.Password, c.Host, c.Port, c.DBName, c.SSLMode)
}

// New creates a new DB wrapper and establishes the connection pool.
func New(ctx context.Context, cfg Config) (*DB, error) {
	return NewFromDSN(ctx, cfg.ConnString())
}

// NewFromDSN creates a new DB wrapper using a raw connection string (e.g. DATABASE_URL).
func NewFromDSN(ctx context.Context, dsn string) (*DB, error) {
	connConfig, err := pgxpool.ParseConfig(dsn)
	if err != nil {
		return nil, fmt.Errorf("parsing connection config: %w", err)
	}

	pool, err := pgxpool.NewWithConfig(ctx, connConfig)
	if err != nil {
		return nil, fmt.Errorf("creating pool: %w", err)
	}

	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return nil, fmt.Errorf("pinging postgres: %w", err)
	}

	return &DB{Pool: pool}, nil
}

// Close releases all pool connections.
func (d *DB) Close() {
	if d.Pool != nil {
		d.Pool.Close()
	}
}

// RunMigrations executes SQL migration files from the migrations directory.
func (d *DB) RunMigrations(ctx context.Context, migrationsDir string) error {
	entries, err := os.ReadDir(migrationsDir)
	if err != nil {
		return fmt.Errorf("reading migrations dir: %w", err)
	}

	for _, entry := range entries {
		if !entry.IsDir() && strings.HasSuffix(entry.Name(), ".up.sql") {
			filePath := filepath.Join(migrationsDir, entry.Name())
			content, err := os.ReadFile(filePath)
			if err != nil {
				return fmt.Errorf("reading migration %s: %w", entry.Name(), err)
			}

			if _, err := d.Pool.Exec(ctx, string(content)); err != nil {
				return fmt.Errorf("executing migration %s: %w", entry.Name(), err)
			}
		}
	}

	return nil
}
