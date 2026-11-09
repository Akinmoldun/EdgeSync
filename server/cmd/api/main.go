package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	"github.com/akinmoladun/edgesync/server/internal/api"
	"github.com/akinmoladun/edgesync/server/internal/db"
)

func main() {
	log.Println("Starting EdgeSync Sync Server...")

	port := 8080
	if envPort := os.Getenv("PORT"); envPort != "" {
		if p, err := strconv.Atoi(envPort); err == nil {
			port = p
		}
	}

	var database *db.DB
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)

	if dsn := os.Getenv("DATABASE_URL"); dsn != "" {
		log.Println("Connecting to PostgreSQL via DATABASE_URL...")
		var err error
		database, err = db.NewFromDSN(ctx, dsn)
		cancel()
		if err != nil {
			log.Fatalf("Database connection failed via DATABASE_URL: %v", err)
		}
	} else {
		dbHost := os.Getenv("DB_HOST")
		if dbHost == "" {
			dbHost = "localhost"
		}
		dbPort := 5432
		if envDBPort := os.Getenv("DB_PORT"); envDBPort != "" {
			if p, err := strconv.Atoi(envDBPort); err == nil {
				dbPort = p
			}
		}
		dbUser := os.Getenv("DB_USER")
		if dbUser == "" {
			dbUser = "postgres"
		}
		dbPass := os.Getenv("DB_PASSWORD")
		if dbPass == "" {
			dbPass = "postgrespassword"
		}
		dbName := os.Getenv("DB_NAME")
		if dbName == "" {
			dbName = "edgesync"
		}

		cfg := db.Config{
			Host:     dbHost,
			Port:     dbPort,
			User:     dbUser,
			Password: dbPass,
			DBName:   dbName,
			SSLMode:  "disable",
		}

		var err error
		database, err = db.New(ctx, cfg)
		cancel()
		if err != nil {
			log.Fatalf("Database connection failed: %v", err)
		}
	}
	defer database.Close()

	// Apply migrations on startup
	migrationsDir := os.Getenv("MIGRATIONS_DIR")
	if migrationsDir == "" {
		migrationsDir = "migrations"
	}
	migCtx, migCancel := context.WithTimeout(context.Background(), 15*time.Second)
	err := database.RunMigrations(migCtx, migrationsDir)
	migCancel()
	if err != nil {
		log.Fatalf("Failed to run migrations: %v", err)
	}
	log.Println("Database migrations applied successfully.")

	srv := api.NewServer(database)

	// TLS / mTLS Configuration (§8 Week 10 & Week 11)
	// Check TLS_CERT / TLS_KEY (with SERVER_CERT_FILE / SERVER_KEY_FILE fallback)
	serverCertFile := os.Getenv("TLS_CERT")
	if serverCertFile == "" {
		serverCertFile = os.Getenv("SERVER_CERT_FILE")
	}
	serverKeyFile := os.Getenv("TLS_KEY")
	if serverKeyFile == "" {
		serverKeyFile = os.Getenv("SERVER_KEY_FILE")
	}
	caCertFile := os.Getenv("TLS_CA")
	if caCertFile == "" {
		caCertFile = os.Getenv("CA_CERT_FILE")
	}

	var tlsConfig *tls.Config
	if serverCertFile != "" && serverKeyFile != "" {
		cert, err := tls.LoadX509KeyPair(serverCertFile, serverKeyFile)
		if err != nil {
			log.Fatalf("Failed to load server certificate and key: %v", err)
		}

		tlsConfig = &tls.Config{
			Certificates: []tls.Certificate{cert},
			MinVersion:   tls.VersionTLS13,
		}

		if caCertFile != "" {
			caBytes, err := os.ReadFile(caCertFile)
			if err != nil {
				log.Fatalf("Failed to read CA certificate: %v", err)
			}
			caPool := x509.NewCertPool()
			if !caPool.AppendCertsFromPEM(caBytes) {
				log.Fatalf("Failed to parse CA certificate into pool")
			}
			tlsConfig.ClientCAs = caPool
			// Strict mTLS enforcement: reject connections without verified client certs (Fail Closed)
			tlsConfig.ClientAuth = tls.RequireAndVerifyClientCert
			log.Println("mTLS enabled: client certificates required and verified against trusted CA.")
		} else {
			log.Println("HTTPS enabled: standard TLS (no client cert requirement).")
		}
	} else {
		log.Println("TLS certificates not provided (TLS_CERT / TLS_KEY unset); gracefully falling back to standard HTTP for public demo deployment.")
	}

	httpServer := &http.Server{
		Addr:         fmt.Sprintf(":%d", port),
		Handler:      srv.Router,
		ReadTimeout:  60 * time.Second,
		WriteTimeout: 60 * time.Second,
		TLSConfig:    tlsConfig,
	}

	go func() {
		if tlsConfig != nil {
			log.Printf("Server listening on HTTPS (mTLS enforced) on port %d", port)
			if err := httpServer.ListenAndServeTLS("", ""); err != nil && err != http.ErrServerClosed {
				log.Fatalf("HTTPS server error: %v", err)
			}
		} else {
			log.Printf("Server listening on port %d", port)
			if err := httpServer.ListenAndServe(); err != nil && err != http.ErrServerClosed {
				log.Fatalf("HTTP server error: %v", err)
			}
		}
	}()

	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	<-stop

	log.Println("Shutting down server gracefully...")
	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer shutdownCancel()
	if err := httpServer.Shutdown(shutdownCtx); err != nil {
		log.Fatalf("Server shutdown failed: %v", err)
	}
	log.Println("Server stopped.")
}
