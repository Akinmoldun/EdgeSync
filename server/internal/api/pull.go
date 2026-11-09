package api

import (
	"encoding/json"
	"net/http"
	"strconv"
)

func (s *Server) handlePull(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")

	afterSeqStr := r.URL.Query().Get("after_seq")
	var afterSeq int64 = 0
	if afterSeqStr != "" {
		parsed, err := strconv.ParseInt(afterSeqStr, 10, 64)
		if err != nil || parsed < 0 {
			w.WriteHeader(http.StatusBadRequest)
			_ = json.NewEncoder(w).Encode(map[string]string{"error": "invalid after_seq parameter: must be a non-negative integer"})
			return
		}
		afterSeq = parsed
	}

	events, err := s.DB.GetEventsAfterSeq(r.Context(), afterSeq, 500)
	if err != nil {
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "failed to retrieve events: " + err.Error()})
		return
	}

	w.WriteHeader(http.StatusOK)
	if err := json.NewEncoder(w).Encode(events); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}
