package api

import (
	"encoding/json"
	"net/http"

	"github.com/akinmoladun/edgesync/server/internal/db"
	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
)

// Server holds dependencies for HTTP handlers.
type Server struct {
	DB     *db.DB
	Router chi.Router
}

// NewServer initializes Chi router and registers endpoints.
func NewServer(database *db.DB) *Server {
	r := chi.NewRouter()

	r.Use(middleware.RequestID)
	r.Use(middleware.RealIP)
	r.Use(middleware.Logger)
	r.Use(middleware.Recoverer)

	s := &Server{
		DB:     database,
		Router: r,
	}

	r.Get("/healthz", s.handleHealthz)
	r.Post("/sync/push", s.handlePush)
	r.Get("/sync/pull", s.handlePull)
	r.Get("/conflicts", s.handleListConflicts)
	r.Post("/conflicts/{id}/resolve", s.handleResolveConflict)

	return s
}

func (s *Server) handleHealthz(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	if err := json.NewEncoder(w).Encode(map[string]string{"status": "ok"}); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}
